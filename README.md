# meshtastic-lora-rs

A Meshtastic-compatible mesh networking stack built in pure Rust on top of
[lora-rs](lora-rs/), a complete LoRa PHY library.

`lora-rs` lives here as a git submodule and is extended only to expose a clean
library API. All layers above the PHY — packet framing, encryption, mesh
routing, duty-cycle enforcement, protobuf types, and the application interface
— live in the `mesh` crate in this workspace.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  meshtastic-lora-rs  (this repo)                               │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  app      — MeshNode API, ChannelConfig, MeshMessage    │   │
│  ├─────────────────────────────────────────────────────────┤   │
│  │  mesh     — flood router, dedup cache, hop-limit logic  │   │
│  │             node identity, neighbour table               │   │
│  ├─────────────────────────────────────────────────────────┤   │
│  │  mac      — OTA framing, AES-256-CTR, duty-cycle        │   │
│  ├─────────────────────────────────────────────────────────┤   │
│  │  proto    — Data, User, PortNum (prost types)           │   │
│  └──────────────────────────┬──────────────────────────────┘   │
│                              │ lora::modem  lora::channel       │
└──────────────────────────────┼──────────────────────────────────┘
                               │
┌──────────────────────────────▼──────────────────────────────────┐
│  lora-rs  (submodule — PHY + library API)                      │
│  lora::modem   — Tx / Rx byte↔IQ wrappers                      │
│  lora::channel — Driver trait + Channel (AWGN sim)             │
│  lora::tx / rx — DSP pipeline (whiten/Hamming/chirp/FFT/…)     │
│  lora::ui      — spectrum / waterfall egui widgets             │
│  bin/gui_sim   — standalone LoRa PHY simulator GUI             │
└─────────────────────────────────────────────────────────────────┘
```

---

## Workspace layout

```
meshtastic-lora-rs/
├── lora-rs/                 — PHY submodule (lora crate)
│   └── src/
│       ├── modem.rs         — Tx, Rx, DecodeResult  ← new lib API
│       ├── channel.rs       — Driver trait, Channel  ← new lib API
│       ├── tx/              — DSP encode pipeline
│       ├── rx/              — DSP decode pipeline
│       ├── ui/              — egui spectrum/waterfall widgets
│       └── bin/gui_sim/     — standalone LoRa GUI simulator
├── mesh/                    — mesh networking crate
│   └── src/
│       ├── lib.rs
│       ├── mac/
│       │   ├── packet.rs    — MeshHeader / MeshFrame OTA framing
│       │   ├── crypto.rs    — AES-256-CTR encrypt / decrypt
│       │   └── duty_cycle.rs— airtime budget tracker
│       ├── mesh/
│       │   ├── router.rs    — stateless flood router + DedupCache
│       │   └── node.rs      — LocalNode, NodeInfo, NeighbourTable
│       ├── proto/
│       │   ├── mod.rs       — Data, User, PortNum (hand-written prost)
│       │   ├── radio.rs     — MeshPacket, FromRadio, ToRadio, MyNodeInfo
│       │   └── service_envelope.rs — ServiceEnvelope (MQTT payload)
│       ├── presets.rs       — ModemPreset + all 9 Meshtastic presets
│       ├── app.rs           — MeshNode public API
│       ├── serial.rs        — serial framing (magic + length-prefix)
│       ├── mqtt.rs          — MQTT bridge (rumqttc, ServiceEnvelope)
│       └── bin/
│           ├── mesh_sim.rs  — two-node egui simulation binary
│           └── mesh_node.rs — headless node (stdin/stdout + UHD)
└── Cargo.toml               — workspace root
```

---

## Running the simulation

```sh
# Two-node mesh sim with egui GUI (default binary)
cargo run

# Headless node — type messages, see received packets
cargo run --bin mesh_node

# Headless node with real RF (USRP)
cargo run --bin mesh_node -- --uhd --freq 906.875

# Original LoRa PHY GUI simulator
cargo run --bin gui_sim
```

The `mesh_sim` GUI shows:
- **Left panel** — preset selector (all 9 Meshtastic presets), SF slider,
  signal / noise / SNR controls, TX interval, pause/resume, packet counters,
  per-node neighbour tables
- **Right panel** — scrolling message log with colour-coded TX / RX / error entries

---

## Key design decisions

**`lora-rs` is used as a library, not forked.**  All DSP primitives (`modulate`,
`frame_sync`, etc.) are consumed through `lora::modem::Tx` / `Rx`.  The only
changes to the submodule are additive: the two new public modules.

**Protobuf types are hand-written with `prost` derives** rather than generated
from `.proto` files.  This avoids a `prost-build` + submodule dependency for
the two message types currently needed (`Data`, `User`).  Migrating to
generated code is a drop-in replacement when the full proto corpus is needed.

**`MeshNode::process_rx_frame` is synchronous and stateless at the PHY
boundary.**  The caller owns the IQ pipeline and calls `process_rx_frame` with
raw decoded bytes; the node returns `(Option<MeshMessage>, Option<MeshFrame>)`
(deliver, forward).  This keeps the mesh layer testable without a running
tokio runtime.

---

## Data I/O integration roadmap

The mesh stack is designed to support multiple external data interfaces.
Each step builds on the same `MeshNode` API (`build_text_frame`,
`process_rx_frame`) and the `lora::channel::Driver` trait (Sim / UHD).

| Step | Interface | What it enables |
|------|-----------|-----------------|
| **A** | `mesh_node` headless binary (stdin/stdout + UHD) | Real RF testing against off-the-shelf Meshtastic radios |
| **B** | `FromRadio` / `ToRadio` serial protobuf API | Full Meshtastic toolchain: Python CLI, web client, mobile apps via USB |
| **C** | MQTT bridge (`msh/+/+/#`) | Internet ↔ mesh bridging, compatible with `mqtt.meshtastic.org` |
| **D** | WebSocket API for the WASM GUI | Browser-based sim with external message injection |
| **E** | BLE GATT service (`6ba1b218-…`) | Direct Android / iOS Meshtastic app connection |

### A — Headless node (implemented)

Single-node binary: reads lines from stdin, transmits as `TEXT_MESSAGE_APP`,
prints received messages to stdout.  Supports both simulated channel and UHD
for real RF.

```sh
# Simulated (loopback — useful for protocol testing)
cargo run --bin mesh_node

# Real RF via USRP
cargo run --bin mesh_node -- --uhd --freq 906.875
```

### B — Serial protobuf API (implemented)

The `--serial` flag on `mesh_node` enables the Meshtastic serial framing
protocol (`[0x94 0xC3] [len_u16_be] [protobuf]`) on stdin/stdout.  The
node speaks `FromRadio` / `ToRadio` and responds to config handshakes
(`want_config_id` → `my_info` + `node_info` + `config_complete_id`).

```sh
cargo run --bin mesh_node -- --serial
```

Hand-written prost types cover `MeshPacket`, `FromRadio`, `ToRadio`,
`MyNodeInfo`, `NodeInfoProto`.  For full Meshtastic toolchain compatibility,
swap to prost-build with the official
[meshtastic/protobufs](https://github.com/meshtastic/protobufs) as a
submodule.

### C — MQTT bridge (implemented)

The `--mqtt` flag on `mesh_node` connects to a Meshtastic MQTT broker and
bridges packets between the local RF channel and the internet.

```sh
# Default broker (mqtt.meshtastic.org)
cargo run --bin mesh_node -- --mqtt

# Custom broker
cargo run --bin mesh_node -- --mqtt --mqtt-host broker.local --mqtt-port 1883
```

Subscribes to `msh/2/c/LongFast/+`, publishes to
`msh/2/c/LongFast/%21{gateway_id}`.  Packets arrive as `ServiceEnvelope`
protobufs and are fed to `MeshNode::process_rx_frame`.  Local RF RX and
stdin text are also bridged to MQTT.  Uses `rumqttc` (async, tokio).

### D — WebSocket (WASM)

Expose a WebSocket endpoint from the browser-based mesh_sim for external
tools (Node.js scripts, Home Assistant, dashboards) to send/receive
messages as JSON.

### E — BLE GATT

Advertise the Meshtastic BLE GATT service so Android/iOS apps can connect
directly.  Platform-specific (`btleplug` / `bluer`); defer until the core
protocol is stable.

---

## Modem config presets

| Preset        | SF | BW kHz | CR  | Sync | Preamble |
|---------------|----|--------|-----|------|----------|
| ShortTurbo    | 7  | 500    | 4/5 | 0x2B | 16 |
| ShortFast     | 7  | 250    | 4/5 | 0x2B | 16 |
| ShortSlow     | 8  | 250    | 4/5 | 0x2B | 16 |
| MediumFast    | 9  | 250    | 4/5 | 0x2B | 16 |
| MediumSlow    | 10 | 250    | 4/5 | 0x2B | 16 |
| **LongFast**  | 11 | 250    | 4/5 | 0x2B | 16 | ← Meshtastic default
| LongModerate  | 11 | 125    | 4/8 | 0x2B | 16 |
| LongSlow      | 12 | 125    | 4/8 | 0x2B | 16 |
| VeryLongSlow  | 12 | 62.5   | 4/8 | 0x2B | 16 |

---

## Dependencies

| Crate    | Purpose                        |
|----------|--------------------------------|
| `aes`    | AES-256 block cipher           |
| `ctr`    | CTR mode wrapper               |
| `prost`  | Protobuf encode / decode       |
| `rand`   | Random node ID generation      |
| `tokio`  | Async runtime (native feature) |
| `egui`   | Immediate-mode GUI             |
| `eframe` | Native / wasm app framework    |
| `rumqttc`| Async MQTT client (mqtt feat)  |
| `lora`   | PHY TX / RX pipeline (submod)  |

No C dependencies in the `mesh` crate (UHD links libuhd via lora).

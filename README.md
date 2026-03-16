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
│  │  proto    — Data, User, PortNum, MeshPacket, FromRadio  │   │
│  │             ToRadio, ServiceEnvelope (prost types)       │   │
│  └──────────────────────────┬──────────────────────────────┘   │
│  serial — Meshtastic serial framing protocol                   │
│  mqtt   — MQTT bridge (rumqttc, ServiceEnvelope)               │
│  ws     — WebSocket server (JSON commands/events)              │
│                              │ lora::modem  lora::channel       │
└──────────────────────────────┼──────────────────────────────────┘
                               │
┌──────────────────────────────▼──────────────────────────────────┐
│  lora-rs  (submodule — PHY + library API)                      │
│  lora::modem   — Tx / Rx byte↔IQ wrappers                      │
│  lora::channel — Driver trait + Channel (AWGN sim)             │
│  lora::uhd     — UhdDevice (USRP hardware driver)              │
│  lora::tx / rx — DSP pipeline (whiten/Hamming/chirp/FFT/…)     │
│  lora::ui      — spectrum / waterfall / SpectrumAnalyzer       │
│  bin/gui_sim   — standalone LoRa PHY simulator GUI             │
└─────────────────────────────────────────────────────────────────┘
```

---

## Workspace layout

```
meshtastic-lora-rs/
├── lora-rs/                 — PHY submodule (lora crate)
│   └── src/
│       ├── modem.rs         — Tx, Rx, DecodeResult
│       ├── channel.rs       — Driver trait, Channel (AWGN sim)
│       ├── uhd.rs           — UhdDevice (USRP, feature-gated)
│       ├── tx/              — DSP encode pipeline
│       ├── rx/              — DSP decode pipeline
│       ├── ui/              — spectrum, waterfall, SpectrumAnalyzer
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
│       │   ├── mod.rs       — re-exports + helper impls
│       │   └── generated.rs — prost-build output (from protobufs/ submodule)
│       ├── presets.rs       — ModemPreset + all 9 Meshtastic presets
│       ├── app.rs           — MeshNode public API
│       ├── serial.rs        — serial framing (magic + length-prefix)
│       ├── mqtt.rs          — MQTT bridge (rumqttc, ServiceEnvelope)
│       ├── ws.rs            — WebSocket server (tokio-tungstenite, JSON)
│       └── bin/
│           ├── mesh_sim.rs  — two-node egui simulation (spectrum + waterfall)
│           └── mesh_node.rs — headless node (text / serial / MQTT)
├── protobufs/               — meshtastic/protobufs submodule (.proto files)
├── web/                     — WASM build assets
│   └── index.html
├── Makefile
└── Cargo.toml               — workspace root
```

---

## Usage

### GUI simulation

```sh
# Two-node mesh sim with spectrum + waterfall (default binary)
cargo run

# Build WASM version
make wasm-serve   # serves on http://localhost:3000
```

The `mesh_sim` GUI shows a left settings panel (preset selector, SF, TX/RX
gain, driver selection Sim/UHD, node info) and a central panel with live
spectrum, waterfall, and a scrolling mesh message log.

### Headless node

```sh
# Text mode — type messages, see received packets
cargo run --bin mesh_node

# Serial protobuf mode — Meshtastic framing on stdin/stdout
cargo run --bin mesh_node -- --serial

# MQTT bridge — connect to mqtt.meshtastic.org
cargo run --bin mesh_node -- --mqtt

# MQTT with custom broker
cargo run --bin mesh_node -- --mqtt --mqtt-host broker.local

# Real RF via USRP
cargo run --bin mesh_node -- --uhd --freq 906.875

# WebSocket server — external tools connect via ws://localhost:9001
cargo run --bin mesh_node -- --ws

# Combine modes: MQTT + UHD + WebSocket (four-way bridge)
cargo run --bin mesh_node -- --mqtt --uhd --freq 906.875 --ws
```

**Text mode** (default) reads lines from stdin, transmits as
`TEXT_MESSAGE_APP` broadcasts, and prints received messages to stdout.

**Serial mode** (`--serial`) speaks the Meshtastic serial framing protocol
(`[0x94 0xC3] [len_u16_be] [protobuf]`) using `FromRadio` / `ToRadio`
messages.  Responds to config handshakes (`want_config_id` →
`my_info` + `node_info` + `config_complete_id`).

**MQTT mode** (`--mqtt`) connects to a Meshtastic MQTT broker, subscribes to
`msh/2/c/LongFast/+`, and bridges packets between the local RF channel and
the internet as `ServiceEnvelope` protobufs.

**WebSocket** (`--ws`) starts a WebSocket server on port 9001 (or
`--ws-port N`).  External tools send JSON commands and receive JSON events:

```jsonc
// → send to node
{ "type": "send_text", "to": 4294967295, "text": "hello" }

// ← received from node
{ "type": "rx", "from": 2864434397, "to": 4294967295,
  "portnum": 1, "text": "hello", "payload_len": 5, "hops": 2 }
```

`--ws` is combinable with any mode (text, serial, mqtt) — the WebSocket
server runs alongside as an additional I/O channel.

### PHY simulator

```sh
# Original LoRa PHY GUI simulator (from lora-rs submodule)
cargo run --bin gui_sim
```

### Examples & guides

See [docs/examples.md](docs/examples.md) for detailed walkthroughs:
- What is Meshtastic / LoRa (background for newcomers)
- Exploring the GUI simulator
- Talking to a real Meshtastic radio over MQTT
- Building a Home Assistant alert bridge
- Monitoring a mesh network from a web dashboard
- Setting up a USRP SDR gateway

---

## Key design decisions

**`lora-rs` is used as a library, not forked.**  All DSP primitives
(`modulate`, `frame_sync`, etc.) are consumed through `lora::modem::Tx` /
`Rx`.  The only changes to the submodule are additive public modules
(`modem`, `channel`, `uhd`, `ui::SpectrumAnalyzer`).

**Protobuf types are generated from the official
[meshtastic/protobufs](https://github.com/meshtastic/protobufs)** via
`prost-build`.  The proto repo is a git submodule under `protobufs/`.
Commonly used types (`Data`, `User`, `MeshPacket`, `FromRadio`, `ToRadio`,
`ServiceEnvelope`, `PortNum`) are re-exported from `proto::` with helper
impls (`encode_to_data`, `decode_user`, `text`).  The full generated
corpus is available under `proto::generated::meshtastic`.

**`MeshNode::process_rx_frame` is synchronous and stateless at the PHY
boundary.**  The caller owns the IQ pipeline and calls `process_rx_frame`
with raw decoded bytes; the node returns
`(Option<MeshMessage>, Option<MeshFrame>)` (deliver, forward).  This keeps
the mesh layer testable without a running async runtime.

**Three I/O modes in one binary.**  `mesh_node` supports text, serial
protobuf, and MQTT in the same binary, selectable at runtime.  All three
share the same PHY tick loop and `MeshNode` instance.

---

## Roadmap

### mesh_sim → RF communications terminal

| # | Change | Status |
|---|--------|--------|
| 1 | Auto-TX toggle + manual TX queue + text input / Send button | done |
| 2 | Operating mode: TwoNodeTest (current) vs Terminal (single-node, real RF) | done |
| 3 | Enhanced message log — timestamps, direction (TX/RX/FWD), from-ID, hops, colour | done |
| 4 | Forward handling — re-modulate + relay `Option<MeshFrame>` from `process_rx_frame` | done |
| 5 | Node identity config — short/long name fields in Terminal mode | done |
| 6 | PER enhancements — colour-coded label | done |
| 7 | Destination selector — broadcast or unicast to neighbour | done |

### Other

| Interface | What it enables |
|-----------|-----------------|
| BLE GATT service (`6ba1b218-…`) | Direct Android / iOS Meshtastic app connection |

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
| `prost-build` | Proto code generation (build-dep) |
| `rand`   | Random node ID generation      |
| `tokio`  | Async runtime (native feature) |
| `egui`   | Immediate-mode GUI             |
| `eframe` | Native / wasm app framework    |
| `rumqttc`| Async MQTT client (mqtt feat)  |
| `tokio-tungstenite` | WebSocket server (ws feat) |
| `serde`  | JSON serialization (ws feat)   |
| `rustfft`| FFT for spectrum analyzer      |
| `lora`   | PHY TX / RX pipeline (submod)  |

No C dependencies in the `mesh` crate (UHD links libuhd via lora).

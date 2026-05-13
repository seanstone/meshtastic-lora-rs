# meshtastic-lora-rs

A Meshtastic-compatible mesh networking stack built in pure Rust on top of
[lora-rs](lora-rs/), a complete LoRa PHY library.

`lora-rs` lives here as a git submodule and is extended only to expose a clean
library API. All layers above the PHY — packet framing, encryption, mesh
routing, duty-cycle enforcement, protobuf types, and the application interface
— live in the `mesh` crate in this workspace.

---

## Usage

The single `mesh` binary runs the radio loop, exposes an HTTP + WebSocket
server, and (optionally) opens a desktop egui window. The same `view` module
is reused by a WebAssembly binary (`mesh_web`) that the server hosts as a
browser GUI.

```sh
# Build the wasm GUI bundle into dist/ (once, or after view changes)
make wasm-web

# Run mesh — opens a desktop window if a display is available,
# and serves http://0.0.0.0:3000 (HTTP + ws:///ws) for browser clients.
cargo run --bin mesh
# or:
make run
```

Headless / pure-server:

```sh
# Skip the egui window even if compiled in.
cargo run --bin mesh -- --headless

# Build without the eframe-based desktop GUI at all (smaller binary):
cargo run --bin mesh --no-default-features --features server,uhd
```

The web GUI at `http://<host>:3000/` opens a WebSocket back to `/ws` and
mirrors the server's `ViewModel` into a local copy that the same egui
rendering code paints — desktop and browser are the same view sitting on
top of two different transports.

### PHY simulator

```sh
# Standalone LoRa PHY GUI simulator (from the lora-rs submodule)
cargo run --bin gui_sim
```

### Wire protocol

External tools can talk to `/ws` directly with adjacently-tagged JSON.
Outgoing commands use the [`Command`](mesh/src/model.rs) enum
(`{"t":"SetSf","c":7}`, `{"t":"SendText","c":"hi"}`, etc.); the server
pushes [`ServerMsg`](mesh/src/proto_ws.rs) frames — primarily `Snapshot`s
of the radio state at ~10 Hz.

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
│  model     — ViewModel, Command (radio's sole writer)          │
│  radio     — sim_loop: drives PHY, drains Commands             │
│  view      — egui rendering (cfg desktop or wasm)              │
│  server    — axum HTTP + WS /ws, serves dist/ for the web GUI  │
│  proto_ws  — Snapshot / ServerMsg wire types                   │
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
│       ├── mac/             — packet framing, AES, duty-cycle
│       ├── mesh/            — flood router, node identity, neighbours
│       ├── proto/           — prost types + helper impls
│       ├── presets.rs       — ModemPreset + Meshtastic preset table
│       ├── app.rs           — MeshNode public API
│       ├── model.rs         — shared ViewModel + Command enum
│       ├── proto_ws.rs      — WS wire types (ServerMsg / Snapshot)
│       ├── radio.rs         — sim_loop (drives PHY, drains Commands)
│       ├── view/            — egui rendering (cfg desktop or wasm)
│       ├── server.rs        — axum HTTP + WS server (cfg server)
│       └── bin/
│           ├── mesh.rs      — combined server + optional desktop window
│           └── mesh_web.rs  — wasm GUI, WS-backed
├── protobufs/               — meshtastic/protobufs submodule (.proto files)
├── web/                     — WASM HTML shells
│   └── index_web.html
├── Makefile
└── Cargo.toml               — workspace root
```

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

**One binary, dual GUI.**  `mesh` runs the radio + HTTP/WS server in a
background tokio runtime; the desktop egui window is a compile-time
opt-in (`desktop` feature) and the wasm `mesh_web` binary hosts the
same `view` module in a browser, talking back over `/ws`. The
`Command` enum and `Snapshot` types are the only wire surface between
them.

---

## Roadmap

| Item | Description |
|------|-------------|
| BLE GATT service (`6ba1b218-…`) | Direct Android / iOS Meshtastic app connection |

See [CHANGES.md](CHANGES.md) for completed work.

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

---

## Issues

* DC spike (use IF?)
* A top banner saying "Simulation"
* Set "Auto TX" to off in sim
* Fix zoom scale for web
* Resume from pause in UHD mode
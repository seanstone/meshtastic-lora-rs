# meshtastic-lora-rs

A Meshtastic-compatible mesh networking stack built in pure Rust on top of
[lora-rs](lora-rs/), a complete LoRa PHY library.

`lora-rs` lives here as a git submodule and is extended only to expose a clean
library API. All layers above the PHY — packet framing, encryption, mesh
routing, duty-cycle enforcement, protobuf types, and the application interface
— live in the `mesh` crate in this workspace.

---

## Status

All seven implementation phases are complete and the workspace compiles cleanly.

| Phase | Description | Status |
|-------|-------------|--------|
| 1a | Sync word parameterised + validated in `frame_sync` | ✅ done |
| 1b | BW 62.5 kHz added to GUI sim | ✅ done |
| 1c | Preamble length wired through `SimShared` at runtime | ✅ done |
| 2 | Cargo workspace + `mesh` crate skeleton | ✅ done |
| 3 | MAC layer — OTA framing, AES-256-CTR, duty-cycle tracker | ✅ done |
| 4 | Protobuf types — `Data`, `User`, `PortNum` (hand-written prost) | ✅ done |
| 5 | Mesh layer — flood router, dedup cache, node identity, neighbour table | ✅ done |
| 6 | Application interface — `MeshNode`, `ChannelConfig`, `ProcessError` | ✅ done |
| 7 | Simulation driver — two-node egui GUI sim (`mesh_sim` binary) | ✅ done |

### lora-rs library additions

Two new public modules were added to the `lora` crate to support the mesh layer:

| Module | Contents |
|--------|----------|
| `lora::modem` | `Tx` — encodes bytes → IQ; `Rx` — decodes IQ → bytes; `DecodeResult` |
| `lora::channel` | `Driver` trait (IQ transport abstraction); `Channel` (AWGN sim) |

The `gui_sim` binary now uses these library types rather than its own copies.

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
│       │   └── mod.rs       — Data, User, PortNum (hand-written prost)
│       ├── presets.rs       — ModemPreset + all 9 Meshtastic presets
│       ├── app.rs           — MeshNode public API
│       └── bin/
│           └── mesh_sim.rs  — two-node egui simulation binary
└── Cargo.toml               — workspace root
```

---

## Running the simulation

```sh
# Two-node mesh sim with egui GUI
cargo run --bin mesh_sim

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
| `lora`   | PHY TX / RX pipeline (submod)  |

No C dependencies in the `mesh` crate.

# meshtastic-lora-rs

A Meshtastic-compatible mesh networking stack built in pure Rust on top of
[lora-rs](lora-rs/), a complete LoRa PHY library.

`lora-rs` lives here as a git submodule and is not modified beyond what is
needed to expose a clean library API. All layers above the PHY — packet
framing, encryption, mesh routing, duty-cycle enforcement, and the application
interface — live in this workspace.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  meshtastic-lora-rs  (this repo)                               │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  app  — text messages, position, NodeInfo beaconing     │   │
│  ├─────────────────────────────────────────────────────────┤   │
│  │  mesh — flood routing, dedup cache, hop-limit logic     │   │
│  ├─────────────────────────────────────────────────────────┤   │
│  │  mac  — OTA framing, AES-256-CTR, duty-cycle tracker   │   │
│  └──────────────────────────┬──────────────────────────────┘   │
│                              │ lora::tx / lora::rx API          │
└──────────────────────────────┼──────────────────────────────────┘
                               │
┌──────────────────────────────▼──────────────────────────────────┐
│  lora-rs  (submodule — PHY only)                               │
│  TX: whiten → header → CRC → Hamming → interleave → chirp      │
│  RX: frame_sync → FFT demod → deinterleave → Hamming → dewhiten│
│  UI: spectrum / waterfall egui widgets + GUI simulator          │
└─────────────────────────────────────────────────────────────────┘
```

---

## Workspace layout

```
meshtastic-lora-rs/
├── lora-rs/            — PHY submodule (lora crate)
├── mesh/               — mesh networking crate  ← main work
│   ├── src/
│   │   ├── lib.rs
│   │   ├── mac/
│   │   │   ├── mod.rs
│   │   │   ├── packet.rs       — MeshPacket OTA framing
│   │   │   ├── crypto.rs       — AES-256-CTR encrypt / decrypt
│   │   │   └── duty_cycle.rs   — airtime budget tracker
│   │   ├── mesh/
│   │   │   ├── mod.rs
│   │   │   ├── router.rs       — flood routing + dedup cache
│   │   │   └── node.rs         — node ID, NodeInfo, neighbor table
│   │   ├── proto/
│   │   │   ├── mod.rs
│   │   │   └── meshtastic.rs   — generated or hand-written protobuf types
│   │   ├── presets.rs          — modem config presets (LongFast, etc.)
│   │   └── app.rs              — public send / recv API
│   └── Cargo.toml
├── sim/                — optional: simulation driver + GUI for this stack
│   └── …
└── Cargo.toml          — workspace root
```

---

## Implementation plan

### Phase 1 — PHY preparation (changes to lora-rs submodule)

These are small, targeted changes to expose a cleaner API from `lora-rs`.
Everything else stays untouched.

**1a. Sync word is already parameterised (done)**
`frame_sync` now validates the two sync-word chirps and rejects frames whose
sync word does not match.  The default remains `0x12`; Meshtastic uses `0x2B`.

**1b. BW 62.5 kHz is already supported (done)**
`BW_OPTIONS_KHZ` now includes 62.5 kHz, covering the *VeryLongSlow* preset.

**1c. Expose preamble length as a per-call parameter (minor)**
The preamble length is already a parameter of `modulate()` and `frame_sync()`.
Wire it through the GUI sim's `SimShared` so it can be changed at runtime
(Meshtastic default: 16 upchirps).

---

### Phase 2 — Workspace skeleton

Create `Cargo.toml` at the repo root declaring a workspace with two members:
`lora-rs` (path dep, no default features) and `mesh`.  Add `mesh/Cargo.toml`
with the dependencies listed in Phase 3.

---

### Phase 3 — MAC layer (`mesh/src/mac/`)

#### 3a. OTA packet framing (`packet.rs`)

Meshtastic's over-the-air layout (all fields little-endian):

```
Byte offset  Field           Size  Notes
───────────────────────────────────────────────────────────────────
0            to              4     destination node number
4            from            4     sender node number
8            id              4     packet ID (random u32)
12           flags           1     hop_limit[2:0] | want_ack[3] |
                                   via_mqtt[4] | hop_start[7:5]
13           channel_hash    1     hash of channel name + PSK
14           reserved        2     set to 0x0000
16           payload         ≤237  AES-256-CTR encrypted Data proto
```

Total header: 16 bytes.  Maximum LoRa payload: 253 bytes → max body 237 bytes.

Implement `MeshHeader::encode(&self) -> [u8; 16]` and
`MeshHeader::decode(buf: &[u8]) -> Result<Self>`.

#### 3b. Encryption (`crypto.rs`)

- Algorithm: **AES-256-CTR** (RFC 3686 / SIV-free).
- Key: 256-bit channel PSK.  The default public channel key is the single byte
  `0x01` base64-padded to 32 bytes.
- Nonce (128 bits): `[packet_id: u32 LE, 0x00×4, from_node: u32 LE, 0x00×4]`.
- Crates: `aes` + `ctr` (pure Rust, `no_std`-compatible, wasm-safe).
- API: `MeshCrypto::new(psk: [u8; 32])` → `.encrypt(header, plaintext) -> Vec<u8>`
  / `.decrypt(header, ciphertext) -> Result<Vec<u8>>`.

#### 3c. Duty-cycle tracker (`duty_cycle.rs`)

Track cumulative on-air time in a rolling 3 600-second window.

Time-on-air formula (LoRa datasheet, simplified for integer BW):
```
t_sym   = 2^SF / BW
n_sym   = preamble + 4.25 + ceil((8·PL - 4·SF + 28 + 16·CRC) / (4·SF)) · (CR+4)
t_air   = n_sym · t_sym
```

Expose `DutyCycle::can_send(&self, t_air: Duration) -> bool` and
`DutyCycle::record_tx(&mut self, t_air: Duration)`.
Regional limits: EU868 = 1 %, US915 = no legal cap (Meshtastic self-limits to
~30 s/hr).

---

### Phase 4 — Protobuf types (`mesh/src/proto/`)

Option A (recommended): add `prost` + `prost-build` and generate types from the
official [meshtastic/protobufs](https://github.com/meshtastic/protobufs) `.proto`
files.  Add the proto repo as a second submodule under `proto/`.

Option B: hand-write minimal encode/decode for the two message types needed in
Phase 5 (`Data` with `portnum` + `payload` fields, `User` with `id` + `short_name`
+ `long_name`).  Faster to start but harder to extend.

Key port numbers:
```
1   TEXT_MESSAGE_APP
3   POSITION_APP
4   NODEINFO_APP
67  TELEMETRY_APP
```

---

### Phase 5 — Mesh layer (`mesh/src/mesh/`)

#### 5a. Node identity (`node.rs`)

- Generate a random 32-bit `node_id` at startup (use `rand`).
- Build a `User` protobuf with short name, long name, hardware model.
- Maintain a `HashMap<u32, NodeInfo>` neighbour table, updated on every
  received `NODEINFO_APP` packet.
- Broadcast own `NodeInfo` on startup and every 15 minutes.

#### 5b. Flood router (`router.rs`)

```
on TX:
  1. Build MeshHeader { to, from=node_id, id=rand_u32(),
                        flags: hop_limit=3|hop_start=3, channel_hash }
  2. Encrypt body with MeshCrypto
  3. Enqueue frame → lora::tx pipeline

on RX (decoded LoRa frame):
  1. Decode MeshHeader (16-byte prefix)
  2. Check dedup cache: if (from, id) seen → drop
  3. Insert (from, id) into dedup cache (ring buffer, capacity 50)
  4. Decrypt body; deliver to app layer if `to == node_id || to == 0xFFFFFFFF`
  5. If hop_limit > 0:
       decrement hop_limit in header
       apply random jitter delay (0–5 s, uniform)
       re-encrypt with updated header
       re-enqueue for TX
```

Dedup cache: a fixed-size ring buffer of `(u32, u32)` pairs — no heap
allocation needed.

---

### Phase 6 — Application interface (`mesh/src/app.rs`)

```rust
pub struct MeshNode { … }

impl MeshNode {
    pub fn new(channel: ChannelConfig) -> Self;

    /// Send a UTF-8 text message (TEXT_MESSAGE_APP).
    pub async fn send_text(&self, to: u32, text: &str) -> Result<()>;

    /// Receive the next decoded application-layer payload.
    pub async fn recv(&self) -> MeshMessage;

    /// Current neighbour table snapshot.
    pub fn neighbours(&self) -> Vec<NodeInfo>;
}
```

`MeshNode` owns a `tokio::sync::mpsc` pair wiring the router to the lora-rs
TX/RX workers.  The caller never touches raw IQ samples.

---

### Phase 7 — Simulation driver (`sim/`)

Re-use the existing `gui_sim` binary from `lora-rs` as a starting point.
Replace its `sim_loop` TX/RX payloads with `MeshNode::send_text` /
`MeshNode::recv` calls so the GUI visualises a live mesh exchange.

Extend the egui panels to show:
- Own node ID and channel name
- Neighbour table (node ID, short name, last RSSI if UHD)
- Decoded text messages (replacing the raw sequence-number log)
- Per-packet hop count and want-ack flag

---

## Modem config presets

| Preset        | SF | BW kHz | CR  | Sync |
|---------------|----|--------|-----|------|
| ShortTurbo    | 7  | 500    | 4/5 | 0x2B |
| ShortFast     | 7  | 250    | 4/5 | 0x2B |
| ShortSlow     | 8  | 250    | 4/5 | 0x2B |
| MediumFast    | 9  | 250    | 4/5 | 0x2B |
| MediumSlow    | 10 | 250    | 4/5 | 0x2B |
| **LongFast**  | 11 | 250    | 4/5 | 0x2B | ← default
| LongModerate  | 11 | 125    | 4/8 | 0x2B |
| LongSlow      | 12 | 125    | 4/8 | 0x2B |
| VeryLongSlow  | 12 | 62.5   | 4/8 | 0x2B |

All presets use preamble length 16.

---

## Dependencies (mesh crate)

| Crate        | Purpose                          | `no_std` / wasm |
|--------------|----------------------------------|-----------------|
| `aes`        | AES block cipher                 | yes             |
| `ctr`        | CTR mode wrapper                 | yes             |
| `prost`      | Protobuf encode / decode         | yes             |
| `rand`       | Node ID generation               | yes (wasm)      |
| `tokio`      | Async runtime (native)           | native only     |
| `lora-rs`    | PHY TX / RX pipeline             | yes             |

No C dependencies.  The stack compiles to wasm32-unknown-unknown with the
`wasm` feature (using `gloo-timers` instead of `tokio::time`).

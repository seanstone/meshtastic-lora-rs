/// Meshtastic-compatible mesh networking stack.
///
/// # Layer map
///
/// ```text
/// app        — public send/recv API (MeshNode)
/// mesh       — flood router, dedup cache, hop-limit logic
/// mac        — OTA packet framing, AES-256-CTR, duty-cycle tracker
/// proto      — protobuf types (Data, User, Position, …)
/// presets    — modem config presets (LongFast, ShortFast, …)
/// ```
///
/// The PHY is provided by the `lora` crate (submodule).

pub mod mac;
pub mod mesh;
pub mod proto;
pub mod presets;
pub mod app;
pub mod serial;

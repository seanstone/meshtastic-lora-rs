/// Meshtastic serial-protocol protobuf types.
///
/// These mirror the `FromRadio`, `ToRadio`, `MeshPacket`, `MyNodeInfo`, and
/// `NodeInfo` messages from `meshtastic/mesh.proto`.  Only the fields needed
/// by the serial host API are included.
///
/// Source of truth: <https://buf.build/meshtastic/protobufs/docs/main:meshtastic>

use super::Data;

// ── MeshPacket ───────────────────────────────────────────────────────────────

/// Protobuf representation of an over-the-air mesh packet.
///
/// Used inside `FromRadio` / `ToRadio` — not the same as our internal
/// `MeshFrame` (which is a raw binary struct for the PHY pipeline).
#[derive(Clone, PartialEq, prost::Message)]
pub struct MeshPacket {
    #[prost(uint32, tag = "1")]
    pub from: u32,

    #[prost(uint32, tag = "2")]
    pub to: u32,

    /// Channel index (0-based).
    #[prost(uint32, tag = "3")]
    pub channel: u32,

    #[prost(uint32, tag = "6")]
    pub id: u32,

    /// Unix timestamp when received (0 if unknown).
    #[prost(fixed32, tag = "7")]
    pub rx_time: u32,

    /// SNR of received packet (dB).
    #[prost(float, tag = "8")]
    pub rx_snr: f32,

    #[prost(uint32, tag = "10")]
    pub hop_limit: u32,

    #[prost(bool, tag = "11")]
    pub want_ack: bool,

    /// RSSI of received packet (dBm, typically negative).
    #[prost(int32, tag = "13")]
    pub rx_rssi: i32,

    /// Payload: decoded Data (cleartext) or raw encrypted bytes.
    #[prost(oneof = "mesh_packet::PayloadVariant", tags = "4, 5")]
    pub payload_variant: Option<mesh_packet::PayloadVariant>,
}

pub mod mesh_packet {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum PayloadVariant {
        /// Decoded application-layer payload.
        #[prost(message, tag = "4")]
        Decoded(super::Data),

        /// Raw encrypted bytes (as received OTA).
        #[prost(bytes, tag = "5")]
        Encrypted(Vec<u8>),
    }
}

// ── MyNodeInfo ───────────────────────────────────────────────────────────────

/// Sent once at config time to tell the host our node number.
#[derive(Clone, PartialEq, prost::Message)]
pub struct MyNodeInfo {
    /// This node's numeric ID.
    #[prost(uint32, tag = "1")]
    pub my_node_num: u32,

    /// Maximum number of channels this device supports.
    #[prost(uint32, tag = "7")]
    pub max_channels: u32,

    /// Firmware version string.
    #[prost(string, tag = "8")]
    pub firmware_version: String,
}

// ── NodeInfo ─────────────────────────────────────────────────────────────────

/// Information about a known node (ourselves or a neighbour).
#[derive(Clone, PartialEq, prost::Message)]
pub struct NodeInfoProto {
    /// Node number.
    #[prost(uint32, tag = "1")]
    pub num: u32,

    /// User identity.
    #[prost(message, optional, tag = "2")]
    pub user: Option<super::User>,

    /// Last SNR we heard from this node.
    #[prost(float, tag = "4")]
    pub snr: f32,

    /// Unix timestamp of last reception.
    #[prost(fixed32, tag = "5")]
    pub last_heard: u32,
}

// ── FromRadio ────────────────────────────────────────────────────────────────

/// Message sent from the node to the host over the serial link.
#[derive(Clone, PartialEq, prost::Message)]
pub struct FromRadio {
    /// Sequence number (monotonically increasing).
    #[prost(uint32, tag = "1")]
    pub id: u32,

    #[prost(oneof = "from_radio::PayloadVariant", tags = "2, 3, 4, 8")]
    pub payload_variant: Option<from_radio::PayloadVariant>,
}

pub mod from_radio {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum PayloadVariant {
        /// A received (or echoed TX) mesh packet.
        #[prost(message, tag = "2")]
        Packet(super::MeshPacket),

        /// Our own node info (sent once during config handshake).
        #[prost(message, tag = "3")]
        MyInfo(super::MyNodeInfo),

        /// A known node (sent during config dump).
        #[prost(message, tag = "4")]
        NodeInfo(super::NodeInfoProto),

        /// Signals end of config dump (value = the want_config_id we received).
        #[prost(uint32, tag = "8")]
        ConfigCompleteId(u32),
    }
}

// ── ToRadio ──────────────────────────────────────────────────────────────────

/// Message sent from the host to the node over the serial link.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ToRadio {
    #[prost(oneof = "to_radio::PayloadVariant", tags = "1, 3")]
    pub payload_variant: Option<to_radio::PayloadVariant>,
}

pub mod to_radio {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum PayloadVariant {
        /// Packet to transmit.
        #[prost(message, tag = "1")]
        Packet(super::MeshPacket),

        /// Request a config dump; value is a nonce echoed in config_complete_id.
        #[prost(uint32, tag = "3")]
        WantConfigId(u32),
    }
}

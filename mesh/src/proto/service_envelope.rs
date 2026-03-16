/// ServiceEnvelope — the protobuf wrapper used on MQTT topics.
///
/// Meshtastic nodes publish and subscribe to MQTT topics of the form:
///   `msh/2/c/{channel_name}/{gateway_id}`
///
/// The payload of each MQTT message is a `ServiceEnvelope` containing a
/// `MeshPacket` and the gateway's node ID + channel ID.
///
/// Source: <https://buf.build/meshtastic/protobufs/docs/main:meshtastic#meshtastic.ServiceEnvelope>

use super::radio::MeshPacket;

#[derive(Clone, PartialEq, prost::Message)]
pub struct ServiceEnvelope {
    /// The mesh packet being relayed.
    #[prost(message, optional, tag = "1")]
    pub packet: Option<MeshPacket>,

    /// Channel ID string (e.g. "LongFast").
    #[prost(string, tag = "2")]
    pub channel_id: String,

    /// Gateway node ID (the node bridging to/from MQTT).
    #[prost(uint32, tag = "3")]
    pub gateway_id: u32,
}

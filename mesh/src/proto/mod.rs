/// Hand-written prost types mirroring the Meshtastic protobuf schema.
///
/// Source of truth: <https://github.com/meshtastic/protobufs>
/// Only the subset needed by Phases 5–6 is implemented here.
/// When the full proto corpus is needed, replace with prost-build output.

/// Destination port numbers (meshtastic/mesh.proto `PortNum`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum PortNum {
    UnknownApp      = 0,
    TextMessageApp  = 1,
    RemoteHardwareApp = 2,
    PositionApp     = 3,
    NodeinfoApp     = 4,
    TelemetryApp    = 67,
}

/// Generic application-layer payload (meshtastic/mesh.proto `Data`).
#[derive(Clone, PartialEq, prost::Message)]
pub struct Data {
    /// Which application owns this payload.
    #[prost(enumeration = "PortNum", tag = "1")]
    pub portnum: i32,

    /// Raw bytes — interpretation depends on `portnum`.
    #[prost(bytes = "vec", tag = "2")]
    pub payload: Vec<u8>,

    /// Ask the remote to send a response packet.
    #[prost(bool, tag = "3")]
    pub want_response: bool,

    /// Explicit unicast destination (0 = broadcast).
    #[prost(uint32, tag = "4")]
    pub dest: u32,

    /// Originating node (set when relayed through the app layer).
    #[prost(uint32, tag = "5")]
    pub source: u32,

    /// Packet ID this is a reply to.
    #[prost(uint32, tag = "6")]
    pub request_id: u32,
}

/// Node identity beacon (meshtastic/mesh.proto `User`).
///
/// Sent on `NODEINFO_APP` at startup and every 15 minutes.
#[derive(Clone, PartialEq, prost::Message)]
pub struct User {
    /// Unique node ID string (e.g. `"!aabbccdd"`).
    #[prost(string, tag = "1")]
    pub id: String,

    /// Human-readable long name (≤ 39 chars).
    #[prost(string, tag = "2")]
    pub long_name: String,

    /// Short name shown on the map (≤ 4 chars).
    #[prost(string, tag = "3")]
    pub short_name: String,

    /// MAC address bytes (6 bytes, optional).
    #[prost(bytes = "vec", tag = "4")]
    pub macaddr: Vec<u8>,
}

impl User {
    /// Encode to bytes suitable for wrapping in a [`Data`] payload.
    pub fn encode_to_data(&self) -> Data {
        use prost::Message;
        Data {
            portnum:    PortNum::NodeinfoApp as i32,
            payload:    self.encode_to_vec(),
            ..Default::default()
        }
    }
}

impl Data {
    /// Decode a `User` from a `NODEINFO_APP` payload.
    pub fn decode_user(&self) -> Result<User, prost::DecodeError> {
        use prost::Message;
        User::decode(self.payload.as_slice())
    }

    /// Interpret `TEXT_MESSAGE_APP` payload as a UTF-8 string.
    pub fn text(&self) -> Option<&str> {
        if self.portnum == PortNum::TextMessageApp as i32 {
            std::str::from_utf8(&self.payload).ok()
        } else {
            None
        }
    }
}

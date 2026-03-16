/// `MeshNode` — public application-layer API.
///
/// Owns the crypto, dedup cache, and routing state.  Bridges the
/// application layer to the MAC/mesh layers through async channels.
///
/// # Example (native)
/// ```no_run
/// # #[tokio::main] async fn main() {
/// use mesh::app::{MeshNode, MeshMessage, ChannelConfig};
/// let node = MeshNode::new(ChannelConfig::default());
/// node.send_text(mesh::mac::packet::BROADCAST, "hello mesh").await.unwrap();
/// let msg = node.recv().await;
/// println!("{:?}", msg);
/// # }
/// ```

use prost::Message as _;

use crate::mac::crypto::MeshCrypto;
use crate::mac::packet::{BROADCAST, MAX_PAYLOAD, MeshFrame, MeshHeader};
use crate::mesh::node::{LocalNode, NeighbourTable, NodeInfo};
use crate::mesh::router::{DedupCache, RouteDecision, route};
use crate::proto::{Data, PortNum, User};

// ── Channel configuration ──────────────────────────────────────────────────

/// Identifies a Meshtastic channel (name + PSK).
#[derive(Clone)]
pub struct ChannelConfig {
    pub name: String,
    pub psk:  [u8; 32],
}

impl Default for ChannelConfig {
    /// The built-in public channel ("LongFast" / key `0x01` padded).
    fn default() -> Self {
        Self {
            name: "LongFast".to_owned(),
            psk:  MeshCrypto::public_psk(),
        }
    }
}

impl ChannelConfig {
    /// Compute the single-byte `channel_hash` stored in every frame header.
    ///
    /// Meshtastic hashes the channel name XOR'd with the last byte of the PSK.
    pub fn channel_hash(&self) -> u8 {
        let name_hash: u8 = self.name.bytes().fold(0u8, |a, b| a.wrapping_add(b));
        name_hash ^ self.psk[31]
    }
}

// ── Decoded message delivered to the application ──────────────────────────

/// An application-layer message received from the mesh.
#[derive(Debug, Clone)]
pub struct MeshMessage {
    /// Sender node ID.
    pub from: u32,
    /// Destination node ID (`BROADCAST` if not unicast).
    pub to: u32,
    /// Decoded application payload.
    pub data: Data,
    /// Number of hops remaining when received (informational).
    pub hop_limit: u8,
}

// ── MeshNode ──────────────────────────────────────────────────────────────

/// The top-level mesh node handle.
///
/// Call [`MeshNode::process_rx_frame`] whenever the PHY delivers a decoded
/// LoRa frame (raw bytes).  Call [`MeshNode::send_text`] or
/// [`MeshNode::build_frame`] to obtain frames ready for the PHY TX pipeline.
pub struct MeshNode {
    local:    LocalNode,
    crypto:   MeshCrypto,
    channel:  ChannelConfig,
    dedup:    DedupCache,
    neighbours: NeighbourTable,
}

impl MeshNode {
    /// Create a new node with a random node ID.
    pub fn new(channel: ChannelConfig) -> Self {
        let local  = LocalNode::new("mesh", "Meshtastic-rs node");
        let crypto = MeshCrypto::new(channel.psk);
        Self {
            local,
            crypto,
            channel,
            dedup: DedupCache::new(),
            neighbours: NeighbourTable::default(),
        }
    }

    /// Create a node with explicit identity.
    pub fn with_identity(
        channel:    ChannelConfig,
        short_name: impl Into<String>,
        long_name:  impl Into<String>,
    ) -> Self {
        let local  = LocalNode::new(short_name, long_name);
        let crypto = MeshCrypto::new(channel.psk);
        Self {
            local,
            crypto,
            channel,
            dedup: DedupCache::new(),
            neighbours: NeighbourTable::default(),
        }
    }

    /// This node's 32-bit ID.
    pub fn node_id(&self) -> u32 { self.local.node_id }

    /// Snapshot of the current neighbour table.
    pub fn neighbours(&self) -> Vec<NodeInfo> {
        self.neighbours.all().cloned().collect()
    }

    // ── TX helpers ────────────────────────────────────────────────────────

    /// Encode a `Data` protobuf, encrypt it, and return a [`MeshFrame`] ready
    /// for the LoRa TX pipeline.
    pub fn build_frame(&self, to: u32, data: &Data) -> Option<MeshFrame> {
        let plaintext = data.encode_to_vec();
        if plaintext.len() > MAX_PAYLOAD { return None; }

        let id = rand::random::<u32>();
        let header = MeshHeader {
            to,
            from:         self.local.node_id,
            id,
            hop_limit:    3,
            hop_start:    3,
            want_ack:     false,
            via_mqtt:     false,
            channel_hash: self.channel.channel_hash(),
        };
        let ciphertext = self.crypto.encrypt(id, self.local.node_id, &plaintext);
        Some(MeshFrame { header, payload: ciphertext })
    }

    /// Build a `TEXT_MESSAGE_APP` frame.
    ///
    /// Returns `None` if the encoded text exceeds [`MAX_PAYLOAD`].
    pub fn build_text_frame(&self, to: u32, text: &str) -> Option<MeshFrame> {
        let data = Data {
            portnum: PortNum::TextMessageApp as i32,
            payload: text.as_bytes().to_vec(),
            ..Default::default()
        };
        self.build_frame(to, &data)
    }

    /// Build a `NODEINFO_APP` beacon frame (broadcast).
    pub fn build_nodeinfo_frame(&self) -> Option<MeshFrame> {
        let user = User {
            id:         format!("!{:08x}", self.local.node_id),
            long_name:  self.local.long_name.clone(),
            short_name: self.local.short_name.clone(),
            ..Default::default()
        };
        let data = user.encode_to_data();
        self.build_frame(BROADCAST, &data)
    }

    // ── RX path ───────────────────────────────────────────────────────────

    /// Process a raw LoRa frame received from the PHY pipeline.
    ///
    /// Returns:
    /// - `Ok(Some(msg))` — a decoded message for the application layer.
    /// - `Ok(None)` — packet was a duplicate, hop-limit-exhausted drop, or
    ///   destined for another node (but may have been forwarded).
    /// - `Err(e)` — frame was malformed or failed to decrypt.
    ///
    /// When forwarding is needed the caller receives the forward frame via
    /// the returned `Option<MeshFrame>` in the second tuple slot.
    pub fn process_rx_frame(
        &mut self,
        raw: &[u8],
    ) -> Result<(Option<MeshMessage>, Option<MeshFrame>), ProcessError> {
        let frame = MeshFrame::from_bytes(raw).ok_or(ProcessError::Malformed)?;
        let h = &frame.header;

        // Reject frames on a different channel.
        // MQTT-relayed packets don't carry channel_hash (set to 0), so skip the check.
        if !h.via_mqtt && h.channel_hash != self.channel.channel_hash() {
            return Err(ProcessError::WrongChannel);
        }

        let plaintext = self.crypto.decrypt(h.id, h.from, &frame.payload);

        let decision = route(&frame, self.local.node_id, plaintext, &mut self.dedup);

        match decision {
            RouteDecision::Drop => Ok((None, None)),

            RouteDecision::Forward { frame: fwd } => Ok((None, Some(fwd))),

            RouteDecision::Deliver { plaintext } => {
                let msg = self.decode_payload(h.to, h.from, h.hop_limit, &plaintext)?;
                Ok((Some(msg), None))
            }

            RouteDecision::DeliverAndForward { plaintext, frame: fwd } => {
                let msg = self.decode_payload(h.to, h.from, h.hop_limit, &plaintext)?;
                Ok((Some(msg), Some(fwd)))
            }
        }
    }

    // ── Private ───────────────────────────────────────────────────────────

    fn decode_payload(
        &mut self,
        to:        u32,
        from:      u32,
        hop_limit: u8,
        plaintext: &[u8],
    ) -> Result<MeshMessage, ProcessError> {
        let data = Data::decode(plaintext).map_err(ProcessError::Proto)?;

        // Update neighbour table on NODEINFO_APP.
        if data.portnum == PortNum::NodeinfoApp as i32 {
            if let Ok(user) = data.decode_user() {
                self.neighbours.update(NodeInfo {
                    node_id:    from,
                    short_name: user.short_name,
                    long_name:  user.long_name,
                    last_rssi:  None,
                });
            }
        }

        Ok(MeshMessage { from, to, data, hop_limit })
    }
}

// ── Error type ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ProcessError {
    /// Raw bytes too short to contain a valid header.
    Malformed,
    /// Frame's `channel_hash` does not match this node's channel.
    WrongChannel,
    /// Protobuf decoding failed.
    Proto(prost::DecodeError),
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed    => write!(f, "malformed frame"),
            Self::WrongChannel => write!(f, "wrong channel"),
            Self::Proto(e)     => write!(f, "proto decode: {e}"),
        }
    }
}

impl std::error::Error for ProcessError {}

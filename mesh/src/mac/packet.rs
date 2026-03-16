/// Meshtastic over-the-air packet framing.
///
/// Every frame transmitted over LoRa has this structure:
///
/// ```text
/// Byte offset  Field           Size  Notes
/// ──────────────────────────────────────────────────────────────
///  0           to              4     destination node number (LE)
///  4           from            4     sender node number (LE)
///  8           id              4     random packet ID (LE)
/// 12           flags           1     hop_limit[2:0] | want_ack[3]
///                                    | via_mqtt[4] | hop_start[7:5]
/// 13           channel_hash    1     truncated hash of channel PSK
/// 14           reserved        2     0x0000
/// 16           payload        ≤237   AES-256-CTR encrypted Data proto
/// ```
///
/// The header (16 bytes) is transmitted in plaintext.
/// The body is encrypted with `MeshCrypto`.

pub const HEADER_LEN: usize = 16;
pub const MAX_PAYLOAD: usize = 237; // 253 bytes LoRa max − 16 byte header
pub const BROADCAST: u32 = 0xFFFF_FFFF;

/// Decoded OTA header.
#[derive(Debug, Clone, Copy)]
pub struct MeshHeader {
    pub to:           u32,
    pub from:         u32,
    pub id:           u32,
    pub hop_limit:    u8,
    pub want_ack:     bool,
    pub via_mqtt:     bool,
    pub hop_start:    u8,
    pub channel_hash: u8,
}

impl MeshHeader {
    /// Serialise into a 16-byte on-air header.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut buf = [0u8; HEADER_LEN];
        buf[0..4].copy_from_slice(&self.to.to_le_bytes());
        buf[4..8].copy_from_slice(&self.from.to_le_bytes());
        buf[8..12].copy_from_slice(&self.id.to_le_bytes());
        buf[12] = (self.hop_limit & 0x07)
            | if self.want_ack { 0x08 } else { 0 }
            | if self.via_mqtt { 0x10 } else { 0 }
            | (self.hop_start & 0x07) << 5;
        buf[13] = self.channel_hash;
        // bytes 14–15 remain 0x00
        buf
    }

    /// Deserialise from a 16-byte on-air header.
    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < HEADER_LEN { return None; }
        let flags = buf[12];
        Some(Self {
            to:           u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            from:         u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            id:           u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            hop_limit:    flags & 0x07,
            want_ack:     flags & 0x08 != 0,
            via_mqtt:     flags & 0x10 != 0,
            hop_start:    (flags >> 5) & 0x07,
            channel_hash: buf[13],
        })
    }
}

/// A complete Meshtastic OTA frame (header + encrypted body).
#[derive(Debug, Clone)]
pub struct MeshFrame {
    pub header:  MeshHeader,
    /// Encrypted payload bytes (up to MAX_PAYLOAD).
    pub payload: Vec<u8>,
}

impl MeshFrame {
    /// Encode to raw bytes ready for the LoRa TX pipeline.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(&self.header.encode());
        out.extend_from_slice(&self.payload);
        out
    }

    /// Decode from raw bytes received from the LoRa RX pipeline.
    pub fn from_bytes(raw: &[u8]) -> Option<Self> {
        let header = MeshHeader::decode(raw)?;
        Some(Self {
            header,
            payload: raw[HEADER_LEN..].to_vec(),
        })
    }
}

/// AES-256-CTR encryption for Meshtastic payloads.
///
/// Key:   256-bit channel PSK.
/// Nonce: `[packet_id: u32 LE, 0x00×4, from_node: u32 LE, 0x00×4]` → 128 bits.
///
/// The header (16 bytes) is always transmitted in plaintext.
/// Only the body (Data protobuf) is encrypted.

use aes::Aes256;
use ctr::cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr128BE;

type Aes256Ctr = Ctr128BE<Aes256>;

/// Holds a channel PSK and provides encrypt/decrypt operations.
pub struct MeshCrypto {
    psk: [u8; 32],
}

impl MeshCrypto {
    /// Create from a 256-bit PSK.
    ///
    /// The default Meshtastic public channel key is the single byte `0x01`
    /// zero-padded to 32 bytes.
    pub fn new(psk: [u8; 32]) -> Self {
        Self { psk }
    }

    /// The default public channel PSK (`0x01` padded to 32 bytes).
    pub fn public_psk() -> [u8; 32] {
        let mut k = [0u8; 32];
        k[0] = 0x01;
        k
    }

    fn nonce(packet_id: u32, from_node: u32) -> [u8; 16] {
        let mut nonce = [0u8; 16];
        nonce[0..4].copy_from_slice(&packet_id.to_le_bytes());
        // bytes 4–7: 0x00
        nonce[8..12].copy_from_slice(&from_node.to_le_bytes());
        // bytes 12–15: 0x00
        nonce
    }

    /// Encrypt `plaintext` in-place using the packet header fields as the nonce.
    pub fn encrypt(&self, packet_id: u32, from_node: u32, plaintext: &[u8]) -> Vec<u8> {
        let mut buf = plaintext.to_vec();
        self.apply_keystream(packet_id, from_node, &mut buf);
        buf
    }

    /// Decrypt `ciphertext` in-place (CTR mode: same operation as encrypt).
    pub fn decrypt(&self, packet_id: u32, from_node: u32, ciphertext: &[u8]) -> Vec<u8> {
        self.encrypt(packet_id, from_node, ciphertext)
    }

    fn apply_keystream(&self, packet_id: u32, from_node: u32, buf: &mut [u8]) {
        let nonce = Self::nonce(packet_id, from_node);
        let mut cipher = Aes256Ctr::new(self.psk.as_ref().into(), nonce.as_ref().into());
        cipher.apply_keystream(buf);
    }
}

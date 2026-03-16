/// Meshtastic serial framing protocol.
///
/// Every message on the serial link is wrapped in a simple frame:
///
/// ```text
/// [0x94] [0xC3] [len_msb] [len_lsb] [protobuf_payload...]
/// ```
///
/// - Magic bytes `0x94 0xC3` mark the start of a frame.
/// - `len` is a big-endian u16 giving the length of the protobuf payload.
/// - Maximum payload: 512 bytes (Meshtastic firmware limit).
///
/// This module provides:
/// - [`encode`] — wrap a protobuf-encoded message in a frame.
/// - [`StreamDecoder`] — incremental byte-by-byte decoder for incoming frames.

/// Magic bytes that precede every serial frame.
pub const MAGIC: [u8; 2] = [0x94, 0xC3];

/// Maximum protobuf payload per frame (Meshtastic firmware limit).
pub const MAX_FRAME_PAYLOAD: usize = 512;

/// Wrap a protobuf-encoded payload into a serial frame.
///
/// Returns `None` if `payload` exceeds [`MAX_FRAME_PAYLOAD`].
pub fn encode(payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() > MAX_FRAME_PAYLOAD { return None; }
    let len = payload.len() as u16;
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&MAGIC);
    buf.push((len >> 8) as u8);
    buf.push((len & 0xFF) as u8);
    buf.extend_from_slice(payload);
    Some(buf)
}

/// Incremental serial frame decoder.
///
/// Feed bytes one-at-a-time or in chunks via [`push`](StreamDecoder::push).
/// Complete frames are returned by [`next_frame`](StreamDecoder::next_frame).
///
/// The decoder is stateful and handles partial frames across `push` calls.
pub struct StreamDecoder {
    state:   State,
    len:     u16,
    buf:     Vec<u8>,
    frames:  std::collections::VecDeque<Vec<u8>>,
}

#[derive(Clone, Copy)]
enum State {
    /// Waiting for first magic byte (0x94).
    Magic0,
    /// Got 0x94, waiting for 0xC3.
    Magic1,
    /// Got magic, waiting for length MSB.
    LenMsb,
    /// Got MSB, waiting for length LSB.
    LenLsb,
    /// Accumulating payload bytes.
    Payload,
}

impl StreamDecoder {
    pub fn new() -> Self {
        Self {
            state:  State::Magic0,
            len:    0,
            buf:    Vec::new(),
            frames: std::collections::VecDeque::new(),
        }
    }

    /// Feed raw bytes into the decoder.
    pub fn push(&mut self, data: &[u8]) {
        for &b in data {
            self.feed_byte(b);
        }
    }

    /// Retrieve the next complete frame payload (if any).
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        self.frames.pop_front()
    }

    fn feed_byte(&mut self, b: u8) {
        match self.state {
            State::Magic0 => {
                if b == MAGIC[0] {
                    self.state = State::Magic1;
                }
                // else: discard — resync
            }
            State::Magic1 => {
                if b == MAGIC[1] {
                    self.state = State::LenMsb;
                } else if b == MAGIC[0] {
                    // Stay in Magic1 (could be repeated 0x94).
                } else {
                    self.state = State::Magic0;
                }
            }
            State::LenMsb => {
                self.len = (b as u16) << 8;
                self.state = State::LenLsb;
            }
            State::LenLsb => {
                self.len |= b as u16;
                if self.len == 0 || self.len as usize > MAX_FRAME_PAYLOAD {
                    // Invalid length — resync.
                    self.state = State::Magic0;
                } else {
                    self.buf.clear();
                    self.state = State::Payload;
                }
            }
            State::Payload => {
                self.buf.push(b);
                if self.buf.len() == self.len as usize {
                    self.frames.push_back(std::mem::take(&mut self.buf));
                    self.state = State::Magic0;
                }
            }
        }
    }
}

impl Default for StreamDecoder {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let payload = b"hello mesh";
        let frame = encode(payload).unwrap();
        assert_eq!(&frame[..2], &MAGIC);
        assert_eq!(frame[2], 0);
        assert_eq!(frame[3], payload.len() as u8);

        let mut dec = StreamDecoder::new();
        dec.push(&frame);
        let got = dec.next_frame().unwrap();
        assert_eq!(got, payload);
    }

    #[test]
    fn resync_on_garbage() {
        let payload = b"test";
        let frame = encode(payload).unwrap();

        let mut dec = StreamDecoder::new();
        dec.push(b"\xFF\xFF\x00"); // garbage
        dec.push(&frame);
        let got = dec.next_frame().unwrap();
        assert_eq!(got, payload);
    }

    #[test]
    fn split_across_pushes() {
        let payload = b"split";
        let frame = encode(payload).unwrap();

        let mut dec = StreamDecoder::new();
        dec.push(&frame[..3]); // magic + half of length
        assert!(dec.next_frame().is_none());
        dec.push(&frame[3..]); // rest
        let got = dec.next_frame().unwrap();
        assert_eq!(got, payload);
    }
}

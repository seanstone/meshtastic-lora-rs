/// Flood router with hop-limit and dedup cache.
///
/// Routing algorithm:
///
/// **TX path**
/// 1. Build `MeshHeader { to, from=node_id, id=rand_u32(), hop_limit=3, hop_start=3 }`.
/// 2. Encrypt body with `MeshCrypto`.
/// 3. Enqueue `MeshFrame` for the PHY TX pipeline.
///
/// **RX path**
/// 1. Decode `MeshFrame` from raw LoRa payload.
/// 2. Check dedup cache: if `(from, id)` already seen → drop.
/// 3. Insert into dedup cache.
/// 4. Decrypt body; if `to == node_id || to == BROADCAST` → deliver to app.
/// 5. If `hop_limit > 0`: decrement, re-encrypt with updated header, apply
///    random jitter (0–5 s), re-enqueue for TX.

use std::collections::VecDeque;
use crate::mac::packet::{MeshHeader, MeshFrame, BROADCAST};

/// Capacity of the packet deduplication ring buffer.
const DEDUP_CAPACITY: usize = 50;

/// Ring buffer of recently-seen (from_node, packet_id) pairs.
pub struct DedupCache {
    entries: VecDeque<(u32, u32)>,
}

impl DedupCache {
    pub fn new() -> Self {
        Self { entries: VecDeque::with_capacity(DEDUP_CAPACITY) }
    }

    /// Returns `true` if the packet was already seen (duplicate).
    /// Inserts the entry on a cache miss.
    pub fn check_and_insert(&mut self, from: u32, id: u32) -> bool {
        if self.entries.contains(&(from, id)) {
            return true; // duplicate
        }
        if self.entries.len() == DEDUP_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back((from, id));
        false
    }
}

/// Decision returned by the router for each received frame.
pub enum RouteDecision {
    /// Deliver plaintext body to the application layer.
    Deliver { plaintext: Vec<u8> },
    /// Re-broadcast with decremented hop_limit (after jitter delay).
    Forward  { frame: MeshFrame },
    /// Deliver AND forward (this node is the destination but should also relay).
    DeliverAndForward { plaintext: Vec<u8>, frame: MeshFrame },
    /// Drop silently (duplicate or hop_limit exhausted).
    Drop,
}

/// Stateless routing function — does not hold crypto or node state.
/// Callers must supply the decrypted body and the local node_id.
pub fn route(
    frame:        &MeshFrame,
    node_id:      u32,
    plaintext:    Vec<u8>,
    dedup:        &mut DedupCache,
) -> RouteDecision {
    let h = &frame.header;

    // Dedup check.
    if dedup.check_and_insert(h.from, h.id) {
        return RouteDecision::Drop;
    }

    let for_us = h.to == node_id || h.to == BROADCAST;

    if h.hop_limit == 0 {
        return if for_us {
            RouteDecision::Deliver { plaintext }
        } else {
            RouteDecision::Drop
        };
    }

    // Build the forwarded frame with decremented hop_limit.
    let fwd_header = MeshHeader {
        hop_limit: h.hop_limit - 1,
        ..*h
    };
    let fwd_frame = MeshFrame { header: fwd_header, payload: frame.payload.clone() };

    if for_us {
        RouteDecision::DeliverAndForward { plaintext, frame: fwd_frame }
    } else {
        RouteDecision::Forward { frame: fwd_frame }
    }
}

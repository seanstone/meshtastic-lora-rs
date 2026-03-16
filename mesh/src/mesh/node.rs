/// Node identity and neighbour table.

use rand::Rng;
use std::collections::HashMap;

/// Information about a mesh node (mirrors Meshtastic `User` protobuf).
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub node_id:    u32,
    pub short_name: String,
    pub long_name:  String,
    /// Last-heard RSSI in dBm, if available.
    pub last_rssi:  Option<i16>,
}

/// This node's persistent identity.
pub struct LocalNode {
    pub node_id:    u32,
    pub short_name: String,
    pub long_name:  String,
}

impl LocalNode {
    /// Generate a new node with a random ID.
    pub fn new(short_name: impl Into<String>, long_name: impl Into<String>) -> Self {
        let node_id = rand::thread_rng().r#gen::<u32>();
        Self { node_id, short_name: short_name.into(), long_name: long_name.into() }
    }

    /// Reconstruct a node from a previously-persisted ID.
    pub fn with_id(node_id: u32, short_name: impl Into<String>, long_name: impl Into<String>) -> Self {
        Self { node_id, short_name: short_name.into(), long_name: long_name.into() }
    }

    pub fn node_info(&self) -> NodeInfo {
        NodeInfo {
            node_id:    self.node_id,
            short_name: self.short_name.clone(),
            long_name:  self.long_name.clone(),
            last_rssi:  None,
        }
    }
}

/// Live view of neighbouring nodes.
#[derive(Default)]
pub struct NeighbourTable {
    nodes: HashMap<u32, NodeInfo>,
}

impl NeighbourTable {
    pub fn update(&mut self, info: NodeInfo) {
        self.nodes.insert(info.node_id, info);
    }

    pub fn all(&self) -> impl Iterator<Item = &NodeInfo> {
        self.nodes.values()
    }

    pub fn get(&self, node_id: u32) -> Option<&NodeInfo> {
        self.nodes.get(&node_id)
    }
}

use std::collections::HashMap;

/// A single node in the Drain prefix tree.
///
/// Interior nodes map token strings to child nodes.
/// Leaf nodes carry a list of cluster IDs whose templates share the same prefix path.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Node {
    pub key_to_child_node: HashMap<String, Node>,
    pub cluster_ids: Vec<usize>,
}

impl Node {
    pub fn new() -> Self {
        Self {
            key_to_child_node: HashMap::new(),
            cluster_ids: Vec::new(),
        }
    }
}

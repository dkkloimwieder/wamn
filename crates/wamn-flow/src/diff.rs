//! Structured version diff between two flows — the editor's change view.
//!
//! Compares by node id, edge identity, trigger, entry and credential set, so a
//! reviewer sees *what changed* between two versions rather than a text diff.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::types::{Edge, Flow, NodeId, Trigger};

/// What changed about a single node kept across both versions.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeChange {
    pub id: NodeId,
    /// `Some((old, new))` if the node's `type` changed.
    pub type_changed: Option<(String, String)>,
    /// `true` if the node's `config` object changed.
    pub config_changed: bool,
    /// `Some((old, new))` if the node's credential reference changed.
    pub credential_changed: Option<(Option<String>, Option<String>)>,
}

impl NodeChange {
    fn any(&self) -> bool {
        self.type_changed.is_some() || self.config_changed || self.credential_changed.is_some()
    }
}

/// A structured diff from `old` to `new`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlowDiff {
    pub nodes_added: Vec<NodeId>,
    pub nodes_removed: Vec<NodeId>,
    pub nodes_changed: Vec<NodeChange>,
    pub edges_added: Vec<Edge>,
    pub edges_removed: Vec<Edge>,
    pub entry_changed: Option<(NodeId, NodeId)>,
    pub trigger_changed: Option<(Trigger, Trigger)>,
    pub credentials_added: Vec<String>,
    pub credentials_removed: Vec<String>,
}

impl FlowDiff {
    /// `true` if the two flow versions are structurally identical.
    pub fn is_empty(&self) -> bool {
        self.nodes_added.is_empty()
            && self.nodes_removed.is_empty()
            && self.nodes_changed.is_empty()
            && self.edges_added.is_empty()
            && self.edges_removed.is_empty()
            && self.entry_changed.is_none()
            && self.trigger_changed.is_none()
            && self.credentials_added.is_empty()
            && self.credentials_removed.is_empty()
    }
}

/// Diff `old` → `new`. Node identity is `id`; edge identity is the full
/// `(from, from_port, to, to_port)` tuple ([`Edge`] is `Eq + Hash`).
pub fn diff(old: &Flow, new: &Flow) -> FlowDiff {
    let old_nodes: BTreeMap<&str, &crate::types::Node> =
        old.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let new_nodes: BTreeMap<&str, &crate::types::Node> =
        new.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let mut d = FlowDiff::default();

    for (id, n) in &new_nodes {
        if !old_nodes.contains_key(id) {
            d.nodes_added.push((*id).to_string());
        } else {
            let o = old_nodes[id];
            let change = NodeChange {
                id: (*id).to_string(),
                type_changed: (o.node_type != n.node_type)
                    .then(|| (o.node_type.clone(), n.node_type.clone())),
                config_changed: o.config != n.config,
                credential_changed: (o.credential != n.credential)
                    .then(|| (o.credential.clone(), n.credential.clone())),
            };
            if change.any() {
                d.nodes_changed.push(change);
            }
        }
    }
    for id in old_nodes.keys() {
        if !new_nodes.contains_key(id) {
            d.nodes_removed.push((*id).to_string());
        }
    }

    let old_edges: HashSet<&Edge> = old.edges.iter().collect();
    let new_edges: HashSet<&Edge> = new.edges.iter().collect();
    for e in new.edges.iter() {
        if !old_edges.contains(e) {
            d.edges_added.push(e.clone());
        }
    }
    for e in old.edges.iter() {
        if !new_edges.contains(e) {
            d.edges_removed.push(e.clone());
        }
    }

    if old.entry != new.entry {
        d.entry_changed = Some((old.entry.clone(), new.entry.clone()));
    }
    if old.trigger != new.trigger {
        d.trigger_changed = Some((old.trigger.clone(), new.trigger.clone()));
    }

    let old_creds: BTreeSet<&str> = old.credentials.iter().map(|c| c.name.as_str()).collect();
    let new_creds: BTreeSet<&str> = new.credentials.iter().map(|c| c.name.as_str()).collect();
    d.credentials_added = new_creds
        .difference(&old_creds)
        .map(|s| s.to_string())
        .collect();
    d.credentials_removed = old_creds
        .difference(&new_creds)
        .map(|s| s.to_string())
        .collect();

    d
}

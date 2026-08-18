//! The resolved wiring: the router's own graph type, indexed for O(1) node
//! lookup and outgoing-edge routing.
//!
//! A wiring is **data** — the active version comes from the env-hot store and is
//! cached by version by the caller. [`Wiring::compile`] indexes one and checks
//! only the structural preconditions the walk relies on; gating a wiring against
//! the palette is upstream of the router, not its job.

use std::collections::HashMap;

use serde_json::Value;

use crate::terminal::Terminal;

/// Default per-delivery hop limit (see [`Wiring::set_hop_limit`]): generous for
/// a legitimate loop, but bounds a permitted cycle that never terminates.
pub const DEFAULT_HOP_LIMIT: u64 = 10_000;

/// One node of a wiring: a component operation with its opaque config.
#[derive(Debug, Clone, PartialEq)]
pub struct WiringNode {
    /// Node id, unique within the wiring.
    pub id: String,
    /// The component this node invokes — also the instance-pool key.
    pub component: String,
    /// The operation on that component's node interface.
    pub operation: String,
    /// Opaque per-node config. The router reads only the reserved `retry` object
    /// and `deadline-ms`; everything else belongs to the component.
    pub config: Value,
    /// The connection handle bound to this node, if any.
    pub connection: Option<String>,
    /// The [`Terminal`] this node ends the delivery with, if it is one. `None`
    /// on ordinary nodes.
    pub terminal: Option<Terminal>,
}

/// One directed edge, leaving `from` on `from_port` and arriving at `to`.
#[derive(Debug, Clone, PartialEq)]
pub struct WiringEdge {
    pub from: String,
    pub from_port: String,
    pub to: String,
    /// Fan-out order within the `(from, from_port)` group. Absent sorts as 0.
    pub ordinal: Option<u32>,
}

/// Why a wiring could not be compiled into a walkable graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiringError {
    kind: WiringErrorKind,
}

/// The structural precondition a wiring broke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WiringErrorKind {
    /// Two nodes share an id.
    DuplicateNode(String),
    /// The named entry node is not in the wiring.
    UnresolvedEntry(String),
    /// An edge names an endpoint that is not in the wiring.
    UnresolvedEndpoint { from: String, endpoint: String },
}

impl WiringError {
    /// What the wiring broke.
    pub fn kind(&self) -> &WiringErrorKind {
        &self.kind
    }
}

impl std::fmt::Display for WiringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            WiringErrorKind::DuplicateNode(id) => write!(f, "duplicate node id {id:?}"),
            WiringErrorKind::UnresolvedEntry(id) => {
                write!(f, "entry node {id:?} is not in the wiring")
            }
            WiringErrorKind::UnresolvedEndpoint { from, endpoint } => {
                write!(f, "edge from {from:?} names unknown node {endpoint:?}")
            }
        }
    }
}

impl std::error::Error for WiringError {}

/// A resolved wiring, indexed by node id and by from-node for edge routing.
#[derive(Debug, Clone)]
pub struct Wiring {
    entry: String,
    by_id: HashMap<String, WiringNode>,
    /// All edges leaving a node, grouped by the from-node id (filtered by port at
    /// lookup — a node has only a handful of outgoing edges).
    out: HashMap<String, Vec<WiringEdge>>,
    pub(crate) hop_limit: u64,
}

impl Wiring {
    /// Index a resolved wiring, checking the preconditions the walk relies on:
    /// unique node ids, an entry that resolves, and edge endpoints that resolve.
    pub fn compile(
        entry: impl Into<String>,
        nodes: Vec<WiringNode>,
        edges: Vec<WiringEdge>,
    ) -> Result<Wiring, WiringError> {
        let entry = entry.into();
        let mut by_id = HashMap::with_capacity(nodes.len());
        for node in nodes {
            let id = node.id.clone();
            if by_id.insert(id.clone(), node).is_some() {
                return Err(WiringError {
                    kind: WiringErrorKind::DuplicateNode(id),
                });
            }
        }
        if !by_id.contains_key(&entry) {
            return Err(WiringError {
                kind: WiringErrorKind::UnresolvedEntry(entry),
            });
        }

        let mut out: HashMap<String, Vec<WiringEdge>> = HashMap::new();
        for edge in edges {
            for endpoint in [&edge.from, &edge.to] {
                if !by_id.contains_key(endpoint) {
                    return Err(WiringError {
                        kind: WiringErrorKind::UnresolvedEndpoint {
                            from: edge.from.clone(),
                            endpoint: endpoint.clone(),
                        },
                    });
                }
            }
            out.entry(edge.from.clone()).or_default().push(edge);
        }
        // Fan-out order is the explicit `WiringEdge::ordinal`, not array
        // position. Ordinals are scoped to a `(from, from_port)` group, so
        // sorting a whole from-node group here and filtering by port in
        // `successors` yields each port's edges in ordinal order. Stable, so a
        // wiring built without ordinals keeps the order its edges were built in.
        for edges in out.values_mut() {
            edges.sort_by_key(|edge| edge.ordinal.unwrap_or(0));
        }

        Ok(Wiring {
            entry,
            by_id,
            out,
            hop_limit: DEFAULT_HOP_LIMIT,
        })
    }

    /// Override the per-delivery hop limit. Cycles are a wiring feature, so
    /// termination is bounded at runtime instead: once the walk has handed out
    /// this many invocations (retries included) for one delivery, it fails with
    /// the terminal [`FailureKind::HopLimit`](crate::FailureKind::HopLimit) —
    /// never routed to an error path, which could itself be part of the loop.
    pub fn set_hop_limit(&mut self, limit: u64) {
        self.hop_limit = limit;
    }

    /// The per-delivery hop limit in force.
    pub fn hop_limit(&self) -> u64 {
        self.hop_limit
    }

    /// The id of the entry node (guaranteed present post-compile).
    pub fn entry(&self) -> &str {
        &self.entry
    }

    /// Look up a node by id.
    pub fn node(&self, id: &str) -> Option<&WiringNode> {
        self.by_id.get(id)
    }

    /// The edges leaving `node` on `port`, in ordinal order within their
    /// `(from, port)` group, so fan-out to several targets is deterministic.
    pub fn successors(&self, node: &str, port: &str) -> impl Iterator<Item = &WiringEdge> {
        self.out
            .get(node)
            .into_iter()
            .flatten()
            .filter(move |edge| edge.from_port == port)
    }
}

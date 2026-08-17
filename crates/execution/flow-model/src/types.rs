//! Canonical flow-graph types (5.1).
//!
//! A flow is **data, not code**: a versioned directed graph of typed nodes
//! wired by ported edges, starting at one typed entry node. Ordinary node
//! `type` values are open strings resolved by the runner's node library (5.3).

use std::collections::HashMap;

use crate::canonical;
use crate::node_contract::ConnectionTypeDescriptor;
use crate::preimage::FlowPreimage;
use crate::test_set::TestSetCase;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// The flow-schema **format** version this crate implements. Distinct from a
/// flow's own [`Flow::version`]. This pre-version-alpha contract is refreshed
/// from zero; there is one current reader and no legacy migration path.
pub const SCHEMA_VERSION: &str = "0.1";

/// The default (main) output port of a node.
pub const MAIN_PORT: &str = "main";
/// The reserved output port a node emits on when it errors — the "error path"
/// (5.2). Edges from this port route failures without aborting the run.
pub const ERROR_PORT: &str = "error";
/// Node types which identify the graph's unique entry.
pub const ENTRY_TYPES: [&str; 2] = ["request", "event"];

/// A stable node identifier, unique within a flow.
pub type NodeId = String;

/// One version of a flow — the unit stored in the catalog and pointed at by the
/// active-version pointer (deploying = flipping that pointer + a NATS doorbell,
/// 5.14).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Flow {
    /// The flow-schema format version (e.g. `"0.1"`). See [`SCHEMA_VERSION`].
    pub schema_version: String,
    /// Stable identifier shared across every version of this flow.
    pub flow_id: String,
    /// Monotonic version of this flow (>= 1).
    pub version: u32,
    /// Human-readable label (editor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The nodes of the graph. Exactly one has a type in [`ENTRY_TYPES`].
    pub nodes: Vec<Node>,
    /// The wiring between node output ports and downstream nodes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<Edge>,
    /// Portable connection requirements named by this artifact. Environment
    /// bindings, instances, generations, authorities, and secrets never enter
    /// this declaration or the artifact identity derived from it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connection_requirements: Vec<FlowConnectionRequirement>,
    /// The publish gate's test cases. Tests live in the draft beside the graph,
    /// so one draft hash covers both and a successor draft carries them forward
    /// with the rest of the document. Bounded by
    /// [`crate::validate_cases`]; the array's order is each case's ordinal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cases: Vec<TestSetCase>,
}

/// A single graph step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Node {
    /// Unique within the flow.
    pub id: NodeId,
    /// The node type. Engine-reserved types are checked here; ordinary open
    /// strings are resolved through the pinned public node interfaces.
    #[serde(rename = "type")]
    pub node_type: String,
    /// Human-readable label (editor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Opaque per-node configuration — a JSON object typed by the node library
    /// (5.3), not by this crate.
    #[serde(default, skip_serializing_if = "is_empty_object")]
    pub config: Value,
    /// The artifact-local connection requirement consumed by this integrate
    /// node. Pure and control nodes do not carry a connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
}

/// One artifact-local name bound to a portable connection requirement.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct FlowConnectionRequirement {
    /// Artifact-local logical name referenced by [`Node::connection`].
    pub name: String,
    /// Portable type, contract, and field-ownership requirement.
    pub requirement: ConnectionTypeDescriptor,
}

impl Node {
    /// The engine-reserved entry kind represented by this node, if any.
    pub fn entry_kind(&self) -> Option<EntryKind> {
        match self.node_type.as_str() {
            "request" => Some(EntryKind::Request),
            "event" => Some(EntryKind::Event),
            _ => None,
        }
    }
}

/// A wire from one node's output port to a downstream node. Branch = several
/// edges from distinct ports of one node; merge = several edges into one node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Edge {
    /// Source node id.
    pub from: NodeId,
    /// Source output port. Defaults to [`MAIN_PORT`]; [`ERROR_PORT`] is the
    /// error path; node-library node types may define others (e.g. a
    /// `conditional`'s `true`/`false`).
    #[serde(default = "main_port", skip_serializing_if = "is_main_port")]
    pub from_port: String,
    /// Target node id.
    pub to: NodeId,
    /// Target input port. Defaults to the node's single input; present only for
    /// future multi-input node types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_port: Option<String>,
    /// This edge's **fan-out order** within its `(from, from-port)` group: the
    /// sequence the engine dispatches a branch's targets in
    /// ([`crate::Flow`] consumers see it through `Plan::successors`).
    ///
    /// Fan-out order is author-meaningful and therefore an explicit value, never
    /// an element's position in the `edges` array (W2 digest ordering rule 2).
    /// An authored document may omit it; [`Flow::from_json`] then materializes
    /// the edge's position within its group exactly once, at parse, so the
    /// document's order is preserved as an explicit value and array position
    /// stops mattering thereafter. Only order *within* a group is meaningful:
    /// two edges leaving different nodes or different ports never compare.
    ///
    /// `None` therefore only appears on a flow built in Rust without going
    /// through [`Flow::from_json`]; it sorts as `0`, which for such a flow keeps
    /// the array order it was built in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u32>,
}

/// The graph's unique engine-reserved entry kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Request,
    Event,
}

/// Configuration carried by a `request` entry node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RequestConfig {
    /// The draft 2020-12 contract checked before a run is admitted.
    pub input_schema: Value,
}

/// Configuration carried by a `respond` node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RespondConfig {
    /// Final caller status. Informational statuses cannot release a caller.
    pub status: u16,
}

/// Configuration carried by a universal `fail` node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct FailConfig {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default = "default_fail_status")]
    pub status: u16,
}

fn default_fail_status() -> u16 {
    400
}

/// Configuration carried by the engine-reserved `call-flow` node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CallFlowConfig {
    /// Tenant-scoped flow identifier resolved in the parent's pinned release.
    pub flow_id: String,
}

/// Input synthesized for an `event` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct EventInput {
    pub event: RowEvent,
    /// The new row image. Absent when the operation has no new image.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_image"
    )]
    pub new: Option<Map<String, Value>>,
    /// The old row image. Absent when the operation has no old image; `null`
    /// is deliberately rejected so omission has one wire representation.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_image"
    )]
    pub old: Option<Map<String, Value>>,
}

fn deserialize_image<'de, D>(deserializer: D) -> Result<Option<Map<String, Value>>, D::Error>
where
    D: Deserializer<'de>,
{
    Map::<String, Value>::deserialize(deserializer).map(Some)
}

/// A durable row mutation carried by [`EventInput`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum RowEvent {
    #[default]
    Insert,
    Update,
    Delete,
}

impl Flow {
    /// The unique typed entry node, or `None` while cardinality is invalid.
    pub fn entry_node(&self) -> Option<&Node> {
        let mut entries = self.nodes.iter().filter(|node| node.entry_kind().is_some());
        let entry = entries.next()?;
        entries.next().is_none().then_some(entry)
    }

    /// Parse a flow from JSON (import).
    ///
    /// Materializes any absent [`Edge::ordinal`] from the edge's position within
    /// its `(from, from-port)` group — the one point where document order is read
    /// as fan-out order. Idempotent: re-parsing an exported flow changes nothing.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        let mut flow: Flow = serde_json::from_str(s)?;
        assign_edge_ordinals(&mut flow.edges);
        Ok(flow)
    }

    /// Serialize a flow to human-readable JSON (export).
    pub fn to_json(&self) -> String {
        // Infallible for this type; a plain data struct never fails to encode.
        serde_json::to_string_pretty(self).expect("Flow serializes")
    }

    /// RFC 8785 JSON Canonicalization Scheme bytes for artifact identity.
    ///
    /// Hashes the [`FlowPreimage`] projection rather than the document, so node
    /// frames are ordered by [`Node::id`] (W2 digest ordering).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let value = serde_json::to_value(FlowPreimage::of(self)).expect("Flow serializes");
        canonical::to_vec(&value)
    }

    /// SHA-256 of [`Flow::canonical_bytes`], the stored `graph_hash`.
    pub fn graph_hash(&self) -> [u8; 32] {
        canonical::sha256(&self.canonical_bytes())
    }
}

/// Give every edge that omitted [`Edge::ordinal`] its position within its
/// `(from, from-port)` group. The counter advances for every edge in a group,
/// author-supplied ordinals included, so an explicit value is never overwritten
/// and never shifts the positions around it.
fn assign_edge_ordinals(edges: &mut [Edge]) {
    let mut positions: HashMap<(String, String), u32> = HashMap::new();
    for edge in edges {
        let position = positions
            .entry((edge.from.clone(), edge.from_port.clone()))
            .or_default();
        if edge.ordinal.is_none() {
            edge.ordinal = Some(*position);
        }
        *position += 1;
    }
}

fn main_port() -> String {
    MAIN_PORT.to_string()
}

fn is_main_port(p: &str) -> bool {
    p == MAIN_PORT
}

fn is_empty_object(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.is_empty(),
        Value::Null => true,
        _ => false,
    }
}

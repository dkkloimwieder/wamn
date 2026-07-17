//! Canonical flow-graph types (5.1).
//!
//! A flow is **data, not code**: a versioned directed graph of typed nodes
//! wired by ported edges, invoked by one trigger, referencing credentials by
//! name. Node `type` is an open string resolved by the runner's node library
//! (5.3) — this crate validates graph *structure*, not per-node-type config.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The flow-schema **format** version this crate implements. Distinct from a
/// flow's own [`Flow::version`]. Compatibility rule (mirrors the WIT freeze):
/// `0.1.x` is additive/clarifying only; a breaking change waits for `0.2`.
pub const SCHEMA_VERSION: &str = "0.1";

/// The default (main) output port of a node.
pub const MAIN_PORT: &str = "main";
/// The reserved output port a node emits on when it errors — the "error path"
/// (5.2). Edges from this port route failures without aborting the run.
pub const ERROR_PORT: &str = "error";

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
    /// How the flow is invoked. Exactly one.
    pub trigger: Trigger,
    /// The node the trigger payload enters the graph at.
    pub entry: NodeId,
    /// The nodes of the graph.
    pub nodes: Vec<Node>,
    /// The wiring between node output ports and downstream nodes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<Edge>,
    /// Credentials the flow needs, declared by logical name and resolved by the
    /// vault (5.9) at run time. Nodes reference these by [`Node::credential`];
    /// secrets never appear in flow data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<CredentialRef>,
    /// Hosts this flow's outbound HTTP may reach (fqg.11). Entries use the
    /// host allowlist grammar: `host[:port]`, `scheme://host[:port]`, or a
    /// `*.suffix` subdomain wildcard. Egress is opt-in and fail-closed —
    /// undeclared (or empty) means DENY-ALL for the flow, and a declared host
    /// is still bounded by the runner's host-level allowlist (both must
    /// allow). Mirrors [`Flow::credentials`]: capability by declaration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_hosts: Vec<String>,
}

/// A single graph step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Node {
    /// Unique within the flow.
    pub id: NodeId,
    /// The node type — an open string the runner's node library (5.3) resolves
    /// (e.g. `postgres-query`, `transform`, `http-request`, `conditional`,
    /// `respond`, `delay`, `custom`). Structural validation here does not
    /// constrain it.
    #[serde(rename = "type")]
    pub node_type: String,
    /// Human-readable label (editor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Opaque per-node configuration — a JSON object typed by the node library
    /// (5.3), not by this crate.
    #[serde(default, skip_serializing_if = "is_empty_object")]
    pub config: Value,
    /// Optional reference to a declared credential by [`CredentialRef::name`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
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
}

/// How a flow is invoked. The dispatcher (5.14) registers cron and row-event
/// triggers; webhook triggers are routed by the API gateway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Trigger {
    /// HTTP webhook. `sync` = respond within the request (write-ahead default,
    /// D15); otherwise fire-and-forget.
    Webhook {
        #[serde(default)]
        sync: bool,
        /// Optional path suffix under the flow's webhook route.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// Scheduled invocation (cron expression). Dispatcher-owned; wakes parked
    /// projects (F3).
    Cron { schedule: String },
    /// Fires from a durable row event (outbox), e.g. F4 on `dispositions`
    /// insert — the outbox + doorbell path (D4, 5.14).
    RowEvent {
        table: String,
        #[serde(default)]
        event: RowEvent,
    },
    /// Manual / test-run invocation (editor test-run).
    Manual,
}

/// The row mutation a [`Trigger::RowEvent`] fires on.
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

/// A credential the flow references by logical name; the vault (5.9) resolves it
/// to a lazy handle at run time. No secret material is ever stored in flow data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CredentialRef {
    /// Logical name referenced by [`Node::credential`].
    pub name: String,
    /// Optional hint for the editor's credential picker (e.g. `http-basic`,
    /// `api-key`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Human-readable description (editor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Flow {
    /// Parse a flow from canonical JSON (import).
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Serialize a flow to canonical pretty JSON (export).
    pub fn to_json(&self) -> String {
        // Infallible for this type; a plain data struct never fails to encode.
        serde_json::to_string_pretty(self).expect("Flow serializes")
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

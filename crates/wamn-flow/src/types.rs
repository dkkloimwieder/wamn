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
    /// How this flow's `partitioned(key)` runs dispatch when the key's earliest
    /// (head) run is unavailable — backed off, parked, or budget-exhausted
    /// (5.11 ordering decision, D20). Absent = [`PartitionPolicy::Blocking`]:
    /// choosing partitioned dispatch *is* opting into ordering. Inert for a
    /// flow whose runs carry no partition key; materialized onto each queue
    /// row at enqueue (`wamn-run-queue`) so the claim SQL is self-contained.
    #[serde(default, skip_serializing_if = "PartitionPolicy::is_default")]
    pub partition_policy: PartitionPolicy,
    /// The flow's record-stream ordering (5.11): which ordered stream its runs
    /// join. Absent = [`Ordering::Unordered`] (today's global-claim behavior).
    /// The dispatcher evaluates this at fire() and stamps
    /// `run_queue.partition_key` ([`Ordering::partition_key_for`]); the CDC
    /// materializer (wamn-l5i9.17) consumes the SAME declaration. Composes with
    /// [`Flow::partition_policy`], which is the head-unavailability policy
    /// *within* a stream (orthogonal: this picks the stream, that ranks it).
    #[serde(default, skip_serializing_if = "Ordering::is_default")]
    pub ordering: Ordering,
}

/// The flow's record-stream **ordering** (5.11): which ordered stream a run
/// joins. This is the run's `run_queue.partition_key` seam — the dispatcher
/// evaluates the declaration at fire() and stamps the key; a NULL key is the
/// order-agnostic global claim. Orthogonal to [`PartitionPolicy`], which ranks
/// runs *within* one stream (D20). Absent = [`Ordering::Unordered`].
///
/// Per-node ordering (the 5.11 plan wording) is deferred: the queue's dispatch
/// unit is the **run** (records map 1:1 to runs, D9), so ordering is declared
/// on the flow, not per node — a later refinement if per-node streams are ever
/// needed. See `docs/run-queue.md` §Flow-level ordering declaration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum Ordering {
    /// No ordering: runs dispatch by the global claim in `available_at` order
    /// (`partition_key` NULL). Today's behavior, and the default.
    #[default]
    Unordered,
    /// The whole flow is ONE ordered stream: every run carries a constant
    /// partition key (the flow id), so the flow's runs dispatch strictly
    /// in-order — one in flight per stream — across replicas.
    Strict,
    /// Per-key ordering: the run's partition key is a [JMESPath] over the run
    /// input. Runs sharing a key dispatch in-order; distinct keys dispatch
    /// independently.
    ///
    /// [JMESPath]: https://jmespath.org/
    Partitioned {
        /// The JMESPath expression evaluated over the run input to produce the
        /// stream key. Validated for syntactic well-formedness by
        /// [`crate::validate`].
        #[serde(rename = "partition-key")]
        partition_key: String,
    },
}

impl Ordering {
    /// `true` for the default ([`Ordering::Unordered`]) — used to omit the field
    /// on export so flows round-trip minimal.
    pub fn is_default(&self) -> bool {
        matches!(self, Ordering::Unordered)
    }

    /// The `run_queue.partition_key` a run of this flow carries under this
    /// ordering, given the flow id and the run input (5.11 dispatcher stamping;
    /// the CDC materializer reuses it):
    ///
    /// - [`Ordering::Unordered`] → `None` — the global claim (`available_at`
    ///   order); unchanged from today.
    /// - [`Ordering::Strict`] → `Some(flow_id)` — one constant stream for the
    ///   whole flow (per-flow so two strict flows never share a stream).
    /// - [`Ordering::Partitioned`] → the JMESPath result over `input`,
    ///   stringified. A scalar (string / number / bool) becomes the key. A
    ///   null / missing / non-scalar result **falls back to `flow_id`** rather
    ///   than `None`: a flow that opted into ordering must never have a run
    ///   silently escape to the unordered global claim (D20 blocking coherence
    ///   — a NULL key means unordered dispatch, which for a partitioned flow
    ///   would reorder its stream). Such runs share the flow-wide stream and
    ///   stay mutually ordered.
    pub fn partition_key_for(&self, flow_id: &str, input: &Value) -> Option<String> {
        match self {
            Ordering::Unordered => None,
            Ordering::Strict => Some(flow_id.to_string()),
            Ordering::Partitioned { partition_key } => {
                Some(eval_partition_key(partition_key, flow_id, input))
            }
        }
    }
}

/// Evaluate a `partitioned` key expression over the run input, folding the
/// result to a stream key string. A compile/eval failure or a null/missing/
/// non-scalar result degrades to `flow_id` (the flow-wide stream) — never an
/// escape to the unordered global claim. Syntactic validity is checked once at
/// flow-validation ([`crate::validate`]); this stays defensive for an
/// unvalidated flow.
fn eval_partition_key(expr: &str, flow_id: &str, input: &Value) -> String {
    let Ok(compiled) = jmespath::compile(expr) else {
        return flow_id.to_string();
    };
    let Ok(var) = compiled.search(input) else {
        return flow_id.to_string();
    };
    match serde_json::to_value(&var).ok() {
        Some(Value::String(s)) => s,
        // Number stringifies exactly (serde_json::Number preserves its text —
        // the platform's no-float rule holds); bool → "true"/"false".
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        // null / missing path / array / object → the flow-wide stream, never NULL.
        _ => flow_id.to_string(),
    }
}

/// The `partitioned(key)` head-unavailability policy (5.11 / D20): what a key
/// does while its earliest (head) run cannot dispatch. The stream order the
/// policy ranks by is `(enqueued_at, run_id)` — stamped once at enqueue, never
/// moved by a park/backoff (unlike `available_at`).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum PartitionPolicy {
    /// The default: an unavailable head still **blocks** its key — a
    /// backed-off or parked head holds later runs until it completes, and a
    /// head that exhausts its redelivery budget **wedges** the key (operator
    /// release; the janitor's `infrastructure-failure` verdict does not free
    /// it). The Kafka-consumer model: a partition never leapfrogs.
    #[default]
    Blocking,
    /// Opt-in: a later ready run may overtake an unavailable head (the
    /// `(available_at, run_id)` order among currently-ready siblings), and the
    /// janitor's verdict on an exhausted head releases the key. For keys where
    /// ordering is a throughput heuristic, not a correctness requirement.
    Leapfrog,
}

impl PartitionPolicy {
    /// `true` for the default ([`PartitionPolicy::Blocking`]) — used to omit
    /// the field on export so flows round-trip minimal.
    pub fn is_default(&self) -> bool {
        *self == PartitionPolicy::Blocking
    }
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

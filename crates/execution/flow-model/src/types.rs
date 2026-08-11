//! Canonical flow-graph types (5.1).
//!
//! A flow is **data, not code**: a versioned directed graph of typed nodes
//! wired by ported edges, starting at one typed entry node, and referencing
//! credentials by name. Ordinary node `type` values are open strings resolved
//! by the runner's node library (5.3).

use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use wamn_node_manifest::ConnectionTypeDescriptor;

use crate::canonical;
use crate::preimage::FlowPreimage;

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
pub const ENTRY_TYPES: [&str; 3] = ["request", "cron", "event"];

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
    /// row at enqueue (`wamn-run-state`) so the claim SQL is self-contained.
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
    /// The flow's node-level I/O capture policy (9.6): how each node's input /
    /// output payloads are persisted to `node_runs` — faithfully, scrubbed of
    /// secrets, preview-only, or not at all — plus the byte threshold above which
    /// any mode stores a preview only. Absent = [`Capture::default`] (full
    /// capture, 64 KiB threshold): the platform captures replayable payloads by
    /// default. Rides `graph_json`; the flowrunner guest applies it at every
    /// `node_runs` write ([`wamn_run_state::capture`](../wamn_run_state)).
    #[serde(default, skip_serializing_if = "Capture::is_default")]
    pub capture: Capture,
}

/// The default payload-size threshold (bytes) above which any capture mode
/// stores a preview only: 64 KiB. Tunable per flow via [`Capture::max_bytes`].
pub const DEFAULT_CAPTURE_MAX_BYTES: u64 = 64 * 1024;

/// Node-level I/O capture policy (9.6): how a flow's per-node input/output
/// payloads are persisted to `node_runs`, plus the size threshold that forces
/// preview-only storage. The pure application logic — secret scrubbing, size
/// truncation, and preview/size/hash derivation — lives in
/// `wamn_run_state::capture`; this is the authored declaration that rides
/// `graph_json` (the flow is in scope at every `node_runs` write site).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Capture {
    /// The capture mode. Absent = [`CaptureMode::Full`].
    #[serde(default)]
    pub mode: CaptureMode,
    /// Payloads whose serialization exceeds this many bytes are stored
    /// preview-only (head + size + hash, payload NULL) regardless of `mode`,
    /// recorded as `capture_mode = 'preview'`. Absent =
    /// [`DEFAULT_CAPTURE_MAX_BYTES`] (64 KiB).
    #[serde(default = "default_capture_max_bytes", rename = "max-bytes")]
    pub max_bytes: u64,
}

fn default_capture_max_bytes() -> u64 {
    DEFAULT_CAPTURE_MAX_BYTES
}

impl Default for Capture {
    fn default() -> Capture {
        Capture {
            mode: CaptureMode::Full,
            max_bytes: DEFAULT_CAPTURE_MAX_BYTES,
        }
    }
}

impl Capture {
    /// `true` for the default (full capture, 64 KiB threshold) — used to omit the
    /// field on export so flows round-trip minimal (like `partition_policy` /
    /// `ordering`).
    pub fn is_default(&self) -> bool {
        *self == Capture::default()
    }
}

/// How a node's I/O payloads are persisted (9.6), most → least captured. The
/// storage literal (`capture_mode`) is exactly the serde kebab-case name.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureMode {
    /// Payloads stored faithfully (replayable). Secret scrubbing is applied to
    /// the `preview_head` ONLY; the stored payload stays exact, `redacted` false.
    /// The default.
    #[default]
    Full,
    /// Secret scrubbing applied to the STORED payloads AND the preview
    /// (`redacted = true`). A replay still runs, but replays the scrubbed values
    /// — the documented capture/replay tradeoff (see `docs/archive/execution/run-state.md`).
    Scrubbed,
    /// Only the preview head + size + hash are stored; the payloads are NULL, so
    /// the run reconstructs to `CaptureOff` (non-replayable).
    Preview,
    /// Nothing is captured (`capture_mode = 'off'`); payloads NULL, no preview.
    Off,
}

impl CaptureMode {
    /// The `node_runs.capture_mode` storage literal — exactly the serde
    /// kebab-case name (tied to that by `capture_mode_literals_match_serde`).
    pub fn as_str(self) -> &'static str {
        match self {
            CaptureMode::Full => "full",
            CaptureMode::Scrubbed => "scrubbed",
            CaptureMode::Preview => "preview",
            CaptureMode::Off => "off",
        }
    }
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
/// needed. See `docs/archive/execution/run-queue.md` §Flow-level ordering declaration.
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
    /// Optional reference to a declared credential by [`CredentialRef::name`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
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
            "cron" => Some(EntryKind::Cron),
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
    Cron,
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

/// Configuration carried by the engine-reserved `invoke-flow` node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct InvokeFlowConfig {
    /// Tenant-scoped flow identifier resolved in the parent's pinned release.
    pub flow_id: String,
    /// Internal attachment used to authorize and resolve the child.
    pub attachment_id: String,
    /// Actor identity policy applied when the child is created.
    pub actor_mode: InvokeActorMode,
}

/// Actor identity modes accepted by an `invoke-flow` declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum InvokeActorMode {
    Inherit,
    Service,
    Attenuate,
}

/// Input synthesized for a `cron` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CronInput {
    /// The schedule anchor, as RFC 3339 UTC.
    pub scheduled_at: String,
    /// The actual firing time, as RFC 3339 UTC.
    pub fired_at: String,
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

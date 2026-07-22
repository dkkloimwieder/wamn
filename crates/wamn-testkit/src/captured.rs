//! The fact bundle a harness fills, then hands to [`evaluate`](crate::evaluate).
//!
//! [`Captured`] is the seam between the effect shell (the gate: a warm
//! `ServeNode` invocation, a `RunWorker` drain, admin-pool DB reads) and the
//! pure decision ([`evaluate`](crate::evaluate)). The harness runs the effects
//! and records their observable facts here; the evaluator never touches a wasm
//! instance, a clock, or a database — it only reads this struct.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use wamn_run_store::{FailKind, NodeErrorKind, RunStatus};

/// One recorded outbound request.
///
/// LIFTED here from `wamn_host::doubles::egress` (11.4) so a captured fact bundle
/// is serde-serializable and the pure evaluator can assert over egress WITHOUT a
/// host dependency. `wamn_host::doubles::egress` re-exports THIS type, so the
/// recorder API (`records()` / `denied()`) is unchanged for its callers — the
/// recorder produces the identical struct it always did, now with serde derives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct EgressRecord {
    /// The declaring flow/component (the store's workload id) — the egress
    /// assertion's `flow` key filters on this.
    pub workload_id: String,
    /// The request method (`GET`, `POST`, …).
    pub method: String,
    /// The target authority (`host[:port]`) — the allow/deny key.
    pub authority: String,
    /// The request path.
    pub path: String,
    /// Whether the recorder forwarded it (`true`) or denied it as unexpected
    /// (`false`).
    pub allowed: bool,
}

/// The captured result of ONE query a harness ran (via the admin pool, after
/// `scope_session`) so a [`DbState`](crate::Assertion::DbState) assertion can be
/// evaluated PURELY. The evaluator correlates an assertion to its capture by
/// `(query, params)`, then checks the [`DbExpect`](crate::DbExpect) against
/// `rows`. Each row is the query's single JSON column — the harness selects
/// `to_jsonb(t)` so a row is a plain object the matcher can read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct DbCapture {
    pub query: String,
    #[serde(default)]
    pub params: Vec<Value>,
    /// One JSON object per row (the query's `to_jsonb(t)` column).
    #[serde(default)]
    pub rows: Vec<Value>,
}

/// The run-level facts a flow-level harness captures from the runner: the run's
/// terminal status plus the failure classification (mirrors the persisted
/// `runs` columns). A [`RunOutcome`](crate::Assertion::RunOutcome) assertion
/// reads these.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RunFacts {
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_kind: Option<FailKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_node: Option<String>,
}

/// The pure fact bundle a harness fills. A node-level case fills
/// `node_output`/`node_port` or `node_error`; a flow-level case fills `run`,
/// `egress`, and `db`. Absent facts make the assertions that read them fail with
/// a "nothing captured" detail — never a false pass.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Captured {
    /// A node's success emission payload (parsed), if it emitted one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_output: Option<Value>,
    /// The node's emission port — the harness maps the absent (default) port to
    /// the literal `main`, so a `port` assertion is a plain equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_port: Option<String>,
    /// A node's classified error, if it returned one instead of an emission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_error: Option<NodeErrorKind>,
    /// The run-level facts (flow-level cases).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunFacts>,
    /// Every outbound request the flow attempted (the recorder's audit log).
    #[serde(default)]
    pub egress: Vec<EgressRecord>,
    /// The captured DB query results a `DbState` assertion reads.
    #[serde(default)]
    pub db: Vec<DbCapture>,
}

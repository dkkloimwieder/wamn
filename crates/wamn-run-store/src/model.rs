//! The `runs` / `node_runs` record model (5.7) — the durable, queryable shape of
//! one flow execution. These are the *logical* records: import/export as JSON for
//! the run-history read model, and the input reconstruction/partial-re-run read
//! from. The full DB rows carry additional reserved columns (the 5.10/9.6 seams
//! and the DB-managed timestamps); [`NodeRunRecord`] is the reconstruction view
//! of a node-run, not every column of `deploy/run-state.sql`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::status::{FailKind, NodeErrorKind, NodeRunStatus, RunStatus};

/// One row of `runs`: a single flow execution. A replay or partial re-run is a
/// NEW record (fresh `run_id`) linked to its origin via `replay_of` +
/// `root_run_id`, so the original run's record and node-runs stay immutable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RunRecord {
    pub run_id: String,
    pub flow_id: String,
    pub flow_version: u32,
    pub status: RunStatus,
    /// Where the run came from (a trigger label, `"replay"`, `"partial-rerun"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_source: Option<String>,
    /// The trigger payload — reconstruction and full replay seed the entry node
    /// with this. Absent only if capture was off (9.6), which makes the run
    /// non-replayable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// The run result (the last node's output) on completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// At-least-once dedupe key: a redelivered trigger with the same key collapses
    /// to one run. A replay mints a fresh key (it is a distinct execution).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// The run this one replays / re-runs from, if any (lineage parent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_of: Option<String>,
    /// The first original run of the replay chain (lineage root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_run_id: Option<String>,
    /// Why the run failed (mirrors the engine `FailKind`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_kind: Option<FailKind>,
    /// The node the run failed at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_node: Option<String>,
    /// The failure message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_reason: Option<String>,
}

impl RunRecord {
    /// A fresh running run with the given trigger input.
    pub fn new(
        run_id: impl Into<String>,
        flow_id: impl Into<String>,
        flow_version: u32,
        input: Value,
    ) -> RunRecord {
        RunRecord {
            run_id: run_id.into(),
            flow_id: flow_id.into(),
            flow_version,
            status: RunStatus::Running,
            trigger_source: None,
            input: Some(input),
            result: None,
            idempotency_key: None,
            replay_of: None,
            root_run_id: None,
            fail_kind: None,
            fail_node: None,
            fail_reason: None,
        }
    }

    /// The lineage root of this run: its `root_run_id`, else itself (an original
    /// run is its own root).
    pub fn lineage_root(&self) -> &str {
        self.root_run_id.as_deref().unwrap_or(&self.run_id)
    }
}

/// One row of `node_runs`: a single node execution — the branch-aware
/// reconstruction source. A node the flow LOOPS through has one row per visit,
/// disambiguated by `occurrence`; retries of one occurrence share the row and
/// bump `attempt`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct NodeRunRecord {
    pub run_id: String,
    pub node_id: String,
    /// Which visit of this node (0 = the first); the loop-safe part of the
    /// idempotency key `(run_id, node_id, occurrence)`.
    #[serde(default)]
    pub occurrence: u32,
    /// Dispatch order within the run — reconstruction replays completed rows by
    /// this.
    pub seq: u32,
    /// Retry count of this occurrence (retries share the row).
    #[serde(default)]
    pub attempt: u32,
    pub status: NodeRunStatus,
    /// The port the node emitted on (`main`, a branch port, or `error`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_port: Option<String>,
    /// The payload the node emitted — reconstruction folds this as the emission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// The node's input payload — a partial re-run seeds the node with this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// Classified failure kind, for run history (reconstruction keys off the
    /// recorded emission port, not this).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<NodeErrorKind>,
    /// Structured error detail for history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<Value>,
}

impl NodeRunRecord {
    /// A completed successful node-run emitting `payload` on `port`.
    pub fn success(
        run_id: impl Into<String>,
        node_id: impl Into<String>,
        seq: u32,
        port: impl Into<String>,
        payload: Value,
    ) -> NodeRunRecord {
        NodeRunRecord {
            run_id: run_id.into(),
            node_id: node_id.into(),
            occurrence: 0,
            seq,
            attempt: 0,
            status: NodeRunStatus::Success,
            output_port: Some(port.into()),
            output: Some(payload),
            input: None,
            error_kind: None,
            error_detail: None,
        }
    }
}

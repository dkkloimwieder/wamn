//! Replay & partial re-run planning (5.7). Both mint a NEW run linked to its
//! origin (`replay_of` + `root_run_id`) so the original stays immutable:
//!
//! - **replay** re-runs the whole flow from the captured trigger input — the
//!   driver `Plan::start`s the new run and walks from `entry`.
//! - **partial re-run** re-enters a chosen node with ITS captured input — the
//!   driver [`Plan::seed_at`](wamn_runner::Plan::seed_at)s the new run there and
//!   walks only the downstream subtree; upstream, already-committed effects are
//!   not re-fired.
//!
//! These planners are pure: they select the input and build the lineage-linked
//! [`RunRecord`]; the caller mints the fresh `run_id` (the store has no RNG) and
//! performs the walk + persistence.

use serde_json::Value;

use crate::model::{NodeRunRecord, RunRecord};
use crate::status::RunStatus;

/// Why a replay / partial re-run could not be planned.
#[derive(Debug, Clone, PartialEq)]
pub enum RerunError {
    /// The chosen node has no recorded execution in the original run (it never
    /// ran, or the requested `occurrence` does not exist).
    NoSuchNodeRun { node: String, occurrence: u32 },
    /// The needed input payload was not captured (9.6 capture off), so the run
    /// cannot be seeded. `node` is the seed node, or `"(trigger)"` for a replay.
    InputNotCaptured { node: String },
}

impl std::fmt::Display for RerunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RerunError::NoSuchNodeRun { node, occurrence } => write!(
                f,
                "no recorded execution of node {node:?} (occurrence {occurrence}) to re-run from"
            ),
            RerunError::InputNotCaptured { node } => {
                write!(f, "input for {node:?} was not captured — cannot re-run")
            }
        }
    }
}

impl std::error::Error for RerunError {}

/// A planned partial re-run: the new lineage-linked run and the node + payload
/// the driver hands to [`Plan::seed_at`](wamn_runner::Plan::seed_at).
#[derive(Debug, Clone, PartialEq)]
pub struct PartialRerun {
    pub run: RunRecord,
    pub seed_node: String,
    pub seed_input: Value,
}

/// Build a new [`RunRecord`] linked to `original`, carrying `input`, marked
/// running, with the given `trigger_source`. Shared by replay + partial re-run.
fn lineage_run(
    original: &RunRecord,
    new_run_id: String,
    input: Value,
    trigger_source: &str,
) -> RunRecord {
    RunRecord {
        run_id: new_run_id,
        flow_id: original.flow_id.clone(),
        flow_version: original.flow_version,
        status: RunStatus::Running,
        trigger_source: Some(trigger_source.to_string()),
        input: Some(input),
        result: None,
        // A replay is a distinct execution: fresh idempotency key (none here; the
        // driver/queue may mint one), lineage via replay_of/root_run_id.
        idempotency_key: None,
        replay_of: Some(original.run_id.clone()),
        root_run_id: Some(original.lineage_root().to_string()),
        fail_kind: None,
        fail_node: None,
        fail_reason: None,
    }
}

/// Plan a full replay: a new run re-running the whole flow from `original`'s
/// captured trigger input. `new_run_id` is the fresh id the caller minted. The
/// driver `Plan::start`s the returned record's input and walks from `entry`.
pub fn plan_replay(
    original: &RunRecord,
    new_run_id: impl Into<String>,
) -> Result<RunRecord, RerunError> {
    let input = original
        .input
        .clone()
        .ok_or_else(|| RerunError::InputNotCaptured {
            node: "(trigger)".to_string(),
        })?;
    Ok(lineage_run(original, new_run_id.into(), input, "replay"))
}

/// Plan a partial re-run re-entering `from_node` (visit `occurrence`) with its
/// captured input. `new_run_id` is the fresh id the caller minted. The driver
/// [`Plan::seed_at`](wamn_runner::Plan::seed_at)s the returned record at
/// `seed_node`/`seed_input` and walks the downstream subtree.
pub fn plan_partial_rerun(
    original: &RunRecord,
    node_runs: &[NodeRunRecord],
    from_node: &str,
    occurrence: u32,
    new_run_id: impl Into<String>,
) -> Result<PartialRerun, RerunError> {
    let nr = node_runs
        .iter()
        .find(|nr| nr.node_id == from_node && nr.occurrence == occurrence)
        .ok_or_else(|| RerunError::NoSuchNodeRun {
            node: from_node.to_string(),
            occurrence,
        })?;
    let seed_input = nr
        .input
        .clone()
        .ok_or_else(|| RerunError::InputNotCaptured {
            node: from_node.to_string(),
        })?;
    let run = lineage_run(
        original,
        new_run_id.into(),
        seed_input.clone(),
        "partial-rerun",
    );
    Ok(PartialRerun {
        run,
        seed_node: from_node.to_string(),
        seed_input,
    })
}

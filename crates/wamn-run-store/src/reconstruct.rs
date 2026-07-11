//! Branch-aware replay: rebuild a run's in-memory [`RunState`] from its persisted
//! `node_runs`. This is the durable resume path (5.7) that supersedes the S3
//! linear `step_seq` — it replays the run's completed emissions through the pure
//! engine ([`Plan::resume`]) so the reconstructed frontier is exactly what the
//! original walk left outstanding: the same branch, the same merges, error-routed
//! nodes back on their error branch.

use wamn_runner::{MAIN_PORT, Plan, Recorded, ResumeError, RunState};

use crate::model::{NodeRunRecord, RunRecord};

/// Why a run could not be reconstructed from its persisted rows.
#[derive(Debug, Clone, PartialEq)]
pub enum ReconstructError {
    /// A completed node-run has no captured emission payload — capture was off
    /// (9.6) for this run, so it cannot be replayed. Carries the node id.
    CaptureOff { node: String },
    /// The engine could not fold the recorded steps (drift / overrun / a
    /// mid-reconstruction wait). Wraps the engine's [`ResumeError`].
    Resume(ResumeError),
}

impl std::fmt::Display for ReconstructError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReconstructError::CaptureOff { node } => write!(
                f,
                "node {node:?} has no captured output — run is not replayable (capture off)"
            ),
            ReconstructError::Resume(e) => write!(f, "reconstruction failed: {e}"),
        }
    }
}

impl std::error::Error for ReconstructError {}

impl From<ResumeError> for ReconstructError {
    fn from(e: ResumeError) -> ReconstructError {
        ReconstructError::Resume(e)
    }
}

/// Rebuild the [`RunState`] for `run` from its persisted `node_runs`, branch-aware.
///
/// Only COMPLETED node-runs (`success`/`error`) are replayed, in `seq` order;
/// a `running`/`parked` row is an outstanding node the driver re-dispatches
/// (its effect runs at-least-once, deduped by the node's own idempotency). The
/// run's `input` seeds the entry node. The returned state is positioned to
/// continue — the driver calls `next`/`apply` from there.
pub fn reconstruct(
    plan: &Plan,
    run: &RunRecord,
    node_runs: &[NodeRunRecord],
) -> Result<RunState, ReconstructError> {
    let mut completed: Vec<&NodeRunRecord> = node_runs
        .iter()
        .filter(|nr| nr.status.is_completed())
        .collect();
    completed.sort_by_key(|nr| nr.seq);

    let mut recorded = Vec::with_capacity(completed.len());
    for nr in completed {
        // A completed row must carry its emission to be replayable. An error-routed
        // node carries the `{"error": …}` payload; a success carries its output.
        // Absent => capture was off (9.6) => the run cannot be reconstructed.
        let payload = nr
            .output
            .clone()
            .ok_or_else(|| ReconstructError::CaptureOff {
                node: nr.node_id.clone(),
            })?;
        let port = nr
            .output_port
            .clone()
            .unwrap_or_else(|| MAIN_PORT.to_string());
        recorded.push(Recorded::new(nr.node_id.clone(), port, payload));
    }

    let input = run.input.clone().unwrap_or(serde_json::Value::Null);
    Ok(plan.resume(run.run_id.clone(), input, &recorded)?)
}

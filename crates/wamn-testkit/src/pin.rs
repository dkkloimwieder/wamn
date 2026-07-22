//! Pin a recorded run as a test case (11.3): the PURE transform from a stored
//! run (`wamn_run_store` `RunRecord` + its `node_runs`) to a canonical
//! [`TestCase`]. No DB, no clock — the effect shell (`wamn-ctl pin-run`) READS
//! the rows and WRITES the produced case; this decides what the case is.
//!
//! The dependency direction is deliberate: this reads STORE records and writes a
//! testkit [`TestCase`], so it lives in testkit (which already depends on
//! `wamn-run-store`). Putting it in `wamn-run-store` would force
//! run-store → testkit, a cycle.
//!
//! ## What a pinned case is (the minimal-correct v0 shape)
//!
//! A flow-level case:
//! - `flow-ref` = the run's `(flow_id, flow_version)`;
//! - `input` = the run's trigger input (SCRUBBED — see below);
//! - `expect` = a [`RunOutcome`](crate::Assertion::RunOutcome) (the run's terminal
//!   status/fail-kind/fail-node) PLUS, when the run recorded a replayable terminal
//!   node, an [`Equals`](crate::Assertion::Equals) over that node's emission
//!   (the reconstruction-relevant payload — where volatile ids live);
//! - `normalize` = `canonicalize` on + any caller `ignore-paths`, so replay
//!   tolerates a minted id/timestamp in the pinned node output.
//!
//! 9.6 capture persists NODE I/O only; egress and DB state are filled by the LIVE
//! testkitbench harness, not `node_runs`, so an `Egress`/`DbState` assertion
//! cannot be pinned from stored history in v0. `Captured::node_output` is a single
//! value (no whole-run node map), so a multi-node run pins the FLOW outcome plus
//! its TERMINAL node output — not a per-node map. Both are deliberate v0 scoping.
//!
//! ## Secret redaction at pin time
//!
//! Every payload that becomes part of the case — the trigger input and the pinned
//! node output — is passed through [`wamn_run_store::capture::scrub`] first, so a
//! pinned case NEVER contains a secret even from a `full`-capture run (where the
//! stored `node_runs` payloads are faithful). Scrub is idempotent, so an
//! already-`scrubbed` row is safe to re-scrub.

use serde_json::Value;

use wamn_run_store::capture::scrub;
use wamn_run_store::{NodeRunRecord, RunRecord};

use crate::normalize::Normalize;
use crate::{Assertion, FlowRef, SCHEMA_VERSION, TestCase};

/// Why a run cannot be pinned. Enum (the repo's WIT-mirroring house style) so the
/// verb can name the exact defect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinError {
    /// The run's terminal node has no captured output — capture was `off` /
    /// `preview` for this run, so it is not replayable and cannot be pinned.
    /// Carries the node id. Mirrors
    /// [`ReconstructError::CaptureOff`](wamn_run_store::ReconstructError::CaptureOff).
    NotCaptured { node: String },
}

impl std::fmt::Display for PinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PinError::NotCaptured { node } => write!(
                f,
                "node {node:?} has no captured output — run is not replayable (capture off/preview), cannot pin"
            ),
        }
    }
}

impl std::error::Error for PinError {}

/// How to pin: the case name and any extra volatile fields to drop.
#[derive(Debug, Clone, Default)]
pub struct PinOptions {
    /// The name the produced [`TestCase`] carries (the case id).
    pub case_id: String,
    /// Extra RFC-6901 pointers into the pinned node output to drop as volatile
    /// (beyond the UUID/timestamp canonicalization pin turns on by default).
    pub ignore_paths: Vec<String>,
}

/// Fold a recorded run and its completed `node_runs` into a [`TestCase`]. Pure —
/// the caller supplies the rows. See the module docs for the pinned shape.
///
/// Returns [`PinError::NotCaptured`] when the run's terminal completed node has no
/// stored output (capture off/preview) — a non-replayable run is not pinnable.
pub fn pin_run(
    run: &RunRecord,
    node_runs: &[NodeRunRecord],
    opts: &PinOptions,
) -> Result<TestCase, PinError> {
    // The flow-level outcome assertion — always present.
    let mut expect = vec![Assertion::RunOutcome {
        status: run.status,
        fail_kind: run.fail_kind,
        fail_node: run.fail_node.clone(),
    }];

    // The TERMINAL completed node (highest dispatch seq among success/error rows)
    // carries the run's reconstruction-relevant emission — pin an Equals over its
    // SCRUBBED output so replay exercises the node payload. Absent output means
    // capture was off/preview: refuse (mirror of reconstruction's CaptureOff).
    let terminal = node_runs
        .iter()
        .filter(|nr| nr.status.is_completed())
        .max_by_key(|nr| nr.seq);

    let mut normalize = None;
    if let Some(nr) = terminal {
        let Some(mut output) = nr.output.clone() else {
            return Err(PinError::NotCaptured {
                node: nr.node_id.clone(),
            });
        };
        scrub(&mut output);
        expect.push(Assertion::Equals(output));
        // A node-output assertion is present, so carry normalization: canonicalize
        // UUID/timestamp leaves + drop the caller's volatile pointers.
        normalize = Some(Normalize {
            ignore_paths: opts.ignore_paths.clone(),
            canonicalize: true,
        });
    }

    // The trigger input rides the case — SCRUBBED, so a full-capture run's secret
    // trigger never lands in a pinned case.
    let mut input = run.input.clone().unwrap_or(Value::Null);
    scrub(&mut input);

    Ok(TestCase {
        schema_version: SCHEMA_VERSION.to_string(),
        name: opts.case_id.clone(),
        flow_ref: Some(FlowRef {
            flow_id: run.flow_id.clone(),
            version: run.flow_version,
        }),
        node_ref: None,
        input,
        config: None,
        ctx: None,
        expect,
        normalize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Captured, RunFacts, evaluate};
    use serde_json::json;
    use wamn_run_store::{FailKind, NodeRunRecord, RunRecord, RunStatus};

    /// A completed run of `flow` v1 with the given trigger input.
    fn completed_run(input: Value) -> RunRecord {
        let mut run = RunRecord::new("run-1", "flow", 1, input);
        run.status = RunStatus::Completed;
        run
    }

    /// One terminal success node emitting `output` at seq 0.
    fn node(output: Option<Value>) -> NodeRunRecord {
        let mut nr = NodeRunRecord::success("run-1", "final", 0, "main", Value::Null);
        nr.output = output;
        nr
    }

    fn opts() -> PinOptions {
        PinOptions {
            case_id: "pinned".into(),
            ignore_paths: vec![],
        }
    }

    /// MUTANT #1 (skip scrub at pin): a FULL-capture run whose stored node output
    /// carries a raw secret must yield a case with NO secret — the pinned Equals
    /// value is redacted. And the trigger input is scrubbed too.
    #[test]
    fn pin_full_run_scrubs_secrets() {
        let run = completed_run(json!({"trigger": "go", "api_key": "sekret-IN"}));
        let nodes = [node(Some(json!({"result": "ok", "token": "sekret-OUT"})))];
        let case = pin_run(&run, &nodes, &opts()).expect("pins");

        let wire = serde_json::to_string(&case).unwrap();
        assert!(
            !wire.contains("sekret-OUT"),
            "node output secret leaked: {wire}"
        );
        assert!(
            !wire.contains("sekret-IN"),
            "trigger input secret leaked: {wire}"
        );
        assert!(
            wire.contains("[redacted]"),
            "scrub placeholder absent: {wire}"
        );
        // The pinned Equals still carries the non-secret field.
        let equals = case
            .expect
            .iter()
            .find_map(|a| match a {
                Assertion::Equals(v) => Some(v),
                _ => None,
            })
            .expect("an Equals assertion");
        assert_eq!(equals["result"], json!("ok"));
        assert_eq!(equals["token"], json!("[redacted]"));
    }

    /// MUTANT #2 (treat None output as replayable): a preview/off run — the
    /// terminal completed node has NULL output — must be REFUSED, not pinned.
    #[test]
    fn pin_preview_run_is_refused() {
        let run = completed_run(json!({"trigger": "go"}));
        let nodes = [node(None)];
        assert_eq!(
            pin_run(&run, &nodes, &opts()),
            Err(PinError::NotCaptured {
                node: "final".into()
            })
        );
    }

    /// The pinned case carries the run outcome and turns canonicalization on so
    /// replay tolerates a volatile id.
    #[test]
    fn pinned_case_shape_is_flow_outcome_plus_terminal_output() {
        let mut run = completed_run(json!({"trigger": "go"}));
        run.status = RunStatus::Failed;
        run.fail_kind = Some(FailKind::Terminal);
        run.fail_node = Some("final".into());
        let nodes = [node(Some(json!({"error": {"code": "x"}})))];
        let case = pin_run(&run, &nodes, &opts()).expect("pins");

        assert!(case.flow_ref.is_some());
        assert!(case.node_ref.is_none(), "a pinned run is a flow-level case");
        assert!(case.normalize.as_ref().unwrap().canonicalize);
        assert!(matches!(case.expect[0], Assertion::RunOutcome { .. }));
        assert!(matches!(case.expect[1], Assertion::Equals(_)));
    }

    /// MUTANT #3 (normalize does nothing / over-removes): the pure round-trip. A
    /// pinned case, replayed against a rebuilt Captured, PASSES; a mutated
    /// VOLATILE field still PASSES (canonicalization collapses it); a mutated REAL
    /// field FAILS.
    #[test]
    fn replay_round_trip_tolerates_volatile_but_rejects_real() {
        let output = json!({
            "result": "accepted",
            "run_uuid": "550e8400-e29b-41d4-a716-446655440000",
            "at": "2026-07-22T06:59:00Z",
        });
        let run = completed_run(json!({"trigger": "go"}));
        let nodes = [node(Some(output.clone()))];
        let case = pin_run(&run, &nodes, &opts()).expect("pins");

        let facts = || {
            Some(RunFacts {
                status: RunStatus::Completed,
                fail_kind: None,
                fail_node: None,
            })
        };
        let replay = |node_output: Value| Captured {
            run: facts(),
            node_output: Some(node_output),
            ..Default::default()
        };

        // Faithful replay of the recorded facts PASSES.
        assert!(
            evaluate(&case, &replay(output.clone())).passed(),
            "faithful replay"
        );

        // A DIFFERENT uuid + timestamp (the volatile fields) still PASSES.
        let mut volatile = output.clone();
        volatile["run_uuid"] = json!("11111111-2222-3333-4444-555555555555");
        volatile["at"] = json!("2020-01-01T00:00:00Z");
        assert!(
            evaluate(&case, &replay(volatile)).passed(),
            "a mutated volatile field must still pass"
        );

        // A mutated REAL field FAILS.
        let mut real = output;
        real["result"] = json!("rejected");
        assert!(
            !evaluate(&case, &replay(real)).passed(),
            "a mutated real field must fail"
        );
    }

    /// A run with no completed node runs pins a flow-level RunOutcome-only case
    /// (no node assertion, no normalization) — still valid.
    #[test]
    fn pin_run_without_nodes_is_outcome_only() {
        let run = completed_run(json!({"trigger": "go"}));
        let case = pin_run(&run, &[], &opts()).expect("pins");
        assert_eq!(case.expect.len(), 1);
        assert!(matches!(case.expect[0], Assertion::RunOutcome { .. }));
        assert!(case.normalize.is_none());
    }
}

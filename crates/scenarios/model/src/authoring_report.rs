use serde::{Deserialize, Serialize};

use crate::{FailKind, Outcome, RunStatus, ScenarioRefusal};

/// Immutable execution lineage exposed by the author-facing report query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case"
)]
pub enum ExecutionLineage {
    /// Execution used an unpublished, content-addressed flow draft.
    Draft {
        /// Ordinary exact artifact hash, including the proposed publish/runtime version.
        draft_artifact_hash: String,
        runtime_flow_version: i32,
        execution_bundle_hash: String,
        validated_draft_hash: String,
        catalog_id: String,
        catalog_version: i32,
        environment: String,
    },
    /// Execution used a versioned member of an immutable release.
    Release {
        artifact_hash: String,
        catalog_id: String,
        catalog_version: i32,
        environment: String,
    },
}

/// One case result in a durable author-facing suite report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AuthoringCaseReport {
    pub case_id: String,
    pub run_id: String,
    pub passed: bool,
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_kind: Option<FailKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_node: Option<String>,
    pub outcome: Outcome,
}

impl AuthoringCaseReport {
    /// Construct a case report whose summary respects its evidence and lifecycle.
    ///
    /// An effect-uncertain run always fails the case summary, while retaining the
    /// exact assertion outcome as evidence.
    pub fn new(
        case_id: impl Into<String>,
        run_id: impl Into<String>,
        status: RunStatus,
        fail_kind: Option<FailKind>,
        fail_node: Option<String>,
        outcome: Outcome,
    ) -> Self {
        let passed = status != RunStatus::EffectUncertain && outcome.passed();
        Self {
            case_id: case_id.into(),
            run_id: run_id.into(),
            passed,
            status,
            fail_kind,
            fail_node,
            outcome,
        }
    }
}

/// Durable read model returned after a stored suite runs from an authoring surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AuthoringReport {
    pub report_id: String,
    pub execution_id: String,
    pub tenant_id: String,
    pub flow_id: String,
    /// The released version that owns the stored suite data; not draft lineage.
    pub suite_flow_version: i32,
    pub suite_id: String,
    pub passed: bool,
    pub lineage: ExecutionLineage,
    /// Milliseconds from the persisted draft edit to the first admitted case.
    /// Absent when execution refused before any case was admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_to_run_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<ScenarioRefusal>,
    pub cases: Vec<AuthoringCaseReport>,
}

/// Why an immutable authoring-report reservation has not finalized.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PendingAuthoringReportReason {
    /// No durable run exists for any case that does not yet have a captured fact.
    AwaitingAdmission,
    /// At least one deterministic run exists without its immutable captured fact.
    ///
    /// Retrying this report identity must return this state; it must never rerun,
    /// resume, fabricate a fact for, or finalize any listed run.
    CaptureInterrupted { run_ids: Vec<String> },
}

/// Durable read model for a report reservation that cannot yet finalize.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PendingAuthoringReport {
    pub report_id: String,
    pub execution_id: String,
    pub tenant_id: String,
    pub flow_id: String,
    pub suite_flow_version: i32,
    pub suite_id: String,
    pub lineage: ExecutionLineage,
    pub reason: PendingAuthoringReportReason,
    /// Immutable case facts captured before the pending boundary, in suite order.
    pub captured_cases: Vec<AuthoringCaseReport>,
}

/// Typed state returned by the read-only report query.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "report", rename_all = "kebab-case")]
pub enum AuthoringReportState {
    NotFound,
    Pending(PendingAuthoringReport),
    Finalized(AuthoringReport),
}

/// Result of executing (or exactly retrying) one reserved authoring command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "report", rename_all = "kebab-case")]
pub enum AuthoringExecutionResult {
    /// An exact retry observed an unfinished command and performed no new work.
    Pending(PendingAuthoringReport),
    Finalized(AuthoringReport),
}

impl AuthoringReport {
    /// Construct a report whose suite summary is derived from its cases/refusal.
    #[expect(
        clippy::too_many_arguments,
        reason = "the durable report identity, selection, lineage, and timing are all explicit"
    )]
    pub fn new(
        report_id: impl Into<String>,
        execution_id: impl Into<String>,
        tenant_id: impl Into<String>,
        flow_id: impl Into<String>,
        suite_flow_version: i32,
        suite_id: impl Into<String>,
        lineage: ExecutionLineage,
        edit_to_run_ms: Option<u64>,
        refusal: Option<ScenarioRefusal>,
        cases: Vec<AuthoringCaseReport>,
    ) -> Self {
        // Stored suites deliberately permit zero cases. With no refusal and no
        // failed assertion, that suite is a successful no-op; every refusal is
        // a failed execution regardless of how many cases were admitted.
        let passed = refusal.is_none() && cases.iter().all(|case| case.passed);
        Self {
            report_id: report_id.into(),
            execution_id: execution_id.into(),
            tenant_id: tenant_id.into(),
            flow_id: flow_id.into(),
            suite_flow_version,
            suite_id: suite_id.into(),
            passed,
            lineage,
            edit_to_run_ms,
            refusal,
            cases,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn outcome(passed: bool) -> Outcome {
        Outcome {
            name: "case-a".into(),
            results: vec![crate::AssertionResult {
                assertion: crate::Assertion::Equals(json!({"ok": true})),
                passed,
                detail: (!passed).then(|| "mismatch".into()),
            }],
        }
    }

    #[test]
    fn draft_and_release_lineage_have_disjoint_wire_shapes() {
        let draft = serde_json::to_value(ExecutionLineage::Draft {
            draft_artifact_hash: "sha256:artifact".into(),
            runtime_flow_version: 8,
            execution_bundle_hash: "sha256:bundle".into(),
            validated_draft_hash: "sha256:validated".into(),
            catalog_id: "catalog-a".into(),
            catalog_version: 4,
            environment: "dev".into(),
        })
        .unwrap();
        let release = serde_json::to_value(ExecutionLineage::Release {
            artifact_hash: "sha256:release".into(),
            catalog_id: "catalog-a".into(),
            catalog_version: 4,
            environment: "dev".into(),
        })
        .unwrap();

        assert_eq!(draft["kind"], "draft");
        assert!(draft.get("draft-artifact-hash").is_some());
        assert!(draft.get("artifact-hash").is_none());
        assert!(draft.get("runtime-flow-version").is_some());
        assert!(draft.get("execution-bundle-hash").is_some());
        assert!(draft.get("validated-draft-hash").is_some());
        assert_eq!(release["kind"], "release");
        assert!(release.get("artifact-hash").is_some());
        assert!(release.get("draft-artifact-hash").is_none());
        assert!(release.get("runtime-flow-version").is_none());
        assert!(release.get("execution-bundle-hash").is_none());
    }

    #[test]
    fn report_summaries_are_derived_from_case_outcomes() {
        let failed = AuthoringCaseReport::new(
            "case-a",
            "run-a",
            RunStatus::Failed,
            Some(FailKind::Terminal),
            Some("write".into()),
            outcome(false),
        );
        let report = AuthoringReport::new(
            "report-a",
            "exec-a",
            "tenant-a",
            "flow-a",
            3,
            "suite-a",
            ExecutionLineage::Draft {
                draft_artifact_hash: "sha256:artifact".into(),
                runtime_flow_version: 8,
                execution_bundle_hash: "sha256:bundle".into(),
                validated_draft_hash: "sha256:validated".into(),
                catalog_id: "catalog-a".into(),
                catalog_version: 4,
                environment: "dev".into(),
            },
            Some(27),
            None,
            vec![failed],
        );

        assert!(!report.passed);
        assert!(!report.cases[0].passed);
        assert_eq!(report.edit_to_run_ms, Some(27));
    }

    #[test]
    fn effect_uncertain_fails_case_and_report_without_rewriting_passing_evidence() {
        let case = AuthoringCaseReport::new(
            "case-a",
            "run-a",
            RunStatus::EffectUncertain,
            None,
            None,
            outcome(true),
        );
        assert!(case.outcome.passed());
        assert!(!case.passed);

        let report = AuthoringReport::new(
            "report-a",
            "exec-a",
            "tenant-a",
            "flow-a",
            3,
            "suite-a",
            ExecutionLineage::Release {
                artifact_hash: "sha256:artifact".into(),
                catalog_id: "catalog-a".into(),
                catalog_version: 4,
                environment: "dev".into(),
            },
            None,
            None,
            vec![case],
        );
        assert!(!report.passed);
    }

    #[test]
    fn refusal_always_fails_and_an_unrefused_empty_suite_is_a_successful_no_op() {
        let lineage = || ExecutionLineage::Draft {
            draft_artifact_hash: "sha256:artifact".into(),
            runtime_flow_version: 8,
            execution_bundle_hash: "sha256:bundle".into(),
            validated_draft_hash: "sha256:validated".into(),
            catalog_id: "catalog-a".into(),
            catalog_version: 4,
            environment: "dev".into(),
        };
        let refused = AuthoringReport::new(
            "report-refused",
            "exec-refused",
            "tenant-a",
            "flow-a",
            7,
            "suite-a",
            lineage(),
            None,
            Some(ScenarioRefusal::UndrivableNodes {
                node_types: vec!["custom-node".into()],
            }),
            Vec::new(),
        );
        let empty_success = AuthoringReport::new(
            "report-empty",
            "exec-empty",
            "tenant-a",
            "flow-a",
            7,
            "suite-a",
            lineage(),
            None,
            None,
            Vec::new(),
        );

        assert!(!refused.passed, "a refusal cannot become a passing report");
        assert!(
            empty_success.passed,
            "the suite model admits a zero-case no-op"
        );
    }

    #[test]
    fn capture_interrupted_is_an_explicit_non_final_read_state() {
        let pending = AuthoringReportState::Pending(PendingAuthoringReport {
            report_id: "report-a".into(),
            execution_id: "exec-a".into(),
            tenant_id: "tenant-a".into(),
            flow_id: "flow-a".into(),
            suite_flow_version: 7,
            suite_id: "suite-a".into(),
            lineage: ExecutionLineage::Draft {
                draft_artifact_hash: "sha256:artifact".into(),
                runtime_flow_version: 8,
                execution_bundle_hash: "sha256:bundle".into(),
                validated_draft_hash: "sha256:validated".into(),
                catalog_id: "catalog-a".into(),
                catalog_version: 4,
                environment: "dev".into(),
            },
            reason: PendingAuthoringReportReason::CaptureInterrupted {
                run_ids: vec!["scenario-exec-a-1".into()],
            },
            captured_cases: Vec::new(),
        });

        let value = serde_json::to_value(pending).unwrap();
        assert_eq!(value["state"], "pending");
        assert_eq!(value["report"]["reason"]["kind"], "capture-interrupted");
        assert!(value["report"].get("passed").is_none());
    }
}

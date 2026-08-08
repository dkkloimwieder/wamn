//! Public suite-projection view over the internal durable report state.
//!
//! `authoring.rs` owns the durable report; `wamn-authoring-model` owns the
//! frontend-neutral document a client reads. This module is the single
//! translation between them, so no transport builds a projection of its own and
//! no storage type reaches a client. It is data-only and performs no I/O: a
//! caller resolves the exact draft revision behind a validated draft and hands
//! it in, because the durable report retains the executable pins but not the
//! mutable `(draft-id, revision)` the draft was saved under.
//!
//! Two published projection fields have no evidence source in the current
//! durable report and are deliberately not fabricated here:
//! `DraftSuiteProjection`'s node/branch/edge coverage arrays (owned by
//! `wamn-ma5`) and `SuiteExecutionRefusal::DraftConnectionsDenied`'s connection
//! names (not retained by `ScenarioRefusal`). Both are emitted empty, which the
//! authoring-surface contract reads as an exhaustive enumeration; mounting the
//! `suite-projection` route before those sources land would publish an empty
//! enumeration as if it were complete.

use std::fmt;

use wamn_authoring_model::{
    CaseResultProjection, CatalogIdentity, DraftIdentity, DraftSuiteProjection, FailureDetail,
    FailureKind, PassFail, PendingReportReason, PendingSuiteProjection, SCHEMA_VERSION,
    SafeIntegerError, SafeUint64, SuiteExecutionRefusal, SuiteOutcome, SuiteProjectionState,
    SuiteRef, ValidatedDraftIdentity, ValidatedDraftRef,
};
use wamn_scenario_model::{
    AuthoringCaseReport, AuthoringReport, AuthoringReportState, ExecutionLineage, FailKind,
    PendingAuthoringReport, PendingAuthoringReportReason, RunStatus, ScenarioRefusal,
};

/// The exact mutable draft revision one durable report was produced from.
///
/// The report pins the executable, not the editable document, so this is the
/// one fact a caller must resolve before a projection can name the revision a
/// client saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectedDraftRevision<'a> {
    pub draft_id: &'a str,
    pub revision: u64,
}

/// Return the opaque validated-draft handle one report state was produced from.
///
/// `None` means the state carries no draft lineage — a missing report, or a
/// report for an immutable release rather than a draft. A caller uses this to
/// decide whether it must resolve a [`ProjectedDraftRevision`] at all.
pub fn validated_draft_id(state: &AuthoringReportState) -> Option<&str> {
    let lineage = match state {
        AuthoringReportState::NotFound => return None,
        AuthoringReportState::Pending(pending) => &pending.lineage,
        AuthoringReportState::Finalized(report) => &report.lineage,
    };
    draft_handle(lineage)
}

/// Project one durable report state onto the public suite-projection document.
///
/// `draft` is required exactly when [`validated_draft_id`] returned `Some`; a
/// missing report ignores it. The projection is a pure view: it never runs,
/// resumes, finalizes, or fabricates evidence, and a pending reservation stays
/// pending rather than becoming a refusal.
pub fn suite_projection(
    state: &AuthoringReportState,
    draft: Option<ProjectedDraftRevision<'_>>,
) -> Result<SuiteProjectionState, ProjectionError> {
    match state {
        AuthoringReportState::NotFound => Ok(SuiteProjectionState::NotFound),
        AuthoringReportState::Pending(pending) => {
            Ok(SuiteProjectionState::Pending(pending_projection(pending)?))
        }
        AuthoringReportState::Finalized(report) => Ok(SuiteProjectionState::Finalized(Box::new(
            finalized_projection(report, draft)?,
        ))),
    }
}

fn pending_projection(
    pending: &PendingAuthoringReport,
) -> Result<PendingSuiteProjection, ProjectionError> {
    let validated_draft_id = draft_handle(&pending.lineage)
        .ok_or_else(|| error(&pending.report_id, ProjectionErrorKind::ReleaseLineage))?;
    Ok(PendingSuiteProjection {
        report_id: pending.report_id.clone(),
        execution_id: pending.execution_id.clone(),
        validated_draft: ValidatedDraftRef {
            validated_draft_id: validated_draft_id.to_owned(),
        },
        reason: match &pending.reason {
            PendingAuthoringReportReason::AwaitingAdmission => {
                PendingReportReason::AwaitingAdmission
            }
            PendingAuthoringReportReason::CaptureInterrupted { run_ids } => {
                PendingReportReason::CaptureInterrupted {
                    run_ids: run_ids.clone(),
                }
            }
        },
        // Suite order, exactly as the durable reservation retained it.
        captured_case_ids: pending
            .captured_cases
            .iter()
            .map(|case| case.case_id.clone())
            .collect(),
    })
}

fn finalized_projection(
    report: &AuthoringReport,
    draft: Option<ProjectedDraftRevision<'_>>,
) -> Result<DraftSuiteProjection, ProjectionError> {
    let ExecutionLineage::Draft {
        draft_artifact_hash,
        runtime_flow_version,
        execution_bundle_hash,
        validated_draft_hash,
        catalog_id,
        catalog_version,
        environment,
    } = &report.lineage
    else {
        return Err(error(
            &report.report_id,
            ProjectionErrorKind::ReleaseLineage,
        ));
    };
    let draft = draft.ok_or_else(|| error(&report.report_id, ProjectionErrorKind::MissingDraft))?;
    let revision = wire_uint(&report.report_id, draft.revision)?;
    let edit_to_run_ms = report
        .edit_to_run_ms
        .map(|value| wire_uint(&report.report_id, value))
        .transpose()?;

    Ok(DraftSuiteProjection {
        projection_version: SCHEMA_VERSION.to_owned(),
        report_id: report.report_id.clone(),
        execution_id: report.execution_id.clone(),
        draft: ValidatedDraftIdentity {
            validated_draft_id: validated_draft_hash.clone(),
            draft: DraftIdentity {
                draft_id: draft.draft_id.to_owned(),
                flow_id: report.flow_id.clone(),
                revision,
            },
            runtime_flow_version: unsigned_version(*runtime_flow_version),
            artifact_hash: draft_artifact_hash.clone(),
            execution_bundle_hash: execution_bundle_hash.clone(),
            catalog: CatalogIdentity {
                catalog_id: catalog_id.clone(),
                version: unsigned_version(*catalog_version),
            },
            environment: environment.clone(),
        },
        suite: SuiteRef {
            suite_id: report.suite_id.clone(),
            flow_version: unsigned_version(report.suite_flow_version),
        },
        outcome: outcome(report),
        edit_to_run_ms,
        cases: report.cases.iter().map(case).collect(),
        // Owned by `wamn-ma5`; see the module note.
        nodes: Vec::new(),
        branches: Vec::new(),
        edges: Vec::new(),
    })
}

/// A refusal is never also a pass or a fail: the published outcome is one state.
fn outcome(report: &AuthoringReport) -> SuiteOutcome {
    match &report.refusal {
        Some(ScenarioRefusal::UndrivableNodes { node_types }) => {
            SuiteOutcome::Refused(SuiteExecutionRefusal::UndrivableNodes {
                node_types: node_types.clone(),
            })
        }
        Some(ScenarioRefusal::ValidatedDraftDrift) => {
            SuiteOutcome::Refused(SuiteExecutionRefusal::ValidatedDraftDrift)
        }
        Some(ScenarioRefusal::DraftConnectionsDenied) => {
            SuiteOutcome::Refused(SuiteExecutionRefusal::DraftConnectionsDenied {
                // Not retained by the durable refusal; see the module note.
                connection_names: Vec::new(),
            })
        }
        None if report.passed => SuiteOutcome::Passed,
        None => SuiteOutcome::Failed,
    }
}

fn case(case: &AuthoringCaseReport) -> CaseResultProjection {
    CaseResultProjection {
        case_id: case.case_id.clone(),
        run_id: case.run_id.clone(),
        outcome: if case.passed {
            PassFail::Passed
        } else {
            PassFail::Failed
        },
        // A passing case has no failure to classify, even if a stale kind was
        // retained beside it.
        failure: (!case.passed).then(|| FailureDetail {
            kind: failure_kind(case),
            node_id: case.fail_node.clone(),
        }),
    }
}

/// Classify one failed case from its lifecycle status first, then its kind.
///
/// Cancellation and infrastructure failure are lifecycle states with no
/// `FailKind`, and `EffectUncertain` is an unresolved effect rather than a
/// product verdict, so all three land on the two non-product classifications.
fn failure_kind(case: &AuthoringCaseReport) -> FailureKind {
    match case.status {
        RunStatus::Cancelled => return FailureKind::Cancelled,
        RunStatus::InfrastructureFailure => return FailureKind::InfrastructureFault,
        RunStatus::Dispatched | RunStatus::Running | RunStatus::Completed | RunStatus::Failed => {}
    }
    match case.fail_kind {
        Some(FailKind::Terminal) => FailureKind::Terminal,
        Some(FailKind::RetryExhausted) => FailureKind::RetryExhausted,
        Some(FailKind::InvalidInput) => FailureKind::InvalidInput,
        Some(FailKind::RunawayBudget) => FailureKind::RunawayBudget,
        Some(FailKind::EffectUncertain) => FailureKind::InfrastructureFault,
        // A failed assertion with no captured kind is an ordinary terminal
        // product failure.
        None => FailureKind::Terminal,
    }
}

fn draft_handle(lineage: &ExecutionLineage) -> Option<&str> {
    match lineage {
        ExecutionLineage::Draft {
            validated_draft_hash,
            ..
        } => Some(validated_draft_hash),
        ExecutionLineage::Release { .. } => None,
    }
}

/// Storage keeps these version counters as `i32`; the contract publishes `u32`.
/// A negative version is unreachable behind the storage CHECK constraints, and
/// saturating keeps this pure view total rather than adding a refusal a client
/// could never act on.
fn unsigned_version(value: i32) -> u32 {
    value.max(0).unsigned_abs()
}

fn wire_uint(report_id: &str, value: u64) -> Result<SafeUint64, ProjectionError> {
    SafeUint64::try_from(value).map_err(|source| ProjectionError {
        kind: ProjectionErrorKind::OutsideWireDomain,
        report_id: report_id.into(),
        source: Some(source),
    })
}

fn error(report_id: &str, kind: ProjectionErrorKind) -> ProjectionError {
    ProjectionError {
        kind,
        report_id: report_id.into(),
        source: None,
    }
}

/// A durable report that cannot honestly become a public draft projection.
///
/// Every variant is a caller or storage inconsistency, not a product refusal: a
/// transport maps this to its fault plane, never to a `CommandRefusal`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionError {
    kind: ProjectionErrorKind,
    report_id: Box<str>,
    source: Option<SafeIntegerError>,
}

impl ProjectionError {
    pub const fn kind(&self) -> ProjectionErrorKind {
        self.kind
    }

    pub fn report_id(&self) -> &str {
        &self.report_id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionErrorKind {
    /// The report ran an immutable release, so it has no draft projection.
    ReleaseLineage,
    /// The caller did not resolve the draft revision this report requires.
    MissingDraft,
    /// A stored counter falls outside the exactly representable wire domain.
    OutsideWireDomain,
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let report_id = &self.report_id;
        match self.kind {
            ProjectionErrorKind::ReleaseLineage => write!(
                formatter,
                "report {report_id} ran an immutable release and has no draft suite projection"
            ),
            ProjectionErrorKind::MissingDraft => write!(
                formatter,
                "report {report_id} needs its exact draft revision to project"
            ),
            ProjectionErrorKind::OutsideWireDomain => write!(
                formatter,
                "report {report_id} carries a counter outside the authoring wire domain"
            ),
        }
    }
}

impl std::error::Error for ProjectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use wamn_scenario_model::{Assertion, AssertionResult, Outcome};

    use super::*;

    fn lineage() -> ExecutionLineage {
        ExecutionLineage::Draft {
            draft_artifact_hash: "sha256:artifact".into(),
            runtime_flow_version: 2,
            execution_bundle_hash: "sha256:bundle".into(),
            validated_draft_hash: "sha256:validated".into(),
            catalog_id: "catalog-a".into(),
            catalog_version: 4,
            environment: "dev".into(),
        }
    }

    fn outcome_of(passed: bool) -> Outcome {
        Outcome {
            name: "case-a".into(),
            results: vec![AssertionResult {
                assertion: Assertion::Equals(serde_json::json!({"ok": true})),
                passed,
                detail: (!passed).then(|| "mismatch".into()),
            }],
        }
    }

    fn finalized(
        refusal: Option<ScenarioRefusal>,
        cases: Vec<AuthoringCaseReport>,
    ) -> AuthoringReportState {
        AuthoringReportState::Finalized(AuthoringReport::new(
            "report-1",
            "exec-1",
            "tenant-a",
            "flow-a",
            1,
            "suite-a",
            lineage(),
            Some(1_234),
            refusal,
            cases,
        ))
    }

    fn revision() -> ProjectedDraftRevision<'static> {
        ProjectedDraftRevision {
            draft_id: "draft-a",
            revision: 7,
        }
    }

    #[test]
    fn a_missing_report_projects_without_a_draft_revision() {
        let projected = suite_projection(&AuthoringReportState::NotFound, None)
            .expect("a missing report projects");
        assert_eq!(projected, SuiteProjectionState::NotFound);
        assert_eq!(validated_draft_id(&AuthoringReportState::NotFound), None);
    }

    #[test]
    fn a_finalized_report_projects_its_exact_executable_and_draft_pins() {
        let state = finalized(
            None,
            vec![AuthoringCaseReport::new(
                "case-a",
                "run-a",
                RunStatus::Completed,
                None,
                None,
                outcome_of(true),
            )],
        );
        assert_eq!(validated_draft_id(&state), Some("sha256:validated"));
        let SuiteProjectionState::Finalized(projection) =
            suite_projection(&state, Some(revision())).expect("a finalized report projects")
        else {
            panic!("a finalized report must project as finalized");
        };
        assert_eq!(projection.projection_version, SCHEMA_VERSION);
        assert_eq!(projection.report_id, "report-1");
        assert_eq!(projection.execution_id, "exec-1");
        assert_eq!(projection.draft.validated_draft_id, "sha256:validated");
        assert_eq!(projection.draft.draft.draft_id, "draft-a");
        assert_eq!(projection.draft.draft.flow_id, "flow-a");
        assert_eq!(u64::from(projection.draft.draft.revision), 7);
        assert_eq!(projection.draft.runtime_flow_version, 2);
        assert_eq!(projection.draft.artifact_hash, "sha256:artifact");
        assert_eq!(projection.draft.execution_bundle_hash, "sha256:bundle");
        assert_eq!(projection.draft.catalog.catalog_id, "catalog-a");
        assert_eq!(projection.draft.catalog.version, 4);
        assert_eq!(projection.draft.environment, "dev");
        assert_eq!(projection.suite.suite_id, "suite-a");
        assert_eq!(projection.suite.flow_version, 1);
        assert_eq!(projection.outcome, SuiteOutcome::Passed);
        assert_eq!(projection.edit_to_run_ms.map(u64::from), Some(1_234));
        assert_eq!(projection.cases.len(), 1);
        assert_eq!(projection.cases[0].outcome, PassFail::Passed);
        assert_eq!(projection.cases[0].failure, None);
    }

    #[test]
    fn a_refusal_replaces_the_pass_fail_verdict_instead_of_joining_it() {
        for (refusal, expected) in [
            (
                ScenarioRefusal::UndrivableNodes {
                    node_types: vec!["external".into()],
                },
                SuiteExecutionRefusal::UndrivableNodes {
                    node_types: vec!["external".into()],
                },
            ),
            (
                ScenarioRefusal::ValidatedDraftDrift,
                SuiteExecutionRefusal::ValidatedDraftDrift,
            ),
            (
                ScenarioRefusal::DraftConnectionsDenied,
                SuiteExecutionRefusal::DraftConnectionsDenied {
                    connection_names: Vec::new(),
                },
            ),
        ] {
            let state = finalized(Some(refusal), Vec::new());
            let SuiteProjectionState::Finalized(projection) =
                suite_projection(&state, Some(revision())).expect("a refused report projects")
            else {
                panic!("a finalized report must project as finalized");
            };
            assert_eq!(projection.outcome, SuiteOutcome::Refused(expected));
        }
    }

    #[test]
    fn every_failed_case_carries_one_classified_failure() {
        for (status, kind, expected) in [
            (
                RunStatus::Failed,
                Some(FailKind::Terminal),
                FailureKind::Terminal,
            ),
            (
                RunStatus::Failed,
                Some(FailKind::RetryExhausted),
                FailureKind::RetryExhausted,
            ),
            (
                RunStatus::Failed,
                Some(FailKind::InvalidInput),
                FailureKind::InvalidInput,
            ),
            (
                RunStatus::Failed,
                Some(FailKind::RunawayBudget),
                FailureKind::RunawayBudget,
            ),
            (
                RunStatus::Failed,
                Some(FailKind::EffectUncertain),
                FailureKind::InfrastructureFault,
            ),
            (RunStatus::Failed, None, FailureKind::Terminal),
            (RunStatus::Cancelled, None, FailureKind::Cancelled),
            (
                RunStatus::InfrastructureFailure,
                Some(FailKind::Terminal),
                FailureKind::InfrastructureFault,
            ),
        ] {
            let state = finalized(
                None,
                vec![AuthoringCaseReport::new(
                    "case-a",
                    "run-a",
                    status,
                    kind,
                    Some("node-a".into()),
                    outcome_of(false),
                )],
            );
            let SuiteProjectionState::Finalized(projection) =
                suite_projection(&state, Some(revision())).expect("a failed report projects")
            else {
                panic!("a finalized report must project as finalized");
            };
            assert_eq!(projection.outcome, SuiteOutcome::Failed);
            assert_eq!(
                projection.cases[0].failure,
                Some(FailureDetail {
                    kind: expected,
                    node_id: Some("node-a".into()),
                }),
                "{status:?}/{kind:?} was misclassified"
            );
        }
    }

    #[test]
    fn a_pending_reservation_projects_pending_and_never_a_refusal() {
        for (reason, expected) in [
            (
                PendingAuthoringReportReason::AwaitingAdmission,
                PendingReportReason::AwaitingAdmission,
            ),
            (
                PendingAuthoringReportReason::CaptureInterrupted {
                    run_ids: vec!["run-b".into()],
                },
                PendingReportReason::CaptureInterrupted {
                    run_ids: vec!["run-b".into()],
                },
            ),
        ] {
            let state = AuthoringReportState::Pending(PendingAuthoringReport {
                report_id: "report-1".into(),
                execution_id: "exec-1".into(),
                tenant_id: "tenant-a".into(),
                flow_id: "flow-a".into(),
                suite_flow_version: 1,
                suite_id: "suite-a".into(),
                lineage: lineage(),
                reason,
                captured_cases: vec![AuthoringCaseReport::new(
                    "case-a",
                    "run-a",
                    RunStatus::Completed,
                    None,
                    None,
                    outcome_of(true),
                )],
            });
            // A pending reservation needs no draft revision: nothing about it
            // may be finalized to reach the projection.
            let SuiteProjectionState::Pending(projection) =
                suite_projection(&state, None).expect("a pending reservation projects")
            else {
                panic!("a pending reservation must project as pending");
            };
            assert_eq!(projection.report_id, "report-1");
            assert_eq!(projection.execution_id, "exec-1");
            assert_eq!(
                projection.validated_draft.validated_draft_id,
                "sha256:validated"
            );
            assert_eq!(projection.reason, expected);
            assert_eq!(projection.captured_case_ids, vec!["case-a".to_owned()]);
        }
    }

    #[test]
    fn a_release_report_and_an_unresolved_draft_are_faults_not_refusals() {
        let released = AuthoringReportState::Finalized(AuthoringReport::new(
            "report-2",
            "exec-2",
            "tenant-a",
            "flow-a",
            1,
            "suite-a",
            ExecutionLineage::Release {
                artifact_hash: "sha256:release".into(),
                catalog_id: "catalog-a".into(),
                catalog_version: 4,
                environment: "dev".into(),
            },
            None,
            None,
            Vec::new(),
        ));
        assert_eq!(validated_draft_id(&released), None);
        let error = suite_projection(&released, Some(revision())).expect_err("a release refuses");
        assert_eq!(error.kind(), ProjectionErrorKind::ReleaseLineage);
        assert_eq!(error.report_id(), "report-2");

        let error = suite_projection(&finalized(None, Vec::new()), None)
            .expect_err("an unresolved draft refuses");
        assert_eq!(error.kind(), ProjectionErrorKind::MissingDraft);
    }

    #[test]
    fn a_counter_outside_the_wire_domain_refuses_instead_of_rounding() {
        let outside = wamn_authoring_model::SAFE_INTEGER_MAX + 1;
        let error = suite_projection(
            &finalized(None, Vec::new()),
            Some(ProjectedDraftRevision {
                draft_id: "draft-a",
                revision: outside,
            }),
        )
        .expect_err("an unrepresentable revision refuses");
        assert_eq!(error.kind(), ProjectionErrorKind::OutsideWireDomain);
        assert!(std::error::Error::source(&error).is_some());
    }
}

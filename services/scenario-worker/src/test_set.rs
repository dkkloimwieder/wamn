//! Sequential per-ordinal test-case execution against the candidate wiring.
//!
//! One report's cases run ONE AT A TIME (wamn-0h0g.8.5.4). For each ordinal in
//! turn the composition admits the deterministic project run, observes it to
//! terminal, evaluates the case against what it produced, and copies the
//! asserted facts and the frozen binding world into the immutable control case
//! summary — and only then admits the next ordinal.
//!
//! # Two databases, no transaction across them
//!
//! The reservation, the case map, and the report live in the CONTROL database;
//! the run and its queue row live in the PROJECT database. A transaction cannot
//! span them, so this module composes SEPARATE commits and derives its
//! at-most-once guarantee from durable idempotency keys instead of atomicity:
//!
//! - the reservation is `ON CONFLICT DO NOTHING` and then VERIFIED, so a replay
//!   either finds its own selection or refuses someone else's;
//! - each ordinal's run identity is derived, not minted
//!   ([`test_case_run_id`]), and admission is keyed on
//!   `case:{report}:{ordinal}`, so admitting twice returns `duplicate` with the
//!   original run and its frozen world rather than a second run;
//! - every admission parameter is a function of the reservation and the
//!   candidate — never of the clock or the process — so an exact retry rebuilds
//!   a byte-identical admission;
//! - finalization requires a `pending` case, so a repeated pass over an ordinal
//!   that already has a verdict changes nothing.
//!
//! An exact retry after a commit on EITHER database therefore converges: the
//! composition re-drives from wherever it stopped and reaches the same report.

use std::time::{Duration, SystemTime};

use anyhow::Context as _;
use serde_json::{Value, json};
use tokio_postgres::Client;

use wamn_authoring_model::GateRefusal;
use wamn_execution_contract::{WiringFailureKind, validate_cases};
use wamn_run_state::RunStatus;
use wamn_run_state::admission::AdmissionResult;
use wamn_scenario_model::{Captured, evaluate};

use crate::store::admission::{AdmissionSurface, CandidateWiring, ObservedRun, TestCaseAdmission};
use crate::store::test_orchestration::{
    TestCaseFailure, TestCaseFinalization, TestReportReconciliation, TestReportReservation,
    reconcile_test_report, reserve_test_report, select_test_case_plan,
    select_test_report_reservation_sql,
};

/// How often an admitted run is re-observed while it is not yet terminal.
///
/// The stored per-case deadline, not this interval, is the horizon; this only
/// decides how promptly a terminal run is noticed.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// One test-set command's inputs, already reconciled with the fixed scope.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TestSetRunRequest<'a> {
    pub tenant_id: &'a str,
    pub environment: &'a str,
    /// The wiring hash. The owner ruled `validated_draft_id` IS the wiring hash:
    /// the draft concept died with the pivot, the wiring document is the
    /// validated artifact, and its hash is the identity.
    pub validated_draft_id: &'a str,
}

/// What one test-set command produced.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TestSetComposition {
    /// The report exists and was driven as far as this pass could take it.
    Accepted {
        report_id: String,
        reconciliation: TestReportReconciliation,
    },
    Refused(GateRefusal),
}

/// Drive one report's cases to their verdicts, one ordinal at a time.
pub(crate) async fn run_test_set(
    control: &mut Client,
    admission: &mut AdmissionSurface,
    request: &TestSetRunRequest<'_>,
) -> anyhow::Result<TestSetComposition> {
    let Some(candidate) = admission
        .candidate_by_hash(request.validated_draft_id)
        .await?
    else {
        return Ok(TestSetComposition::Refused(
            GateRefusal::ValidatedDraftNotFound {
                validated_draft_id: request.validated_draft_id.to_owned(),
            },
        ));
    };
    if let Err(error) = validate_cases(&candidate.cases) {
        return Ok(TestSetComposition::Refused(
            GateRefusal::InvalidTestSet {
                detail: error.to_string(),
            },
        ));
    }

    // The constitutional clause (wamn-0h0g.8.5.5): gate cases are EFFECT-FREE BY
    // CONTRACT. A gate is a judgment about a document, not an execution of it —
    // effects belong to admitted runs under run identity. This refuses BEFORE
    // the first ordinal is admitted and before the report is reserved, so no
    // effect is performed and then regretted. Assume the clause instead of
    // checking it and the first effectful case silently double-fires.
    let effectful = admission.effectful_components(&candidate).await?;
    if !effectful.is_empty() {
        return Ok(TestSetComposition::Refused(
            GateRefusal::EffectfulComponentReached {
                components: effectful,
            },
        ));
    }

    // The report identity is DERIVED, never minted: admission classifies a
    // test-case whose `report_id` differs from the candidate's `gate_report_id`
    // as `gate-report-mismatch`, so the gate report IS the report.
    let report_id = candidate.gate_report_id.clone();
    if let Some(refusal) = reserved_elsewhere(control, request, &report_id).await? {
        return Ok(TestSetComposition::Refused(refusal));
    }
    let reservation = TestReportReservation {
        tenant_id: request.tenant_id.to_owned(),
        report_id: report_id.clone(),
        // The candidate identity IS the selection hash. The wiring hash covers
        // the document's `cases` array as well as its graph, and the report is
        // per-candidate because its identity is the candidate's gate report, so
        // there is nothing narrower for this column to bind.
        command_hash: candidate.wiring_hash.clone(),
        validated_draft_id: candidate.wiring_hash.clone(),
        catalog_id: candidate.catalog_id.clone(),
        catalog_version: candidate.catalog_version,
        case_ids: candidate
            .cases
            .iter()
            .map(|case| case.case_id.clone())
            .collect(),
    };
    reserve_test_report(control, &reservation).await?;

    let plan = select_test_case_plan(control, request.tenant_id, &report_id).await?;
    if plan.len() != candidate.cases.len() {
        anyhow::bail!("the reserved case plan does not cover the candidate's cases");
    }

    let mut frozen_binding_world: Option<Value> = None;
    let mut effect_uncertain_run_ids = Vec::new();
    for (entry, case) in plan.iter().zip(&candidate.cases) {
        if entry.case_id != case.case_id {
            anyhow::bail!("the reserved case plan names a different case at one ordinal");
        }
        let admitted = admission
            .admit_test_case(&TestCaseAdmission {
                report_id: &report_id,
                ordinal: entry.ordinal,
                run_id: &entry.run_id,
                catalog_id: &candidate.catalog_id,
                catalog_version: candidate.catalog_version,
                environment: request.environment,
                wiring_id: &candidate.wiring_id,
                wiring_version: candidate.wiring_version,
                wiring_hash: &candidate.wiring_hash,
                gate_report_id: &candidate.gate_report_id,
                input_json: &case.input,
                run_deadline_at: entry.case_deadline_at,
                prior_binding_world: frozen_binding_world.as_ref(),
            })
            .await?;
        let world = match admitted {
            AdmissionResult::Admitted {
                binding_world_json, ..
            }
            | AdmissionResult::Duplicate {
                binding_world_json, ..
            } => binding_world_json,
            refused => {
                return Ok(TestSetComposition::Refused(
                    admission_refusal(admission, &candidate, request, refused).await?,
                ));
            }
        };
        // Ordinal zero admits with no prior world and returns the one every
        // later ordinal must match exactly. A resumed pass re-admits ordinal
        // zero purely to recover it, which is a `duplicate` and mutates nothing.
        let frozen = frozen_binding_world.get_or_insert(world);

        if entry.finalized {
            continue;
        }
        let Some(observed) =
            poll_terminal(admission, &entry.run_id, entry.case_deadline_at).await?
        else {
            // The stored horizon elapsed with the run still live. Reconciliation
            // owns that verdict, because the deadline it enforces is the stored
            // one and not this process's view of the clock.
            break;
        };
        if observed.status == RunStatus::EffectUncertain {
            // An unresolved effect must not be followed by another effectful
            // case. Reconciliation finalizes this ordinal from the identity we
            // already observed it under.
            effect_uncertain_run_ids.push(entry.run_id.clone());
            break;
        }
        let outcome = evaluate(case, &captured(&observed)?);
        let passed = outcome.passed;
        finalize_case(
            control,
            &TestCaseFinalization {
                tenant_id: request.tenant_id.to_owned(),
                report_id: report_id.clone(),
                ordinal: entry.ordinal,
                passed,
                failure: (!passed).then_some(TestCaseFailure::AssertionFailed),
                summary: json!({"case": outcome, "binding-world": frozen}),
            },
        )
        .await?;
    }

    let reconciliation = reconcile_test_report(
        control,
        request.tenant_id,
        &report_id,
        &effect_uncertain_run_ids,
    )
    .await?;
    Ok(TestSetComposition::Accepted {
        report_id,
        reconciliation,
    })
}

/// Refuse when this report identity already belongs to a different candidate.
///
/// The report identity is the candidate's gate report, and nothing forbids two
/// candidates from naming one gate run. Reading the existing reservation first
/// turns that collision into a typed refusal rather than the reservation's own
/// verification failure, which cannot be told from an infrastructure fault.
async fn reserved_elsewhere(
    control: &Client,
    request: &TestSetRunRequest<'_>,
    report_id: &str,
) -> anyhow::Result<Option<GateRefusal>> {
    let Some(row) = control
        .query_opt(
            select_test_report_reservation_sql(),
            &[&request.tenant_id, &report_id],
        )
        .await
        .context("read any existing report reservation")?
    else {
        return Ok(None);
    };
    let reserved: String = row.get(1);
    if reserved == request.validated_draft_id {
        return Ok(None);
    }
    Ok(Some(GateRefusal::ValidatedDraftDrift))
}

/// Persist one ordinal's verdict, tolerating a concurrent pass that got there
/// first.
///
/// Finalization requires a `pending` case, which is what makes an ordinal
/// at-most-once. A second pass that finds the verdict already written has
/// nothing to add: the stored verdict is immutable and re-deriving it could only
/// produce the same answer.
async fn finalize_case(
    control: &Client,
    finalization: &TestCaseFinalization,
) -> anyhow::Result<()> {
    match crate::store::test_orchestration::finalize_test_case(control, finalization).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let plan =
                select_test_case_plan(control, &finalization.tenant_id, &finalization.report_id)
                    .await?;
            if plan
                .iter()
                .any(|entry| entry.ordinal == finalization.ordinal && entry.finalized)
            {
                return Ok(());
            }
            Err(error)
        }
    }
}

/// Observe one admitted run until it is terminal or its stored horizon elapses.
async fn poll_terminal(
    admission: &AdmissionSurface,
    run_id: &str,
    deadline: SystemTime,
) -> anyhow::Result<Option<ObservedRun>> {
    loop {
        let observed = admission
            .observe_run(run_id)
            .await?
            .context("an admitted test-case run is not readable")?;
        if observed.status.is_terminal() || observed.status == RunStatus::EffectUncertain {
            return Ok(Some(observed));
        }
        if SystemTime::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Project one observed run onto the bounded facts a case may assert over.
///
/// These are exactly the columns the admitter is granted for evaluation. A run
/// that reached terminal without releasing a caller outcome yields the default
/// facts, which satisfy no expectation and are reported as such rather than
/// being guessed at.
fn captured(observed: &ObservedRun) -> anyhow::Result<Captured> {
    match observed.caller_outcome_kind.as_deref() {
        Some("responded") => {
            let status = observed
                .caller_http_status
                .context("a responded run stored no caller status")?;
            let status =
                u16::try_from(status).context("a stored caller status is outside the wire type")?;
            let body = observed.caller_outcome_json.clone().unwrap_or(Value::Null);
            Captured::responded(status, body)
                .context("a stored caller outcome is not a shape a run row can have")
        }
        Some("failed") => Ok(Captured::failed(failure_kind(
            observed.fail_kind.as_deref(),
        ))),
        _ if observed.fail_kind.is_some() => Ok(Captured::failed(failure_kind(
            observed.fail_kind.as_deref(),
        ))),
        _ => Ok(Captured::default()),
    }
}

/// Decode a stored `fail_kind` into the authorable vocabulary.
///
/// `runs.fail_kind` admits five storage-owned kinds no expectation can name.
/// `None` is the truthful answer for those: such a run satisfies no
/// `outcome: failed` expectation, loudly, instead of being reported as whichever
/// authorable kind is nearest.
fn failure_kind(stored: Option<&str>) -> Option<WiringFailureKind> {
    serde_json::from_value(json!(stored?)).ok()
}

/// Translate a non-admitting result into this command's typed refusal.
async fn admission_refusal(
    admission: &AdmissionSurface,
    candidate: &CandidateWiring,
    request: &TestSetRunRequest<'_>,
    result: AdmissionResult,
) -> anyhow::Result<GateRefusal> {
    Ok(match result {
        AdmissionResult::CandidateNotFound => GateRefusal::ValidatedDraftNotFound {
            validated_draft_id: request.validated_draft_id.to_owned(),
        },
        AdmissionResult::CandidateIdentityMismatch
        | AdmissionResult::GateReportMismatch
        | AdmissionResult::BindingWorldDrift
        | AdmissionResult::ConflictingRunIdentity => GateRefusal::ValidatedDraftDrift,
        AdmissionResult::CandidateDefinitionInvalid => GateRefusal::InvalidTestSet {
            detail: "the candidate's graph names a component this catalog version does not admit"
                .to_owned(),
        },
        AdmissionResult::BindingWorldUnavailable => GateRefusal::DraftConnectionsDenied {
            connection_names: admission
                .unresolved_store_aliases(candidate, request.environment)
                .await?,
        },
        // Neither is reachable from a well-formed composition: the producer is a
        // constant and every input is derived from a row this process just read.
        // Reaching one is a fault in this module, not a product refusal.
        other @ (AdmissionResult::InvalidProducer | AdmissionResult::InvalidInput) => {
            anyhow::bail!("management admission refused a composed test case: {other:?}")
        }
        AdmissionResult::Admitted { .. } | AdmissionResult::Duplicate { .. } => {
            unreachable!("an admitting result is handled by the caller")
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_execution_contract::{Expect, ExpectedOutcome, TestSetCase};

    use super::*;

    fn observed(
        status: RunStatus,
        kind: Option<&str>,
        json: Option<Value>,
        http: Option<i32>,
        fail: Option<&str>,
    ) -> ObservedRun {
        ObservedRun {
            status,
            caller_outcome_kind: kind.map(str::to_owned),
            caller_outcome_json: json,
            caller_http_status: http,
            fail_kind: fail.map(str::to_owned),
        }
    }

    #[test]
    fn a_released_response_becomes_the_facts_the_expectation_reads() {
        let facts = captured(&observed(
            RunStatus::Completed,
            Some("responded"),
            Some(json!({"id": 7})),
            Some(201),
            None,
        ))
        .expect("a released response is storable");
        let response = facts.response.expect("a response was captured");
        assert_eq!(response.status, 201);
        assert_eq!(response.body, json!({"id": 7}));
        assert_eq!(facts.failure_code, None);
    }

    #[test]
    fn a_typed_failure_becomes_its_authorable_code_or_none() {
        let terminal = captured(&observed(
            RunStatus::Failed,
            Some("failed"),
            Some(json!({"code": "terminal"})),
            None,
            Some("terminal"),
        ))
        .expect("a typed failure is storable");
        assert_eq!(terminal.failure_code, Some(WiringFailureKind::Terminal));
        assert_eq!(terminal.response, None);

        // The five storage-owned kinds no expectation can author read as `None`,
        // so they satisfy nothing rather than being rounded to a near neighbour.
        for storage_owned in [
            "unresolvable-name",
            "hash-invalid-bytes",
            "foreign-revision",
            "incompatible-contract",
            "unbound-requirement",
        ] {
            let facts = captured(&observed(
                RunStatus::Failed,
                Some("failed"),
                None,
                None,
                Some(storage_owned),
            ))
            .expect("a storage-owned failure is storable");
            assert_eq!(facts.failure_code, None, "{storage_owned}");
        }
    }

    /// A run that reached terminal without releasing anything is not guessed at.
    #[test]
    fn a_terminal_run_with_no_caller_outcome_asserts_nothing() {
        for status in [RunStatus::Completed, RunStatus::InfrastructureFailure] {
            let facts = captured(&observed(status, None, None, None, None)).expect("default facts");
            assert_eq!(facts, Captured::default());
            let case = TestSetCase {
                case_id: "case".to_owned(),
                input: json!({}),
                expect: Expect {
                    outcome: ExpectedOutcome::Responded,
                    status: Some(200),
                    body_subset: None,
                    failure_code: None,
                },
            };
            let outcome = evaluate(&case, &facts);
            assert!(!outcome.passed, "{status:?}");
            assert!(outcome.detail.is_some(), "{status:?}");
        }
        // A run that failed without a caller release still carries its typed
        // kind, so its expectation is evaluated rather than defaulted away.
        let facts = captured(&observed(
            RunStatus::Failed,
            None,
            None,
            None,
            Some("depth-budget"),
        ))
        .expect("a fail kind without a caller release is storable");
        assert_eq!(facts.failure_code, Some(WiringFailureKind::DepthBudget));
    }

    /// Only the two admitting results carry a world; everything else is a
    /// refusal or a fault, and none of them is silently treated as success.
    #[test]
    fn every_admission_result_is_classified() {
        let admitting = [
            AdmissionResult::Admitted {
                run_id: "r".to_owned(),
                binding_world_json: json!([]),
            },
            AdmissionResult::Duplicate {
                run_id: "r".to_owned(),
                binding_world_json: json!([]),
            },
        ];
        let refusing = [
            AdmissionResult::CandidateNotFound,
            AdmissionResult::CandidateIdentityMismatch,
            AdmissionResult::GateReportMismatch,
            AdmissionResult::CandidateDefinitionInvalid,
            AdmissionResult::BindingWorldUnavailable,
            AdmissionResult::BindingWorldDrift,
            AdmissionResult::ConflictingRunIdentity,
        ];
        let faults = [
            AdmissionResult::InvalidProducer,
            AdmissionResult::InvalidInput,
        ];
        assert_eq!(admitting.len() + refusing.len() + faults.len(), 11);
        for result in &admitting {
            assert!(matches!(
                result,
                AdmissionResult::Admitted { .. } | AdmissionResult::Duplicate { .. }
            ));
        }
    }

    /// The per-case summary carries BOTH halves the acceptance names, and it is
    /// an object, which is what [`finalize_test_case`] requires.
    #[test]
    fn the_case_summary_carries_the_asserted_facts_and_the_frozen_world() {
        let case = TestSetCase {
            case_id: "roundtrip".to_owned(),
            input: json!({}),
            expect: Expect {
                outcome: ExpectedOutcome::Responded,
                status: Some(201),
                body_subset: None,
                failure_code: None,
            },
        };
        let facts = Captured::responded(201, json!({"id": 1})).expect("storable");
        let world = json!([{"store-alias": "a-store"}]);
        let summary = json!({"case": evaluate(&case, &facts), "binding-world": world});
        assert!(summary.is_object());
        assert_eq!(summary["case"]["case-id"], json!("roundtrip"));
        assert_eq!(summary["case"]["passed"], json!(true));
        assert_eq!(summary["case"]["expect"]["status"], json!(201));
        assert_eq!(summary["case"]["actual"]["response"]["status"], json!(201));
        assert_eq!(summary["binding-world"], world);
    }
}

//! Real Receiving command histories, rollback, replay, authority and contention.

#[path = "receiving_history/database.rs"]
pub(crate) mod database;
#[path = "receiving_history/model.rs"]
mod model;

include!("receiving_command_histories_fixture.rs");
#[test]
fn shrinking_preserves_the_original_business_property_and_outcome() {
    let history = History {
        ordered: [2, 1],
        steps: vec![Step::Replay { key: 0 }],
    };
    let outcome = HistoryOutcome {
        operation: RECEIPT_PATH,
        expected: "committed",
        http_status: 200,
        refusal: None,
    };
    let failure = |property, observed: &HistoryOutcome| {
        business::<()>(
            Err(anyhow::anyhow!("fixture assertion detail")),
            property,
            observed,
        )
    };
    let mut failures = ShrinkFailures::default();
    assert!(matches!(
        shrink_result(
            &mut failures,
            &history,
            failure("REC-REVISION/response", &outcome)
        ),
        Err(TestCaseError::Fail(_)),
    ));
    let original = failures.target.clone();
    assert!(matches!(
        shrink_result(
            &mut failures,
            &history,
            failure("REC-HISTORY/status", &outcome)
        ),
        Err(TestCaseError::Reject(_)),
    ));
    let other_outcome = HistoryOutcome {
        expected: "replayed",
        ..outcome.clone()
    };
    assert!(matches!(
        shrink_result(
            &mut failures,
            &history,
            failure("REC-REVISION/response", &other_outcome)
        ),
        Err(TestCaseError::Reject(_)),
    ));
    assert!(matches!(
        shrink_result(
            &mut failures,
            &history,
            failure("REC-REVISION/response", &outcome)
        ),
        Err(TestCaseError::Fail(_)),
    ));
    assert_eq!(failures.target, original);
}

#[test]
fn infrastructure_failure_cannot_replace_a_business_counterexample() {
    let history = History {
        ordered: [2, 1],
        steps: vec![Step::Replay { key: 0 }],
    };
    let outcome = HistoryOutcome {
        operation: RECEIPT_PATH,
        expected: "committed",
        http_status: 200,
        refusal: None,
    };
    let mut failures = ShrinkFailures::default();
    let _ = shrink_result(
        &mut failures,
        &history,
        business::<()>(
            Err(anyhow::anyhow!("revision mismatch")),
            "REC-REVISION/response",
            &outcome,
        ),
    );
    let original = failures.target.clone();
    assert!(
        shrink_result(
            &mut failures,
            &history,
            Err(anyhow::anyhow!("fixture connection lost")),
        )
        .is_ok()
    );
    assert_eq!(failures.target, original);
    assert!(failures.infrastructure.is_some());

    let mut infrastructure_only = ShrinkFailures::default();
    assert!(matches!(
        shrink_result(
            &mut infrastructure_only,
            &history,
            Err(anyhow::anyhow!("fixture connection lost")),
        ),
        Err(TestCaseError::Fail(_))
    ));
    assert!(infrastructure_only.target.is_none());
    assert!(infrastructure_only.infrastructure.is_some());
}

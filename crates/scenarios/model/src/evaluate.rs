//! Pure evaluation of the flat MVP test-case expectation.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wamn_execution_contract::{Expect, ExpectedOutcome, TestSetCase};

use crate::Captured;

/// The machine-diffable expected/actual result of one test case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Outcome {
    pub case_id: String,
    pub passed: bool,
    pub expect: Expect,
    pub actual: Captured,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Evaluate one validated test case against captured facts.
pub fn evaluate(case: &TestSetCase, captured: &Captured) -> Outcome {
    let (passed, detail) = evaluate_expect(&case.expect, captured);
    Outcome {
        case_id: case.case_id.clone(),
        passed,
        expect: case.expect.clone(),
        actual: captured.clone(),
        detail: (!passed).then_some(detail),
    }
}

fn evaluate_expect(expect: &Expect, captured: &Captured) -> (bool, String) {
    if let Err(error) = expect.validate() {
        return (false, error.to_string());
    }
    if let Err(error) = captured.validate() {
        return (false, error.to_string());
    }

    match expect.outcome {
        ExpectedOutcome::Responded => {
            let Some(actual) = &captured.response else {
                return (false, "no terminal response was captured".to_owned());
            };
            if let Some(expected) = expect.status
                && expected != actual.status
            {
                return (
                    false,
                    format!("response status {} did not match {expected}", actual.status),
                );
            }
            match &expect.body_subset {
                Some(subset) if !is_subset(subset, &actual.body) => (
                    false,
                    format!(
                        "response body {} does not contain the expected subset {subset}",
                        actual.body
                    ),
                ),
                _ => (true, String::new()),
            }
        }
        ExpectedOutcome::Failed => match captured.failure_code {
            None => (false, "no typed flow failure was captured".to_owned()),
            Some(actual) => match expect.failure_code {
                Some(expected) if expected != actual => (
                    false,
                    format!("flow failure {actual:?} did not match {expected:?}"),
                ),
                _ => (true, String::new()),
            },
        },
    }
}

/// Whether `actual` contains `expected`: objects match key-wise and
/// recursively, every other JSON value matches only itself exactly.
fn is_subset(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            expected.iter().all(|(key, expected_field)| {
                actual
                    .get(key)
                    .is_some_and(|actual_field| is_subset(expected_field, actual_field))
            })
        }
        _ => expected == actual,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_execution_contract::{Expect, ExpectedOutcome, TestSetCase, WiringFailureKind};

    use super::evaluate;
    use crate::Captured;

    fn case(expect: Expect) -> TestSetCase {
        TestSetCase {
            case_id: "case".to_owned(),
            input: json!({}),
            expect,
        }
    }

    fn responded(status: Option<u16>, body_subset: Option<serde_json::Value>) -> Expect {
        Expect {
            outcome: ExpectedOutcome::Responded,
            status,
            body_subset,
            failure_code: None,
        }
    }

    fn captured_response(status: u16, body: serde_json::Value) -> Captured {
        Captured::responded(status, body).expect("a storable response status")
    }

    #[test]
    fn missing_facts_fail_both_outcomes() {
        for expect in [
            responded(Some(200), None),
            Expect {
                outcome: ExpectedOutcome::Failed,
                status: None,
                body_subset: None,
                failure_code: Some(WiringFailureKind::Terminal),
            },
        ] {
            let outcome = evaluate(&case(expect), &Captured::default());
            assert!(!outcome.passed);
            assert!(outcome.detail.is_some());
        }
    }

    #[test]
    fn a_bare_responded_expectation_asserts_only_that_a_response_exists() {
        let outcome = evaluate(
            &case(responded(None, None)),
            &captured_response(503, json!({"error": "down"})),
        );
        assert!(outcome.passed);
        assert!(outcome.detail.is_none());
    }

    #[test]
    fn status_matches_only_the_exact_literal() {
        let expect = responded(Some(201), None);
        assert!(evaluate(&case(expect.clone()), &captured_response(201, json!({}))).passed);
        assert!(!evaluate(&case(expect), &captured_response(200, json!({}))).passed);
    }

    #[test]
    fn body_subset_ignores_extra_keys_and_recurses() {
        let expect = responded(None, Some(json!({"created": {"id": 7}})));
        assert!(
            evaluate(
                &case(expect.clone()),
                &captured_response(201, json!({"created": {"id": 7, "at": "now"}, "trace": 1}))
            )
            .passed
        );
        for mismatch in [
            json!({"created": {"id": 8}}),
            json!({"created": {}}),
            json!({"other": {"id": 7}}),
            json!([{"created": {"id": 7}}]),
        ] {
            assert!(!evaluate(&case(expect.clone()), &captured_response(201, mismatch)).passed);
        }
    }

    #[test]
    fn failure_code_matches_only_the_exact_literal_and_may_be_omitted() {
        let uncertain = Captured {
            response: None,
            failure_code: Some(WiringFailureKind::EffectUncertain),
        };
        let any_failure = Expect {
            outcome: ExpectedOutcome::Failed,
            status: None,
            body_subset: None,
            failure_code: None,
        };
        assert!(evaluate(&case(any_failure), &uncertain).passed);

        let exact = Expect {
            outcome: ExpectedOutcome::Failed,
            status: None,
            body_subset: None,
            failure_code: Some(WiringFailureKind::EffectUncertain),
        };
        assert!(evaluate(&case(exact.clone()), &uncertain).passed);
        assert!(
            !evaluate(
                &case(exact),
                &Captured {
                    response: None,
                    failure_code: Some(WiringFailureKind::Terminal),
                }
            )
            .passed
        );
    }

    #[test]
    fn a_failed_outcome_reports_the_captured_facts_beside_the_expectation() {
        let expect = responded(Some(200), None);
        let actual = captured_response(500, json!({"error": "boom"}));
        let outcome = evaluate(&case(expect.clone()), &actual);
        assert_eq!(outcome.case_id, "case");
        assert_eq!(outcome.expect, expect);
        assert_eq!(outcome.actual, actual);
        assert!(
            outcome
                .detail
                .expect("a failure carries its detail")
                .contains("500")
        );
    }

    /// The terminal `wamn_run.runs` shapes a case's run can end in, paired with
    /// the verdict each authored expectation must receive once a producer maps
    /// the row into [`Captured`]. No producer exists yet (wamn-0h0g.15.77); this
    /// fixes what the one that lands may not get wrong.
    #[test]
    fn the_run_row_shapes_a_producer_maps_pin_these_verdicts() {
        let expect_status_200 = responded(Some(200), None);
        let expect_any_failure = Expect {
            outcome: ExpectedOutcome::Failed,
            status: None,
            body_subset: None,
            failure_code: None,
        };
        let expect_terminal = Expect {
            outcome: ExpectedOutcome::Failed,
            status: None,
            body_subset: None,
            failure_code: Some(WiringFailureKind::Terminal),
        };

        for (label, expect, facts, passed) in [
            (
                "responded 200 meets status 200",
                expect_status_200.clone(),
                captured_response(200, json!({"ok": true})),
                true,
            ),
            (
                "responded 500 misses status 200",
                expect_status_200.clone(),
                captured_response(500, json!({})),
                false,
            ),
            (
                "a failed run never responded",
                expect_status_200,
                Captured::failed(Some(WiringFailureKind::Terminal)),
                false,
            ),
            (
                "a responded run never failed",
                expect_any_failure.clone(),
                captured_response(200, json!({})),
                false,
            ),
            (
                "any typed failure meets a bare failed",
                expect_any_failure.clone(),
                Captured::failed(Some(WiringFailureKind::RunawayBudget)),
                true,
            ),
            (
                "an unauthorable fail_kind meets nothing",
                expect_any_failure,
                Captured::failed(None),
                false,
            ),
            (
                "terminal meets failure-code terminal",
                expect_terminal.clone(),
                Captured::failed(Some(WiringFailureKind::Terminal)),
                true,
            ),
            (
                "terminal is not a nearest match for an unauthorable fail_kind",
                expect_terminal,
                Captured::failed(None),
                false,
            ),
        ] {
            assert_eq!(evaluate(&case(expect), &facts).passed, passed, "{label}");
        }
    }
}

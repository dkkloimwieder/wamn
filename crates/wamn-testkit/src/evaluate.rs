//! The pure evaluator: fold a [`TestCase`]'s assertions over a [`Captured`] fact
//! bundle into an [`Outcome`]. No effects — every fact was captured by the
//! harness first, so this is a total function of `(case, captured)`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::TestCase;
use crate::assertion::{Assertion, DbExpect, EgressAssertion, EgressMatcher};
use crate::captured::{Captured, DbCapture, EgressRecord};

/// One assertion's verdict, carrying the assertion itself (so a report is
/// self-describing) and, on failure, a human detail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AssertionResult {
    pub assertion: Assertion,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The result of evaluating a whole case: one [`AssertionResult`] per assertion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Outcome {
    pub name: String,
    pub results: Vec<AssertionResult>,
}

impl Outcome {
    /// Whether every assertion passed (an empty expectation set passes — the
    /// caller decides whether that is meaningful).
    pub fn passed(&self) -> bool {
        self.results.iter().all(|r| r.passed)
    }

    /// The assertions that failed (a compact failure summary for a report).
    pub fn failures(&self) -> impl Iterator<Item = &AssertionResult> {
        self.results.iter().filter(|r| !r.passed)
    }
}

/// Deep-subset match with the array rule pinned by 11.4:
/// - **objects**: every key in `expected` is present in `actual` and recursively
///   subset-matches (extra actual keys are ignored);
/// - **arrays**: every element in `expected` subset-matches SOME element of
///   `actual` — order-insensitive, no length constraint (extra actual elements
///   are ignored);
/// - **scalars**: exact JSON equality.
pub fn subset_match(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => e
            .iter()
            .all(|(k, ev)| a.get(k).is_some_and(|av| subset_match(ev, av))),
        (Value::Array(e), Value::Array(a)) => {
            e.iter().all(|ev| a.iter().any(|av| subset_match(ev, av)))
        }
        (e, a) => e == a,
    }
}

/// Evaluate every assertion of `case` against `captured`.
pub fn evaluate(case: &TestCase, captured: &Captured) -> Outcome {
    let results = case
        .expect
        .iter()
        .map(|a| {
            let (passed, detail) = eval_one(a, captured);
            AssertionResult {
                assertion: a.clone(),
                passed,
                detail: (!passed).then(|| detail.unwrap_or_else(|| "assertion failed".into())),
            }
        })
        .collect();
    Outcome {
        name: case.name.clone(),
        results,
    }
}

/// A present matcher field must equal the record's; an absent field is a
/// wildcard.
fn matcher_matches(m: &EgressMatcher, r: &EgressRecord) -> bool {
    m.method.as_deref().is_none_or(|x| x == r.method)
        && m.authority.as_deref().is_none_or(|x| x == r.authority)
        && m.path.as_deref().is_none_or(|x| x == r.path)
}

fn eval_egress(flow: &str, calls: &EgressAssertion, captured: &Captured) -> (bool, Option<String>) {
    let recs: Vec<&EgressRecord> = captured
        .egress
        .iter()
        .filter(|r| r.workload_id == flow)
        .collect();
    match calls {
        EgressAssertion::Count(n) => {
            let got = recs.len() as u64;
            (
                got == *n,
                Some(format!(
                    "flow {flow}: expected {n} egress call(s), got {got}"
                )),
            )
        }
        EgressAssertion::NoneDenied => {
            let denied = recs.iter().filter(|r| !r.allowed).count();
            (
                denied == 0,
                Some(format!("flow {flow}: {denied} denied egress call(s)")),
            )
        }
        EgressAssertion::Includes(matchers) => {
            let missing = matchers
                .iter()
                .filter(|m| !recs.iter().any(|r| matcher_matches(m, r)))
                .count();
            (
                missing == 0,
                Some(format!(
                    "flow {flow}: {missing} expected egress matcher(s) matched no call"
                )),
            )
        }
        EgressAssertion::ExactlyThese(matchers) => {
            // Set-equality "nothing else": every record is covered by some
            // matcher (no UNEXPECTED call — the security regression), every
            // matcher covers some record (no missing expected call), and the
            // counts agree (an extra call that happens to alias an existing
            // matcher is still caught by the length check).
            let unexpected = recs
                .iter()
                .filter(|r| !matchers.iter().any(|m| matcher_matches(m, r)))
                .count();
            let unused = matchers
                .iter()
                .filter(|m| !recs.iter().any(|r| matcher_matches(m, r)))
                .count();
            let len_ok = recs.len() == matchers.len();
            let ok = unexpected == 0 && unused == 0 && len_ok;
            (
                ok,
                Some(format!(
                    "flow {flow}: not EXACTLY the expected set ({unexpected} unexpected call(s), {unused} unused matcher(s), {} records vs {} matchers)",
                    recs.len(),
                    matchers.len()
                )),
            )
        }
    }
}

fn eval_db(
    query: &str,
    params: &[Value],
    expect: &DbExpect,
    captured: &Captured,
) -> (bool, Option<String>) {
    let Some(cap): Option<&DbCapture> = captured
        .db
        .iter()
        .find(|c| c.query == query && c.params == params)
    else {
        return (
            false,
            Some(format!(
                "no captured rows for query {query:?} params {params:?}"
            )),
        );
    };
    match expect {
        DbExpect::RowCount(n) => {
            let got = cap.rows.len() as u64;
            (
                got == *n,
                Some(format!("query {query:?}: expected {n} row(s), got {got}")),
            )
        }
        DbExpect::Empty => (
            cap.rows.is_empty(),
            Some(format!(
                "query {query:?}: expected no rows, got {}",
                cap.rows.len()
            )),
        ),
        DbExpect::FirstRow { row, subset } => match cap.rows.first() {
            None => (false, Some(format!("query {query:?}: no first row"))),
            Some(first) => {
                let ok = if *subset {
                    subset_match(row, first)
                } else {
                    row == first
                };
                (
                    ok,
                    Some(format!(
                        "query {query:?}: first row {first} did not {} {row}",
                        if *subset { "subset-match" } else { "equal" }
                    )),
                )
            }
        },
    }
}

/// Evaluate one assertion; the `Option<String>` is the failure detail (ignored
/// when it passed).
fn eval_one(a: &Assertion, captured: &Captured) -> (bool, Option<String>) {
    match a {
        Assertion::Equals(expected) => match &captured.node_output {
            None => (false, Some("no node output captured".into())),
            Some(actual) => (
                actual == expected,
                Some(format!("node output {actual} != {expected}")),
            ),
        },
        Assertion::Subset(expected) => match &captured.node_output {
            None => (false, Some("no node output captured".into())),
            Some(actual) => (
                subset_match(expected, actual),
                Some(format!(
                    "node output {actual} is not a subset-match of {expected}"
                )),
            ),
        },
        Assertion::PathEquals { pointer, value } => match &captured.node_output {
            None => (false, Some("no node output captured".into())),
            Some(actual) => match actual.pointer(pointer) {
                None => (
                    false,
                    Some(format!("node output has no value at pointer {pointer:?}")),
                ),
                Some(at) => (
                    at == value,
                    Some(format!("node output at {pointer:?} = {at} != {value}")),
                ),
            },
        },
        Assertion::Port(port) => (
            captured.node_port.as_deref() == Some(port.as_str()),
            Some(format!(
                "node port {:?} != {port:?}",
                captured.node_port.as_deref()
            )),
        ),
        Assertion::DbState {
            query,
            params,
            expect,
        } => eval_db(query, params, expect, captured),
        Assertion::Egress { flow, calls } => eval_egress(flow, calls, captured),
        Assertion::ErrorClass { node_error } => (
            captured.node_error == Some(*node_error),
            Some(format!(
                "node error {:?} != {node_error:?}",
                captured.node_error
            )),
        ),
        Assertion::RunOutcome {
            status,
            fail_kind,
            fail_node,
        } => match &captured.run {
            None => (false, Some("no run facts captured".into())),
            Some(run) => {
                let status_ok = run.status == *status;
                let kind_ok = fail_kind.is_none() || &run.fail_kind == fail_kind;
                let node_ok = fail_node.is_none() || &run.fail_node == fail_node;
                (
                    status_ok && kind_ok && node_ok,
                    Some(format!(
                        "run outcome {:?}/{:?}/{:?} != {status:?}/{fail_kind:?}/{fail_node:?}",
                        run.status, run.fail_kind, run.fail_node
                    )),
                )
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assertion::{DbExpect, EgressAssertion, EgressMatcher};
    use crate::captured::{DbCapture, EgressRecord, RunFacts};
    use serde_json::json;
    use wamn_run_store::{FailKind, NodeErrorKind, RunStatus};

    fn node_case(name: &str, expect: Vec<Assertion>) -> TestCase {
        TestCase {
            schema_version: crate::SCHEMA_VERSION.to_string(),
            name: name.to_string(),
            flow_ref: None,
            node_ref: None,
            input: json!({}),
            config: None,
            ctx: None,
            expect,
        }
    }

    fn rec(flow: &str, method: &str, authority: &str, path: &str, allowed: bool) -> EgressRecord {
        EgressRecord {
            workload_id: flow.into(),
            method: method.into(),
            authority: authority.into(),
            path: path.into(),
            allowed,
        }
    }

    // --- subset semantics (incl. the pinned array rule) ---------------------

    #[test]
    fn subset_object_ignores_extra_actual_keys() {
        assert!(subset_match(
            &json!({"a": 1}),
            &json!({"a": 1, "b": 2, "c": 3})
        ));
        // A missing expected key fails.
        assert!(!subset_match(&json!({"z": 1}), &json!({"a": 1})));
        // A present-but-different value fails.
        assert!(!subset_match(&json!({"a": 2}), &json!({"a": 1})));
    }

    /// PIN: the array rule is "each expected element matches SOME actual element,
    /// order-insensitive, no length constraint". This is the drift-guard — an
    /// implementation that required equal length or positional match fails here.
    #[test]
    fn subset_array_is_order_insensitive_and_unconstrained_in_length() {
        // out of order
        assert!(subset_match(&json!([2, 1]), &json!([1, 2, 3])));
        // subset elements against a longer actual
        assert!(subset_match(
            &json!([{"k": "b"}]),
            &json!([{"k": "a"}, {"k": "b", "extra": true}])
        ));
        // an expected element that matches NOTHING fails
        assert!(!subset_match(&json!([4]), &json!([1, 2, 3])));
        // nested: array inside object
        assert!(subset_match(
            &json!({"xs": [3]}),
            &json!({"xs": [1, 2, 3], "ys": [9]})
        ));
    }

    // --- node-output matchers -----------------------------------------------

    #[test]
    fn equals_and_subset_and_path_and_port() {
        let mut cap = Captured {
            node_output: Some(json!({"recommended": "reject", "confidence": 0.9})),
            node_port: Some("main".into()),
            ..Default::default()
        };
        let out = evaluate(
            &node_case(
                "n",
                vec![
                    Assertion::Subset(json!({"recommended": "reject"})),
                    Assertion::PathEquals {
                        pointer: "/confidence".into(),
                        value: json!(0.9),
                    },
                    Assertion::Port("main".into()),
                ],
            ),
            &cap,
        );
        assert!(out.passed(), "{:?}", out.failures().collect::<Vec<_>>());

        // Equals is exact — a subset is NOT an equal.
        let out = evaluate(
            &node_case(
                "n",
                vec![Assertion::Equals(json!({"recommended": "reject"}))],
            ),
            &cap,
        );
        assert!(!out.passed());

        // A wrong port fails.
        cap.node_port = Some("true".into());
        let out = evaluate(&node_case("n", vec![Assertion::Port("main".into())]), &cap);
        assert!(!out.passed());
    }

    #[test]
    fn missing_node_output_fails_rather_than_false_passes() {
        let out = evaluate(
            &node_case("n", vec![Assertion::Subset(json!({"a": 1}))]),
            &Captured::default(),
        );
        assert!(!out.passed());
        assert_eq!(
            out.results[0].detail.as_deref(),
            Some("no node output captured")
        );
    }

    // --- error-class match (mutation target: comparison always-true) --------

    /// The error-class comparison must be a REAL equality: a captured `Terminal`
    /// does NOT satisfy an `InvalidInput` assertion. A mutant that hard-codes the
    /// comparison to `true` fails here.
    #[test]
    fn error_class_matches_the_exact_kind_only() {
        let cap = Captured {
            node_error: Some(NodeErrorKind::Terminal),
            ..Default::default()
        };
        let hit = evaluate(
            &node_case(
                "n",
                vec![Assertion::ErrorClass {
                    node_error: NodeErrorKind::Terminal,
                }],
            ),
            &cap,
        );
        assert!(hit.passed());
        let miss = evaluate(
            &node_case(
                "n",
                vec![Assertion::ErrorClass {
                    node_error: NodeErrorKind::InvalidInput,
                }],
            ),
            &cap,
        );
        assert!(!miss.passed(), "Terminal must not satisfy InvalidInput");
    }

    // --- run outcome --------------------------------------------------------

    #[test]
    fn run_outcome_status_and_optional_constraints() {
        let cap = Captured {
            run: Some(RunFacts {
                status: RunStatus::Failed,
                fail_kind: Some(FailKind::Terminal),
                fail_node: Some("h".into()),
            }),
            ..Default::default()
        };
        // status-only (don't-care on kind/node) passes.
        assert!(
            evaluate(
                &node_case(
                    "n",
                    vec![Assertion::RunOutcome {
                        status: RunStatus::Failed,
                        fail_kind: None,
                        fail_node: None
                    }]
                ),
                &cap
            )
            .passed()
        );
        // a wrong fail_node constraint fails.
        assert!(
            !evaluate(
                &node_case(
                    "n",
                    vec![Assertion::RunOutcome {
                        status: RunStatus::Failed,
                        fail_kind: None,
                        fail_node: Some("w".into())
                    }]
                ),
                &cap
            )
            .passed()
        );
    }

    // --- db state -----------------------------------------------------------

    #[test]
    fn db_state_rowcount_firstrow_empty() {
        let cap = Captured {
            db: vec![
                DbCapture {
                    query: "select to_jsonb(sink) from sink".into(),
                    params: vec![],
                    rows: vec![json!({"step": 0, "payload": "receipt"})],
                },
                DbCapture {
                    query: "empty".into(),
                    params: vec![],
                    rows: vec![],
                },
            ],
            ..Default::default()
        };
        let out = evaluate(
            &node_case(
                "n",
                vec![
                    Assertion::DbState {
                        query: "select to_jsonb(sink) from sink".into(),
                        params: vec![],
                        expect: DbExpect::RowCount(1),
                    },
                    Assertion::DbState {
                        query: "select to_jsonb(sink) from sink".into(),
                        params: vec![],
                        expect: DbExpect::FirstRow {
                            row: json!({"payload": "receipt"}),
                            subset: true,
                        },
                    },
                    Assertion::DbState {
                        query: "empty".into(),
                        params: vec![],
                        expect: DbExpect::Empty,
                    },
                ],
            ),
            &cap,
        );
        assert!(out.passed(), "{:?}", out.failures().collect::<Vec<_>>());

        // A DbState with no matching capture fails (never a false pass).
        let out = evaluate(
            &node_case(
                "n",
                vec![Assertion::DbState {
                    query: "unseen".into(),
                    params: vec![],
                    expect: DbExpect::Empty,
                }],
            ),
            &cap,
        );
        assert!(!out.passed());
    }

    // --- egress: ExactlyThese set-equality (mutation target: ignores extras) -

    fn one_expected_call() -> Vec<Assertion> {
        vec![Assertion::Egress {
            flow: "f".into(),
            calls: EgressAssertion::ExactlyThese(vec![EgressMatcher {
                method: None,
                authority: Some("echo.local:8080".into()),
                path: None,
            }]),
        }]
    }

    #[test]
    fn exactly_these_passes_on_the_exact_set() {
        let cap = Captured {
            egress: vec![rec("f", "GET", "echo.local:8080", "/echo", true)],
            ..Default::default()
        };
        assert!(evaluate(&node_case("n", one_expected_call()), &cap).passed());
    }

    /// The security regression: an EXTRA outbound call the flow should not have
    /// made must FAIL an ExactlyThese asserting only the one expected call. A
    /// mutant that ignores extras (drops the "every record covered" OR the
    /// length check) passes here and is caught.
    #[test]
    fn exactly_these_catches_an_extra_call() {
        let cap = Captured {
            egress: vec![
                rec("f", "GET", "echo.local:8080", "/echo", true),
                // the planted SSRF target — an EXTRA call
                rec("f", "GET", "169.254.169.254", "/latest/meta-data/", false),
            ],
            ..Default::default()
        };
        let out = evaluate(&node_case("n", one_expected_call()), &cap);
        assert!(
            !out.passed(),
            "an extra outbound call must fail ExactlyThese"
        );
    }

    #[test]
    fn exactly_these_catches_a_missing_call() {
        // No records at all — the one expected matcher is unused.
        let out = evaluate(&node_case("n", one_expected_call()), &Captured::default());
        assert!(!out.passed());
    }

    #[test]
    fn egress_includes_count_and_none_denied() {
        let cap = Captured {
            egress: vec![
                rec("f", "GET", "echo.local:8080", "/echo", true),
                rec("g", "POST", "other.local", "/x", true),
            ],
            ..Default::default()
        };
        // Includes tolerates the sibling flow g's call; Count is per-flow.
        let out = evaluate(
            &node_case(
                "n",
                vec![
                    Assertion::Egress {
                        flow: "f".into(),
                        calls: EgressAssertion::Includes(vec![EgressMatcher {
                            method: None,
                            authority: Some("echo.local:8080".into()),
                            path: None,
                        }]),
                    },
                    Assertion::Egress {
                        flow: "f".into(),
                        calls: EgressAssertion::Count(1),
                    },
                    Assertion::Egress {
                        flow: "f".into(),
                        calls: EgressAssertion::NoneDenied,
                    },
                ],
            ),
            &cap,
        );
        assert!(out.passed(), "{:?}", out.failures().collect::<Vec<_>>());

        // A denied call trips NoneDenied.
        let cap = Captured {
            egress: vec![rec("f", "GET", "169.254.169.254", "/x", false)],
            ..Default::default()
        };
        assert!(
            !evaluate(
                &node_case(
                    "n",
                    vec![Assertion::Egress {
                        flow: "f".into(),
                        calls: EgressAssertion::NoneDenied
                    }]
                ),
                &cap
            )
            .passed()
        );
    }

    #[test]
    fn outcome_round_trips_through_json() {
        let cap = Captured {
            node_output: Some(json!({"recommended": "accept"})),
            ..Default::default()
        };
        let out = evaluate(
            &node_case(
                "rt",
                vec![Assertion::Subset(json!({"recommended": "accept"}))],
            ),
            &cap,
        );
        let wire = serde_json::to_string(&out).unwrap();
        let back: Outcome = serde_json::from_str(&wire).unwrap();
        assert_eq!(out, back);
    }
}

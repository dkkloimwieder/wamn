//! Pure evaluation of the four MVP test-set assertion families.

use serde::{Deserialize, Serialize};

use crate::{Assertion, Captured, TestSetCase};

/// One assertion verdict in a finalized test-case result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AssertionResult {
    pub assertion: Assertion,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The pure result of evaluating one test-set case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Outcome {
    pub case_id: String,
    pub results: Vec<AssertionResult>,
}

impl Outcome {
    /// Whether every non-vacuous assertion passed.
    pub fn passed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|result| result.passed)
    }

    /// The assertion verdicts that failed.
    pub fn failures(&self) -> impl Iterator<Item = &AssertionResult> {
        self.results.iter().filter(|result| !result.passed)
    }
}

/// Evaluate one validated test-set case against captured facts.
pub fn evaluate(case: &TestSetCase, captured: &Captured) -> Outcome {
    let results = case
        .expect
        .iter()
        .map(|assertion| {
            let (passed, detail) = evaluate_one(assertion, captured);
            AssertionResult {
                assertion: assertion.clone(),
                passed,
                detail: (!passed).then_some(detail),
            }
        })
        .collect();
    Outcome {
        case_id: case.case_id.clone(),
        results,
    }
}

fn evaluate_one(assertion: &Assertion, captured: &Captured) -> (bool, String) {
    if let Err(error) = assertion.validate() {
        return (false, error.to_string());
    }

    match assertion {
        Assertion::RunTerminalOutcome(expected) => match captured.run_terminal_outcome {
            Some(actual) if actual == expected.status => (true, String::new()),
            Some(actual) => (
                false,
                format!(
                    "run terminal status {actual:?} did not match {:?}",
                    expected.status
                ),
            ),
            None => (false, "no run terminal outcome was captured".to_owned()),
        },
        Assertion::TerminalRespond(expected) => match &captured.terminal_respond {
            Some(actual) if actual == expected => (true, String::new()),
            Some(actual) => (
                false,
                format!("terminal response {actual:?} did not match {expected:?}"),
            ),
            None => (false, "no terminal response was captured".to_owned()),
        },
        Assertion::TypedFlowFailure(expected) => match captured.typed_flow_failure {
            Some(actual) if actual == expected.kind => (true, String::new()),
            Some(actual) => (
                false,
                format!(
                    "typed flow failure {actual:?} did not match {:?}",
                    expected.kind
                ),
            ),
            None => (false, "no typed flow failure was captured".to_owned()),
        },
        Assertion::NamedNodeTerminal(expected) => {
            let mut observed = 0_usize;
            let mut mismatches = 0_usize;
            for actual in captured.named_node_terminals.iter().filter(|actual| {
                actual.flow_id == expected.flow_id && actual.node_id == expected.node_id
            }) {
                observed += 1;
                if actual.status != expected.status || actual.failure_kind != expected.failure_kind
                {
                    mismatches += 1;
                }
            }
            if observed == 0 {
                return (
                    false,
                    format!(
                        "flow/node pair ({:?}, {:?}) was not observed",
                        expected.flow_id, expected.node_id
                    ),
                );
            }
            (
                mismatches == 0,
                format!(
                    "flow/node pair ({:?}, {:?}) had {mismatches} mismatching occurrence(s) out of {observed}",
                    expected.flow_id, expected.node_id
                ),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::evaluate;
    use crate::{
        Assertion, Captured, FlowFailureKind, NamedNodeTerminal, NodeFailureKind,
        NodeTerminalStatus, RunTerminalOutcome, RunTerminalStatus, TerminalRespond, TestSetCase,
        TypedFlowFailure,
    };

    fn named_node(
        flow_id: &str,
        node_id: &str,
        status: NodeTerminalStatus,
        failure_kind: Option<NodeFailureKind>,
    ) -> NamedNodeTerminal {
        NamedNodeTerminal {
            flow_id: flow_id.into(),
            node_id: node_id.into(),
            status,
            failure_kind,
        }
    }

    fn case(expect: Vec<Assertion>) -> TestSetCase {
        TestSetCase {
            case_id: "case".to_owned(),
            input: json!({}),
            expect,
        }
    }

    #[test]
    fn missing_facts_fail_every_family() {
        let assertions = vec![
            Assertion::RunTerminalOutcome(RunTerminalOutcome {
                status: RunTerminalStatus::Completed,
            }),
            Assertion::TerminalRespond(TerminalRespond {
                status: 200,
                body: json!({"ok": true}),
            }),
            Assertion::TypedFlowFailure(TypedFlowFailure {
                kind: FlowFailureKind::Terminal,
            }),
            Assertion::NamedNodeTerminal(named_node(
                "orders",
                "write",
                NodeTerminalStatus::Success,
                None,
            )),
        ];

        let outcome = evaluate(&case(assertions), &Captured::default());
        assert!(!outcome.passed());
        assert_eq!(outcome.failures().count(), 4);
    }

    #[test]
    fn response_requires_both_status_and_exact_parsed_json_body() {
        let assertion = Assertion::TerminalRespond(TerminalRespond {
            status: 201,
            body: json!({"created": {"id": 7}}),
        });
        let exact = Captured {
            terminal_respond: Some(TerminalRespond {
                status: 201,
                body: json!({"created": {"id": 7}}),
            }),
            ..Captured::default()
        };
        assert!(evaluate(&case(vec![assertion.clone()]), &exact).passed());

        for mismatch in [
            TerminalRespond {
                status: 200,
                body: json!({"created": {"id": 7}}),
            },
            TerminalRespond {
                status: 201,
                body: json!({"created": {"id": 7, "extra": true}}),
            },
        ] {
            let facts = Captured {
                terminal_respond: Some(mismatch),
                ..Captured::default()
            };
            assert!(!evaluate(&case(vec![assertion.clone()]), &facts).passed());
        }
    }

    #[test]
    fn run_and_typed_failure_match_only_the_exact_literal() {
        let assertions = vec![
            Assertion::RunTerminalOutcome(RunTerminalOutcome {
                status: RunTerminalStatus::Failed,
            }),
            Assertion::TypedFlowFailure(TypedFlowFailure {
                kind: FlowFailureKind::InvalidInput,
            }),
        ];
        let exact = Captured {
            run_terminal_outcome: Some(RunTerminalStatus::Failed),
            typed_flow_failure: Some(FlowFailureKind::InvalidInput),
            ..Captured::default()
        };
        assert!(evaluate(&case(assertions.clone()), &exact).passed());

        let wrong_run = Captured {
            run_terminal_outcome: Some(RunTerminalStatus::Completed),
            typed_flow_failure: Some(FlowFailureKind::InvalidInput),
            ..Captured::default()
        };
        assert!(!evaluate(&case(assertions.clone()), &wrong_run).passed());

        let wrong_failure = Captured {
            run_terminal_outcome: Some(RunTerminalStatus::Failed),
            typed_flow_failure: Some(FlowFailureKind::Terminal),
            ..Captured::default()
        };
        assert!(!evaluate(&case(assertions), &wrong_failure).passed());
    }

    #[test]
    fn named_node_selector_refuses_zero_observations() {
        let expected = named_node(
            "orders",
            "write",
            NodeTerminalStatus::Error,
            Some(NodeFailureKind::InvalidInput),
        );
        let assertion = Assertion::NamedNodeTerminal(expected.clone());

        assert!(!evaluate(&case(vec![assertion.clone()]), &Captured::default()).passed());
    }

    #[test]
    fn named_node_selector_requires_exact_flow_and_node_identity() {
        let expected = named_node(
            "orders",
            "write",
            NodeTerminalStatus::Error,
            Some(NodeFailureKind::InvalidInput),
        );
        let assertion = Assertion::NamedNodeTerminal(expected.clone());

        let all_match = Captured {
            named_node_terminals: vec![
                expected.clone(),
                named_node("other-flow", "write", NodeTerminalStatus::Success, None),
                named_node("orders", "other-node", NodeTerminalStatus::Success, None),
                expected.clone(),
            ],
            ..Captured::default()
        };
        assert!(evaluate(&case(vec![assertion.clone()]), &all_match).passed());
    }

    #[test]
    fn named_node_selector_requires_all_matching_observations() {
        let expected = named_node(
            "orders",
            "write",
            NodeTerminalStatus::Error,
            Some(NodeFailureKind::InvalidInput),
        );
        let assertion = Assertion::NamedNodeTerminal(expected.clone());

        let mixed = Captured {
            named_node_terminals: vec![
                expected,
                named_node(
                    "orders",
                    "write",
                    NodeTerminalStatus::Error,
                    Some(NodeFailureKind::Terminal),
                ),
            ],
            ..Captured::default()
        };
        assert!(!evaluate(&case(vec![assertion]), &mixed).passed());
    }
}

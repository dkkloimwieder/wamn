//! The closed MVP test-set assertion vocabulary.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use wamn_catalog::ExecutionNodePath;

use crate::{FlowFailureKind, NodeFailureKind, NodeTerminalStatus, RunTerminalStatus};

/// One assertion accepted by the frozen 0.1 test-set contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Assertion {
    RunTerminalOutcome(RunTerminalOutcome),
    TerminalRespond(TerminalRespond),
    TypedFlowFailure(TypedFlowFailure),
    NamedNodeTerminal(NamedNodeTerminal),
}

/// The required terminal status of a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RunTerminalOutcome {
    pub status: RunTerminalStatus,
}

/// The required response status and exact parsed JSON body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TerminalRespond {
    pub status: u16,
    pub body: Value,
}

/// The required typed flow failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TypedFlowFailure {
    pub kind: FlowFailureKind,
}

/// The required terminal result for every observed occurrence of one node path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct NamedNodeTerminal {
    pub path: ExecutionNodePath,
    pub status: NodeTerminalStatus,
    #[serde(
        default,
        deserialize_with = "deserialize_failure_kind",
        skip_serializing_if = "Option::is_none"
    )]
    pub failure_kind: Option<NodeFailureKind>,
}

fn deserialize_failure_kind<'de, D>(deserializer: D) -> Result<Option<NodeFailureKind>, D::Error>
where
    D: Deserializer<'de>,
{
    NodeFailureKind::deserialize(deserializer).map(Some)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AssertionErrorKind {
    ResponseStatus,
    NodeFailureKind,
}

#[derive(Debug)]
pub(crate) struct AssertionError {
    kind: AssertionErrorKind,
}

impl AssertionError {
    #[cfg(test)]
    pub(crate) const fn kind(&self) -> AssertionErrorKind {
        self.kind
    }
}

impl std::fmt::Display for AssertionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            AssertionErrorKind::ResponseStatus => {
                formatter.write_str("terminal respond status must be in 100..=599")
            }
            AssertionErrorKind::NodeFailureKind => formatter.write_str(
                "named-node success forbids failure-kind and error requires failure-kind",
            ),
        }
    }
}

impl std::error::Error for AssertionError {}

impl Assertion {
    pub(crate) fn validate(&self) -> Result<(), AssertionError> {
        match self {
            Self::TerminalRespond(response) if !(100..=599).contains(&response.status) => {
                Err(AssertionError {
                    kind: AssertionErrorKind::ResponseStatus,
                })
            }
            Self::NamedNodeTerminal(node)
                if matches!(
                    (node.status, node.failure_kind),
                    (NodeTerminalStatus::Success, Some(_)) | (NodeTerminalStatus::Error, None)
                ) =>
            {
                Err(AssertionError {
                    kind: AssertionErrorKind::NodeFailureKind,
                })
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        Assertion, AssertionErrorKind, NamedNodeTerminal, RunTerminalOutcome, TerminalRespond,
        TypedFlowFailure,
    };
    use crate::{FlowFailureKind, NodeFailureKind, NodeTerminalStatus, RunTerminalStatus};

    fn round_trip(assertion: Assertion, wire: Value) {
        assert_eq!(serde_json::to_value(&assertion).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<Assertion>(wire).unwrap(),
            assertion
        );
    }

    #[test]
    fn exact_four_wire_families_round_trip() {
        round_trip(
            Assertion::RunTerminalOutcome(RunTerminalOutcome {
                status: RunTerminalStatus::Completed,
            }),
            json!({"run-terminal-outcome": {"status": "completed"}}),
        );
        round_trip(
            Assertion::TerminalRespond(TerminalRespond {
                status: 201,
                body: json!({"created": true}),
            }),
            json!({"terminal-respond": {"status": 201, "body": {"created": true}}}),
        );
        round_trip(
            Assertion::TypedFlowFailure(TypedFlowFailure {
                kind: FlowFailureKind::EffectUncertain,
            }),
            json!({"typed-flow-failure": {"kind": "effect-uncertain"}}),
        );
        round_trip(
            Assertion::NamedNodeTerminal(NamedNodeTerminal {
                path: serde_json::from_value(json!(["normalize", "write"])).unwrap(),
                status: NodeTerminalStatus::Error,
                failure_kind: Some(NodeFailureKind::Terminal),
            }),
            json!({"named-node-terminal": {
                "path": ["normalize", "write"],
                "status": "error",
                "failure-kind": "terminal"
            }}),
        );
        round_trip(
            Assertion::NamedNodeTerminal(NamedNodeTerminal {
                path: serde_json::from_value(json!(["respond"])).unwrap(),
                status: NodeTerminalStatus::Success,
                failure_kind: None,
            }),
            json!({"named-node-terminal": {
                "path": ["respond"],
                "status": "success"
            }}),
        );
    }

    #[test]
    fn assertion_payloads_deny_unknown_fields() {
        for wire in [
            json!({"run-terminal-outcome": {"status": "completed", "extra": true}}),
            json!({"terminal-respond": {"status": 200, "body": null, "headers": {}}}),
            json!({"typed-flow-failure": {"kind": "terminal", "message": "no"}}),
            json!({"named-node-terminal": {"path": ["write"], "status": "success", "output": {}}}),
        ] {
            assert!(serde_json::from_value::<Assertion>(wire).is_err());
        }
    }

    #[test]
    fn semantic_validation_pins_response_and_node_terminal_invariants() {
        for status in [99, 600] {
            let error = Assertion::TerminalRespond(TerminalRespond {
                status,
                body: Value::Null,
            })
            .validate()
            .unwrap_err();
            assert_eq!(error.kind(), AssertionErrorKind::ResponseStatus);
        }
        for assertion in [
            Assertion::NamedNodeTerminal(NamedNodeTerminal {
                path: serde_json::from_value(json!(["write"])).unwrap(),
                status: NodeTerminalStatus::Success,
                failure_kind: Some(NodeFailureKind::Terminal),
            }),
            Assertion::NamedNodeTerminal(NamedNodeTerminal {
                path: serde_json::from_value(json!(["write"])).unwrap(),
                status: NodeTerminalStatus::Error,
                failure_kind: None,
            }),
        ] {
            assert_eq!(
                assertion.validate().unwrap_err().kind(),
                AssertionErrorKind::NodeFailureKind
            );
        }
    }
}

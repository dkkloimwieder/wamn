//! The single MVP test-case expectation shape.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::status::FlowFailureKind;

/// The one observable a test case may require of its run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Expect {
    pub outcome: ExpectedOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_subset: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<FlowFailureKind>,
}

/// The two terminal shapes a black-box test case distinguishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ExpectedOutcome {
    Responded,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExpectErrorKind {
    ResponseStatus,
    RespondedFailureCode,
    FailedResponseFields,
}

/// Why an [`Expect`] is refused. Read through [`std::fmt::Display`]; the
/// classification behind it is this crate's own concern.
#[derive(Debug)]
pub struct ExpectError {
    kind: ExpectErrorKind,
}

impl ExpectError {
    #[cfg(test)]
    pub(crate) const fn kind(&self) -> ExpectErrorKind {
        self.kind
    }
}

impl std::fmt::Display for ExpectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ExpectErrorKind::ResponseStatus => {
                formatter.write_str("expected status must be in 100..=599")
            }
            ExpectErrorKind::RespondedFailureCode => {
                formatter.write_str("a responded outcome forbids failure-code")
            }
            ExpectErrorKind::FailedResponseFields => {
                formatter.write_str("a failed outcome forbids status and body-subset")
            }
        }
    }
}

impl std::error::Error for ExpectError {}

impl Expect {
    /// `Ok` if the outcome and its optional fields are a legal pairing.
    pub fn validate(&self) -> Result<(), ExpectError> {
        if self
            .status
            .is_some_and(|status| !(100..=599).contains(&status))
        {
            return Err(ExpectError {
                kind: ExpectErrorKind::ResponseStatus,
            });
        }
        match self.outcome {
            ExpectedOutcome::Responded if self.failure_code.is_some() => Err(ExpectError {
                kind: ExpectErrorKind::RespondedFailureCode,
            }),
            ExpectedOutcome::Failed if self.status.is_some() || self.body_subset.is_some() => {
                Err(ExpectError {
                    kind: ExpectErrorKind::FailedResponseFields,
                })
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{Expect, ExpectErrorKind, ExpectedOutcome};
    use crate::status::FlowFailureKind;

    fn round_trip(expect: Expect, wire: Value) {
        assert_eq!(serde_json::to_value(&expect).unwrap(), wire);
        assert_eq!(serde_json::from_value::<Expect>(wire).unwrap(), expect);
    }

    #[test]
    fn the_one_wire_shape_round_trips() {
        round_trip(
            Expect {
                outcome: ExpectedOutcome::Responded,
                status: Some(201),
                body_subset: Some(json!({"created": true})),
                failure_code: None,
            },
            json!({
                "outcome": "responded",
                "status": 201,
                "body-subset": {"created": true}
            }),
        );
        round_trip(
            Expect {
                outcome: ExpectedOutcome::Failed,
                status: None,
                body_subset: None,
                failure_code: Some(FlowFailureKind::InvalidInput),
            },
            json!({"outcome": "failed", "failure-code": "invalid-input"}),
        );
        round_trip(
            Expect {
                outcome: ExpectedOutcome::Responded,
                status: None,
                body_subset: None,
                failure_code: None,
            },
            json!({"outcome": "responded"}),
        );
    }

    #[test]
    fn the_removed_families_and_unknown_fields_are_refused() {
        for wire in [
            json!({"run-terminal-outcome": {"status": "completed"}}),
            json!({"terminal-respond": {"status": 200, "body": null}}),
            json!({"typed-flow-failure": {"kind": "terminal"}}),
            json!({"named-node-terminal": {
                "flow-id": "orders", "node-id": "write", "status": "success"
            }}),
            json!({"outcome": "responded", "body": {}}),
            json!({"outcome": "responded", "flow-id": "orders"}),
            json!({"outcome": "completed"}),
            json!({"status": 200}),
        ] {
            assert!(serde_json::from_value::<Expect>(wire).is_err());
        }
    }

    #[test]
    fn semantic_validation_pins_the_outcome_field_pairing() {
        for status in [99, 600] {
            let error = Expect {
                outcome: ExpectedOutcome::Responded,
                status: Some(status),
                body_subset: None,
                failure_code: None,
            }
            .validate()
            .unwrap_err();
            assert_eq!(error.kind(), ExpectErrorKind::ResponseStatus);
        }
        assert_eq!(
            Expect {
                outcome: ExpectedOutcome::Responded,
                status: None,
                body_subset: None,
                failure_code: Some(FlowFailureKind::Terminal),
            }
            .validate()
            .unwrap_err()
            .kind(),
            ExpectErrorKind::RespondedFailureCode
        );
        for (status, body_subset) in [(Some(200), None), (None, Some(json!({})))] {
            assert_eq!(
                Expect {
                    outcome: ExpectedOutcome::Failed,
                    status,
                    body_subset,
                    failure_code: None,
                }
                .validate()
                .unwrap_err()
                .kind(),
                ExpectErrorKind::FailedResponseFields
            );
        }
    }
}

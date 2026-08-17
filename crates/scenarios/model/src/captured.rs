//! Captured facts consumed by the pure MVP test-case evaluator.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wamn_flow::FlowFailureKind;

/// The bounded facts one run makes available to the flat expectation.
///
/// The two fields are the terminal `wamn_run.runs` columns of the case's run:
/// `response` is `caller_http_status` plus `caller_outcome_json` when
/// `caller_outcome_kind` is `responded`, and `failure_code` is `fail_kind` when
/// it is `failed`. That column is single-valued, so at most one field is ever
/// set; [`Captured::responded`] and [`Captured::failed`] are the constructors
/// that hold to it, and [`Captured::validate`] refuses a shape no run row can
/// have (wamn-0h0g.15.77).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Captured {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<CapturedResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<FlowFailureKind>,
}

/// The terminal response a run produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CapturedResponse {
    pub status: u16,
    pub body: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CapturedErrorKind {
    ResponseStatus,
    ContradictoryOutcome,
}

/// Why captured facts are refused. Read through [`std::fmt::Display`]; the
/// classification behind it is this crate's own concern.
#[derive(Debug)]
pub struct CapturedError {
    kind: CapturedErrorKind,
}

impl CapturedError {
    #[cfg(test)]
    pub(crate) const fn kind(&self) -> CapturedErrorKind {
        self.kind
    }
}

impl std::fmt::Display for CapturedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            CapturedErrorKind::ResponseStatus => {
                formatter.write_str("captured status must be in 100..=599")
            }
            CapturedErrorKind::ContradictoryOutcome => {
                formatter.write_str("captured facts must not set both response and failure-code")
            }
        }
    }
}

impl std::error::Error for CapturedError {}

impl Captured {
    /// The facts of a run that released a terminal response.
    ///
    /// `status` carries the stored `runs.caller_http_status` bound, so a status
    /// no run row can hold is refused here instead of reaching the evaluator,
    /// which would report it as an ordinary status mismatch.
    pub fn responded(status: u16, body: Value) -> Result<Self, CapturedError> {
        let captured = Self {
            response: Some(CapturedResponse { status, body }),
            failure_code: None,
        };
        captured.validate()?;
        Ok(captured)
    }

    /// The facts of a run that failed.
    ///
    /// `failure_code` is `None` when the run's `fail_kind` is one of the
    /// storage-owned kinds no expectation can author; such a run then satisfies
    /// no `outcome: failed` expectation, loudly, instead of being reported as
    /// whichever authorable kind is nearest.
    pub fn failed(failure_code: Option<FlowFailureKind>) -> Self {
        Self {
            response: None,
            failure_code,
        }
    }

    /// `Ok` if these facts are a shape some run row can actually have.
    ///
    /// A producer should call this; [`crate::evaluate`] calls it regardless,
    /// because facts also arrive by deserializing a stored report summary.
    pub fn validate(&self) -> Result<(), CapturedError> {
        if self.response.is_some() && self.failure_code.is_some() {
            return Err(CapturedError {
                kind: CapturedErrorKind::ContradictoryOutcome,
            });
        }
        if let Some(response) = &self.response
            && !(100..=599).contains(&response.status)
        {
            return Err(CapturedError {
                kind: CapturedErrorKind::ResponseStatus,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_flow::FlowFailureKind;

    use super::{Captured, CapturedErrorKind, CapturedResponse};

    /// The `runs.fail_kind` vocabulary, mirroring `runs_fail_kind_check`
    /// (`crates/schema/control/src/run_plane.rs`, whose own drift guard ties
    /// that mirror to the deployed CHECK).
    const RUNS_FAIL_KIND_LITERALS: [&str; 12] = [
        "terminal",
        "retry-exhausted",
        "invalid-input",
        "runaway-budget",
        "effect-uncertain",
        "depth-budget",
        "dispatch-budget",
        "unresolvable-name",
        "hash-invalid-bytes",
        "foreign-revision",
        "incompatible-contract",
        "unbound-requirement",
    ];

    /// Exhaustive by construction: a new [`FlowFailureKind`] variant stops this
    /// match compiling until someone decides whether a run can be persisted
    /// with it at all.
    const fn authorable_literal(kind: FlowFailureKind) -> &'static str {
        match kind {
            FlowFailureKind::Terminal => "terminal",
            FlowFailureKind::RetryExhausted => "retry-exhausted",
            FlowFailureKind::InvalidInput => "invalid-input",
            FlowFailureKind::RunawayBudget => "runaway-budget",
            FlowFailureKind::EffectUncertain => "effect-uncertain",
            FlowFailureKind::DepthBudget => "depth-budget",
            FlowFailureKind::DispatchBudget => "dispatch-budget",
        }
    }

    #[test]
    fn a_responded_capture_carries_the_stored_status_bound() {
        for status in [100, 599] {
            let captured =
                Captured::responded(status, json!({})).expect("a storable status is accepted");
            assert_eq!(
                captured
                    .response
                    .expect("responded facts carry a response")
                    .status,
                status
            );
            assert!(captured.failure_code.is_none());
        }
        for status in [0, 99, 600, 1000] {
            assert_eq!(
                Captured::responded(status, json!({}))
                    .expect_err("an unstorable status is refused")
                    .kind(),
                CapturedErrorKind::ResponseStatus
            );
        }
    }

    #[test]
    fn validation_refuses_only_the_shape_no_run_row_can_have() {
        for facts in [
            Captured {
                response: Some(CapturedResponse {
                    status: 200,
                    body: json!({}),
                }),
                failure_code: Some(FlowFailureKind::Terminal),
            },
            serde_json::from_value(json!({
                "response": {"status": 200, "body": {}}, "failure-code": "terminal"
            }))
            .expect("the contradictory shape still parses"),
        ] {
            assert_eq!(
                facts
                    .validate()
                    .expect_err("caller-outcome-kind is single-valued")
                    .kind(),
                CapturedErrorKind::ContradictoryOutcome
            );
        }

        // A run whose fail_kind is unauthorable carries no failure-code, and a
        // reconciled case never reaches the evaluator at all. Both are legal
        // shapes that simply satisfy nothing.
        Captured::failed(None)
            .validate()
            .expect("an unauthorable failure is still a capture");
        Captured::default()
            .validate()
            .expect("absent facts are refused by evaluation, not by shape");
    }

    #[test]
    fn authorable_failure_codes_are_a_strict_subset_of_the_persisted_fail_kinds() {
        for kind in [
            FlowFailureKind::Terminal,
            FlowFailureKind::RetryExhausted,
            FlowFailureKind::InvalidInput,
            FlowFailureKind::RunawayBudget,
            FlowFailureKind::EffectUncertain,
            FlowFailureKind::DepthBudget,
            FlowFailureKind::DispatchBudget,
        ] {
            let literal = authorable_literal(kind);
            assert_eq!(
                serde_json::to_value(kind).expect("a failure kind serializes"),
                json!(literal)
            );
            assert!(
                RUNS_FAIL_KIND_LITERALS.contains(&literal),
                "{literal} is authorable but no run can be persisted with it"
            );
        }
        for storage_owned in [
            "unresolvable-name",
            "hash-invalid-bytes",
            "foreign-revision",
            "incompatible-contract",
            "unbound-requirement",
        ] {
            assert!(RUNS_FAIL_KIND_LITERALS.contains(&storage_owned));
            assert!(
                serde_json::from_value::<FlowFailureKind>(json!(storage_owned)).is_err(),
                "{storage_owned} must stay unauthorable"
            );
        }
    }
}

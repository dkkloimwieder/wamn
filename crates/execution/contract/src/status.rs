//! Frozen failure literals used by MVP test expectations.

use serde::{Deserialize, Serialize};

/// A typed flow failure accepted by `failure-code`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FlowFailureKind {
    Terminal,
    RetryExhausted,
    InvalidInput,
    RunawayBudget,
    EffectUncertain,
    DepthBudget,
    DispatchBudget,
}

#[cfg(test)]
mod tests {
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::json;

    use super::FlowFailureKind;

    fn assert_wire<T>(value: T, literal: &str)
    where
        T: Copy + std::fmt::Debug + PartialEq + Serialize + DeserializeOwned,
    {
        assert_eq!(serde_json::to_value(value).unwrap(), json!(literal));
        assert_eq!(serde_json::from_value::<T>(json!(literal)).unwrap(), value);
    }

    #[test]
    fn frozen_literals_round_trip_only_the_mvp_sets() {
        for (value, literal) in [
            (FlowFailureKind::Terminal, "terminal"),
            (FlowFailureKind::RetryExhausted, "retry-exhausted"),
            (FlowFailureKind::InvalidInput, "invalid-input"),
            (FlowFailureKind::RunawayBudget, "runaway-budget"),
            (FlowFailureKind::EffectUncertain, "effect-uncertain"),
            (FlowFailureKind::DepthBudget, "depth-budget"),
            (FlowFailureKind::DispatchBudget, "dispatch-budget"),
        ] {
            assert_wire(value, literal);
        }

        for removed in [
            "dispatched",
            "running",
            "cancelled",
            "completed",
            "success",
            "error",
            "retryable",
            "rate-limited",
            "infrastructure-failure",
        ] {
            assert!(serde_json::from_value::<FlowFailureKind>(json!(removed)).is_err());
        }
    }

    #[test]
    fn depth_budget_literal_is_frozen() {
        assert_wire(FlowFailureKind::DepthBudget, "depth-budget");
    }

    #[test]
    fn dispatch_budget_literal_is_frozen() {
        assert_wire(FlowFailureKind::DispatchBudget, "dispatch-budget");
    }
}

//! Frozen failure literals used by MVP test expectations.

use serde::{Deserialize, Serialize};

/// A typed wiring failure accepted by `failure-code`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WiringFailureKind {
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

    use super::WiringFailureKind;

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
            (WiringFailureKind::Terminal, "terminal"),
            (WiringFailureKind::RetryExhausted, "retry-exhausted"),
            (WiringFailureKind::InvalidInput, "invalid-input"),
            (WiringFailureKind::RunawayBudget, "runaway-budget"),
            (WiringFailureKind::EffectUncertain, "effect-uncertain"),
            (WiringFailureKind::DepthBudget, "depth-budget"),
            (WiringFailureKind::DispatchBudget, "dispatch-budget"),
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
            assert!(serde_json::from_value::<WiringFailureKind>(json!(removed)).is_err());
        }
    }

    #[test]
    fn depth_budget_literal_is_frozen() {
        assert_wire(WiringFailureKind::DepthBudget, "depth-budget");
    }

    #[test]
    fn dispatch_budget_literal_is_frozen() {
        assert_wire(WiringFailureKind::DispatchBudget, "dispatch-budget");
    }
}

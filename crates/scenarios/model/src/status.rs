//! Frozen status and failure literals used by MVP test assertions.

use serde::{Deserialize, Serialize};

/// A terminal run status accepted by `run-terminal-outcome`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunTerminalStatus {
    Completed,
    Failed,
    InfrastructureFailure,
    EffectUncertain,
}

/// A typed flow failure accepted by `typed-flow-failure`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// A terminal node status accepted by `named-node-terminal`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeTerminalStatus {
    Success,
    Error,
}

/// A typed node failure accepted when a named node terminates with `error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeFailureKind {
    Retryable,
    RateLimited,
    Terminal,
    InvalidInput,
}

#[cfg(test)]
mod tests {
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::json;

    use super::{FlowFailureKind, NodeFailureKind, NodeTerminalStatus, RunTerminalStatus};

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
            (RunTerminalStatus::Completed, "completed"),
            (RunTerminalStatus::Failed, "failed"),
            (
                RunTerminalStatus::InfrastructureFailure,
                "infrastructure-failure",
            ),
            (RunTerminalStatus::EffectUncertain, "effect-uncertain"),
        ] {
            assert_wire(value, literal);
        }
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
        for (value, literal) in [
            (NodeTerminalStatus::Success, "success"),
            (NodeTerminalStatus::Error, "error"),
        ] {
            assert_wire(value, literal);
        }
        for (value, literal) in [
            (NodeFailureKind::Retryable, "retryable"),
            (NodeFailureKind::RateLimited, "rate-limited"),
            (NodeFailureKind::Terminal, "terminal"),
            (NodeFailureKind::InvalidInput, "invalid-input"),
        ] {
            assert_wire(value, literal);
        }

        for removed in ["dispatched", "running", "cancelled"] {
            assert!(serde_json::from_value::<RunTerminalStatus>(json!(removed)).is_err());
            assert!(serde_json::from_value::<FlowFailureKind>(json!(removed)).is_err());
            assert!(serde_json::from_value::<NodeFailureKind>(json!(removed)).is_err());
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

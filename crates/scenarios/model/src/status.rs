//! Product scenario outcome taxonomies.
//!
//! These types describe assertions and captured facts. Storage and invocation
//! adapters translate their own boundary types at the scenario-model edge.

use serde::{Deserialize, Serialize};

/// A captured run's lifecycle status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    Dispatched,
    Running,
    Completed,
    Failed,
    InfrastructureFailure,
    EffectUncertain,
}

/// Why a captured run failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailKind {
    Terminal,
    RetryExhausted,
    InvalidInput,
    RunawayBudget,
    EffectUncertain,
}

/// A captured node failure classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeErrorKind {
    Retryable,
    RateLimited,
    Terminal,
    InvalidInput,
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn outcome_taxonomies_keep_their_wire_literals() {
        for (status, wire) in [
            (RunStatus::Dispatched, "dispatched"),
            (RunStatus::Running, "running"),
            (RunStatus::Completed, "completed"),
            (RunStatus::Failed, "failed"),
            (RunStatus::InfrastructureFailure, "infrastructure-failure"),
            (RunStatus::EffectUncertain, "effect-uncertain"),
        ] {
            assert_eq!(serde_json::to_value(status).unwrap(), json!(wire));
        }
        for (kind, wire) in [
            (FailKind::Terminal, "terminal"),
            (FailKind::RetryExhausted, "retry-exhausted"),
            (FailKind::InvalidInput, "invalid-input"),
            (FailKind::RunawayBudget, "runaway-budget"),
            (FailKind::EffectUncertain, "effect-uncertain"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(wire));
        }
        for (kind, wire) in [
            (NodeErrorKind::Retryable, "retryable"),
            (NodeErrorKind::RateLimited, "rate-limited"),
            (NodeErrorKind::Terminal, "terminal"),
            (NodeErrorKind::InvalidInput, "invalid-input"),
            (NodeErrorKind::Cancelled, "cancelled"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(wire));
        }
    }
}

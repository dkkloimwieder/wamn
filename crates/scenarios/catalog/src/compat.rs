//! Explicit adapters between scenario contracts and durable/runtime boundaries.
//!
//! Keeping these translations exhaustive makes parallel taxonomies visible at
//! compile time when either boundary adds a variant.

use wamn_node_invoke::{WireNodeError, WireRunContext};
use wamn_run_state::{
    FailKind as StoredFailKind, NodeErrorKind as StoredNodeErrorKind, RunStatus as StoredRunStatus,
};
use wamn_scenario_model::{FailKind, NodeErrorKind, RunContext, RunStatus};

/// Translate a durable run status into the scenario product contract.
pub fn run_status_from_store(status: StoredRunStatus) -> RunStatus {
    match status {
        StoredRunStatus::Dispatched => RunStatus::Dispatched,
        StoredRunStatus::Running => RunStatus::Running,
        StoredRunStatus::Completed => RunStatus::Completed,
        StoredRunStatus::Failed => RunStatus::Failed,
        StoredRunStatus::Cancelled => RunStatus::Cancelled,
        StoredRunStatus::InfrastructureFailure => RunStatus::InfrastructureFailure,
    }
}

/// Translate a scenario run status to its durable representation.
pub fn run_status_to_store(status: RunStatus) -> StoredRunStatus {
    match status {
        RunStatus::Dispatched => StoredRunStatus::Dispatched,
        RunStatus::Running => StoredRunStatus::Running,
        RunStatus::Completed => StoredRunStatus::Completed,
        RunStatus::Failed => StoredRunStatus::Failed,
        RunStatus::Cancelled => StoredRunStatus::Cancelled,
        RunStatus::InfrastructureFailure => StoredRunStatus::InfrastructureFailure,
    }
}

/// Translate a durable run failure into the scenario product contract.
pub fn fail_kind_from_store(kind: StoredFailKind) -> FailKind {
    match kind {
        StoredFailKind::Terminal => FailKind::Terminal,
        StoredFailKind::RetryExhausted => FailKind::RetryExhausted,
        StoredFailKind::InvalidInput => FailKind::InvalidInput,
        StoredFailKind::RunawayBudget => FailKind::RunawayBudget,
    }
}

/// Translate a scenario run failure to its durable representation.
pub fn fail_kind_to_store(kind: FailKind) -> StoredFailKind {
    match kind {
        FailKind::Terminal => StoredFailKind::Terminal,
        FailKind::RetryExhausted => StoredFailKind::RetryExhausted,
        FailKind::InvalidInput => StoredFailKind::InvalidInput,
        FailKind::RunawayBudget => StoredFailKind::RunawayBudget,
    }
}

/// Translate a durable node failure into the scenario product contract.
pub fn node_error_kind_from_store(kind: StoredNodeErrorKind) -> NodeErrorKind {
    match kind {
        StoredNodeErrorKind::Retryable => NodeErrorKind::Retryable,
        StoredNodeErrorKind::RateLimited => NodeErrorKind::RateLimited,
        StoredNodeErrorKind::Terminal => NodeErrorKind::Terminal,
        StoredNodeErrorKind::InvalidInput => NodeErrorKind::InvalidInput,
        StoredNodeErrorKind::Cancelled => NodeErrorKind::Cancelled,
    }
}

/// Translate a scenario node failure to its durable representation.
pub fn node_error_kind_to_store(kind: NodeErrorKind) -> StoredNodeErrorKind {
    match kind {
        NodeErrorKind::Retryable => StoredNodeErrorKind::Retryable,
        NodeErrorKind::RateLimited => StoredNodeErrorKind::RateLimited,
        NodeErrorKind::Terminal => StoredNodeErrorKind::Terminal,
        NodeErrorKind::InvalidInput => StoredNodeErrorKind::InvalidInput,
        NodeErrorKind::Cancelled => StoredNodeErrorKind::Cancelled,
    }
}

/// Classify an invocation-protocol node error for scenario assertions.
pub fn node_error_kind_from_wire(error: &WireNodeError) -> NodeErrorKind {
    match error {
        WireNodeError::Retryable(_) => NodeErrorKind::Retryable,
        WireNodeError::RateLimited(_) => NodeErrorKind::RateLimited,
        WireNodeError::Terminal(_) => NodeErrorKind::Terminal,
        WireNodeError::InvalidInput(_) => NodeErrorKind::InvalidInput,
        WireNodeError::Cancelled => NodeErrorKind::Cancelled,
    }
}

/// Translate the invocation protocol context into a scenario context.
pub fn run_context_from_wire(context: WireRunContext) -> RunContext {
    RunContext {
        run_id: context.run_id,
        flow_id: context.flow_id,
        flow_version: context.flow_version,
        node_id: context.node_id,
        attempt: context.attempt,
        idempotency_key: context.idempotency_key,
        deadline_ms: context.deadline_ms,
        traceparent: context.traceparent,
        tracestate: context.tracestate,
        config: context.config,
    }
}

/// Translate a scenario context into the invocation protocol context.
pub fn run_context_to_wire(context: RunContext) -> WireRunContext {
    WireRunContext {
        run_id: context.run_id,
        flow_id: context.flow_id,
        flow_version: context.flow_version,
        node_id: context.node_id,
        attempt: context.attempt,
        idempotency_key: context.idempotency_key,
        deadline_ms: context.deadline_ms,
        traceparent: context.traceparent,
        tracestate: context.tracestate,
        config: context.config,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_node_invoke::{WireErrorDetail, WireRateLimit};

    macro_rules! assert_same_wire {
        ($left:expr, $right:expr) => {
            assert_eq!(
                serde_json::to_value($left).unwrap(),
                serde_json::to_value($right).unwrap()
            )
        };
    }

    #[test]
    fn run_status_store_round_trip_preserves_every_wire_value() {
        for stored in StoredRunStatus::ALL {
            let scenario = run_status_from_store(stored);
            assert_same_wire!(&stored, &scenario);
            assert_eq!(run_status_to_store(scenario), stored);
        }
    }

    #[test]
    fn fail_kind_store_round_trip_preserves_every_wire_value() {
        for stored in StoredFailKind::ALL {
            let scenario = fail_kind_from_store(stored);
            assert_same_wire!(&stored, &scenario);
            assert_eq!(fail_kind_to_store(scenario), stored);
        }
    }

    #[test]
    fn node_error_store_round_trip_preserves_every_wire_value() {
        for stored in StoredNodeErrorKind::ALL {
            let scenario = node_error_kind_from_store(stored);
            assert_same_wire!(&stored, &scenario);
            assert_eq!(node_error_kind_to_store(scenario), stored);
        }
    }

    #[test]
    fn every_wire_node_error_maps_to_the_matching_scenario_class() {
        let detail = || WireErrorDetail {
            message: "detail".into(),
            code: None,
            data: None,
        };
        for (wire, expected) in [
            (WireNodeError::Retryable(detail()), NodeErrorKind::Retryable),
            (
                WireNodeError::RateLimited(WireRateLimit {
                    detail: detail(),
                    retry_after_ms: Some(100),
                    target_host: Some("example.test".into()),
                }),
                NodeErrorKind::RateLimited,
            ),
            (WireNodeError::Terminal(detail()), NodeErrorKind::Terminal),
            (
                WireNodeError::InvalidInput(detail()),
                NodeErrorKind::InvalidInput,
            ),
            (WireNodeError::Cancelled, NodeErrorKind::Cancelled),
        ] {
            assert_eq!(node_error_kind_from_wire(&wire), expected);
        }
    }

    #[test]
    fn run_context_wire_round_trip_preserves_every_field() {
        let wire = WireRunContext {
            run_id: "run".into(),
            flow_id: "flow".into(),
            flow_version: 7,
            node_id: "node".into(),
            attempt: 2,
            idempotency_key: "run:node:2".into(),
            deadline_ms: Some(123),
            traceparent: Some("trace".into()),
            tracestate: Some("state".into()),
            config: r#"{"mode":"safe"}"#.into(),
        };
        let scenario = run_context_from_wire(wire.clone());
        assert_same_wire!(&wire, &scenario);
        assert_eq!(run_context_to_wire(scenario), wire);
    }
}

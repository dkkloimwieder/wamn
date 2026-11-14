//! Map a terminal router outcome onto the production run-store boundary.

use crate::graph::{FailureType as RouterFailureType, Outcome, Verdict, WalkStatus};
use wamn_run_state::FailType;
use wamn_run_state::run_store::{
    ProductionCompletion, ProductionLeaseError, ProductionLeaseErrorType,
};

/// Boundary work selected from a terminal router outcome.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ProductionRouterAction {
    /// Commit the run/caller result through the existing fenced transition.
    Complete(ProductionCompletion),
    /// The emit publisher/admission join is owned by `wamn-0h0g.19.8`.
    Emit {
        event: serde_json::Value,
        dedup_id: String,
        entity: String,
        operation: wamn_event_wire::Op,
        node_id: String,
    },
    /// Cancellation is not a failure verdict; leave the lease to redelivery.
    Cancelled,
}

/// Translate the router taxonomy exactly once at the run-store boundary.
pub(crate) fn production_router_action(
    outcome: &Outcome,
) -> Result<ProductionRouterAction, ProductionLeaseError> {
    if outcome.status == WalkStatus::Running {
        return Err(ProductionLeaseError::new(
            ProductionLeaseErrorType::Contract,
            "map router outcome",
            "router-returned-running-outcome",
        ));
    }

    // A verdict may be recorded before the frontier empties. It is the owning
    // boundary truth even when later background work fails or cancels; that
    // later status is observability, not permission to suppress the first
    // caller response or publish.
    if outcome.verdict.is_some()
        && matches!(outcome.status, WalkStatus::Failed | WalkStatus::Cancelled)
    {
        tracing::warn!(
            status = ?outcome.status,
            failure_type = ?outcome.failure.as_ref().map(|failure| failure.type_),
            failure_node = outcome.failure.as_ref().map(|failure| failure.node.as_str()),
            "router first verdict stood after a later frontier outcome"
        );
    }
    match outcome.verdict.as_ref() {
        Some(Verdict::Respond { .. }) => {
            return Err(ProductionLeaseError::new(
                ProductionLeaseErrorType::Contract,
                "map router response",
                "router-response-without-result-owner",
            ));
        }
        Some(Verdict::Emit {
            event,
            dedup_id,
            entity,
            operation,
            node_id,
        }) => {
            return Ok(ProductionRouterAction::Emit {
                event: event.clone(),
                dedup_id: dedup_id.clone(),
                entity: entity.clone(),
                operation: *operation,
                node_id: node_id.clone(),
            });
        }
        Some(Verdict::Discard) => {
            return Ok(ProductionRouterAction::Complete(
                ProductionCompletion::completed(outcome.result.clone(), None),
            ));
        }
        None => {}
    }

    match outcome.status {
        WalkStatus::Completed => Err(ProductionLeaseError::new(
            ProductionLeaseErrorType::Contract,
            "map completed router outcome",
            "router-completed-without-verdict",
        )),
        WalkStatus::Failed => {
            let failure = outcome.failure.as_ref().ok_or_else(|| {
                ProductionLeaseError::new(
                    ProductionLeaseErrorType::Contract,
                    "map failed router outcome",
                    "router-failed-without-failure",
                )
            })?;
            let fail_type = persisted_router_failure(failure.type_);
            let code = failure
                .detail
                .code
                .as_deref()
                .unwrap_or_else(|| failure.type_.code());
            let mut error = serde_json::Map::from_iter([
                (
                    "code".to_owned(),
                    serde_json::Value::String(code.to_owned()),
                ),
                (
                    "message".to_owned(),
                    serde_json::Value::String(failure.detail.message.clone()),
                ),
                (
                    "node".to_owned(),
                    serde_json::Value::String(failure.node.clone()),
                ),
            ]);
            if let Some(data) = failure.detail.data.as_ref() {
                error.insert("data".to_owned(), data.clone());
            }
            let body = serde_json::Value::Object(serde_json::Map::from_iter([(
                "error".to_owned(),
                serde_json::Value::Object(error),
            )]));
            Ok(ProductionRouterAction::Complete(
                ProductionCompletion::failed(body, fail_type, None),
            ))
        }
        WalkStatus::Cancelled => Ok(ProductionRouterAction::Cancelled),
        WalkStatus::Running => unreachable!("running outcomes were refused above"),
    }
}

fn persisted_router_failure(type_: RouterFailureType) -> FailType {
    match type_ {
        RouterFailureType::RetryExhausted => FailType::RetryExhausted,
        RouterFailureType::InvalidInput => FailType::InvalidInput,
        RouterFailureType::HopLimit => FailType::RunawayBudget,
        RouterFailureType::Terminal
        | RouterFailureType::UnreleasedCaller
        | RouterFailureType::MissingDedupId
        | RouterFailureType::RespondWithoutCaller
        | RouterFailureType::SecondVerdict => FailType::Terminal,
    }
}

#[cfg(test)]
mod tests {
    use wamn_run_state::RunStatus;

    use super::*;

    #[test]
    fn detached_respond_still_has_no_result_owner() {
        let outcome = Outcome {
            status: WalkStatus::Completed,
            result: serde_json::Value::Null,
            failure: None,
            hops: 1,
            verdict: Some(Verdict::Respond {
                payload: serde_json::json!({"accepted": true}),
                node_id: "respond".into(),
            }),
        };

        let error = production_router_action(&outcome)
            .expect_err("detached delivery cannot own a response");
        assert_eq!(error.error_type(), ProductionLeaseErrorType::Contract);
    }

    #[test]
    fn router_failure_maps_once_to_run_and_caller_truth() {
        let outcome = Outcome {
            status: WalkStatus::Failed,
            result: serde_json::Value::Null,
            failure: Some(crate::graph::Failure {
                node: "validate".into(),
                type_: RouterFailureType::InvalidInput,
                detail: crate::graph::ErrorDetail::coded("bad-order", "order is malformed"),
            }),
            hops: 1,
            verdict: None,
        };
        let ProductionRouterAction::Complete(completion) =
            production_router_action(&outcome).expect("failed walk maps")
        else {
            panic!("failed walk must complete the run");
        };
        assert_eq!(completion.status(), RunStatus::Failed);
        assert_eq!(completion.fail_type(), Some(FailType::InvalidInput));
        assert_eq!(completion.result()["error"]["code"], "bad-order");
        assert_eq!(completion.result()["error"]["node"], "validate");
        assert!(completion.caller().is_none());
    }

    #[test]
    fn callerless_discard_completes_without_caller_projection() {
        let outcome = Outcome {
            status: WalkStatus::Completed,
            result: serde_json::json!({"ok": true}),
            failure: None,
            hops: 1,
            verdict: Some(Verdict::Discard),
        };
        let ProductionRouterAction::Complete(completion) =
            production_router_action(&outcome).expect("discard maps")
        else {
            panic!("discard must complete the run");
        };
        assert_eq!(completion.status(), RunStatus::Completed);
        assert_eq!(completion.fail_type(), None);
        assert_eq!(completion.caller(), None);
    }

    #[test]
    fn first_emit_verdict_wins_over_later_frontier_failure_or_cancellation() {
        for status in [WalkStatus::Failed, WalkStatus::Cancelled] {
            let outcome = Outcome {
                status,
                result: serde_json::Value::Null,
                failure: (status == WalkStatus::Failed).then(|| crate::graph::Failure {
                    node: "later".into(),
                    type_: RouterFailureType::SecondVerdict,
                    detail: crate::graph::ErrorDetail::coded(
                        "second-verdict",
                        "later frontier reached another terminal",
                    ),
                }),
                hops: 2,
                verdict: Some(Verdict::Emit {
                    event: serde_json::json!({"order": 42}),
                    dedup_id: "wiring-1:7:first:d1".into(),
                    entity: "orders".into(),
                    operation: wamn_event_wire::Op::Insert,
                    node_id: "first".into(),
                }),
            };

            assert_eq!(
                production_router_action(&outcome).expect("first verdict maps"),
                ProductionRouterAction::Emit {
                    event: serde_json::json!({"order": 42}),
                    dedup_id: "wiring-1:7:first:d1".into(),
                    entity: "orders".into(),
                    operation: wamn_event_wire::Op::Insert,
                    node_id: "first".into(),
                },
                "later {status:?} must not suppress the first emit verdict"
            );
        }
    }

    #[test]
    fn running_outcome_is_refused_even_if_it_carries_a_verdict() {
        let outcome = Outcome {
            status: WalkStatus::Running,
            result: serde_json::Value::Null,
            failure: None,
            hops: 1,
            verdict: Some(Verdict::Discard),
        };

        let error = production_router_action(&outcome)
            .expect_err("an in-progress router result is not a queue terminal");
        assert_eq!(error.error_type(), ProductionLeaseErrorType::Contract);
    }
}

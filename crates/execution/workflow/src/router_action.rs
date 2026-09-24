//! Map a terminal router outcome onto the production run-store boundary.

use wamn_router::{FailureKind as RouterFailureKind, Outcome, Verdict, WalkStatus};
use wamn_run_state::FailKind;
use wamn_run_state::run_store::{
    ProductionCallerOutcome, ProductionClaimError, ProductionClaimErrorKind, ProductionCompletion,
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
    },
    /// Cancellation is not a failure verdict; leave the lease to redelivery.
    Cancelled,
}

/// Translate the router taxonomy exactly once at the run-store boundary.
pub(crate) fn production_router_action(
    outcome: &Outcome,
    caller_attached: bool,
) -> Result<ProductionRouterAction, ProductionClaimError> {
    production_router_action_with_mode(
        outcome,
        if caller_attached {
            RouterResultMode::DurableCaller
        } else {
            RouterResultMode::Detached
        },
    )
}

/// Translate a management candidate response into the run result without
/// fabricating a synchronous durable caller.
pub(crate) fn production_router_result_action(
    outcome: &Outcome,
) -> Result<ProductionRouterAction, ProductionClaimError> {
    production_router_action_with_mode(outcome, RouterResultMode::StoredResult)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouterResultMode {
    Detached,
    DurableCaller,
    StoredResult,
}

fn production_router_action_with_mode(
    outcome: &Outcome,
    mode: RouterResultMode,
) -> Result<ProductionRouterAction, ProductionClaimError> {
    if outcome.status == WalkStatus::Running {
        return Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
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
            failure_kind = ?outcome.failure.as_ref().map(|failure| failure.kind),
            failure_node = outcome.failure.as_ref().map(|failure| failure.node.as_str()),
            "router first verdict stood after a later frontier outcome"
        );
    }
    match outcome.verdict.as_ref() {
        Some(Verdict::Respond { payload, node_id }) => match mode {
            RouterResultMode::StoredResult => {
                return Ok(ProductionRouterAction::Complete(
                    ProductionCompletion::completed(payload.clone(), None),
                ));
            }
            RouterResultMode::DurableCaller => {
                let caller =
                    ProductionCallerOutcome::responded(payload.clone(), 200, node_id.clone());
                return Ok(ProductionRouterAction::Complete(
                    ProductionCompletion::completed(payload.clone(), Some(caller)),
                ));
            }
            RouterResultMode::Detached => {
                return Err(ProductionClaimError::new(
                    ProductionClaimErrorKind::Contract,
                    "map router response",
                    "router-response-without-result-owner",
                ));
            }
        },
        Some(Verdict::Emit { event, .. }) if mode == RouterResultMode::StoredResult => {
            return Ok(ProductionRouterAction::Complete(
                ProductionCompletion::completed(event.clone(), None),
            ));
        }
        Some(Verdict::Emit {
            event,
            dedup_id,
            entity,
            operation,
            ..
        }) => {
            return Ok(ProductionRouterAction::Emit {
                event: event.clone(),
                dedup_id: dedup_id.clone(),
                entity: entity.clone(),
                operation: *operation,
            });
        }
        Some(Verdict::Discard) if mode != RouterResultMode::DurableCaller => {
            return Ok(ProductionRouterAction::Complete(
                ProductionCompletion::completed(outcome.result.clone(), None),
            ));
        }
        Some(Verdict::Discard) => {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "map router discard",
                "router-discard-with-caller",
            ));
        }
        None => {}
    }

    match outcome.status {
        WalkStatus::Completed => Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "map completed router outcome",
            "router-completed-without-verdict",
        )),
        WalkStatus::Failed => {
            let failure = outcome.failure.as_ref().ok_or_else(|| {
                ProductionClaimError::new(
                    ProductionClaimErrorKind::Contract,
                    "map failed router outcome",
                    "router-failed-without-failure",
                )
            })?;
            let fail_kind = persisted_router_failure(failure.kind);
            let code = failure
                .detail
                .code
                .as_deref()
                .unwrap_or_else(|| router_failure_code(failure.kind));
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
            let caller = (mode == RouterResultMode::DurableCaller).then(|| {
                ProductionCallerOutcome::failed(body.clone(), 500, Some(failure.node.clone()))
            });
            Ok(ProductionRouterAction::Complete(
                ProductionCompletion::failed(body, fail_kind, caller),
            ))
        }
        WalkStatus::Cancelled => Ok(ProductionRouterAction::Cancelled),
        WalkStatus::Running => unreachable!("running outcomes were refused above"),
    }
}

fn persisted_router_failure(kind: RouterFailureKind) -> FailKind {
    match kind {
        RouterFailureKind::RetryExhausted => FailKind::RetryExhausted,
        RouterFailureKind::InvalidInput => FailKind::InvalidInput,
        RouterFailureKind::HopLimit => FailKind::RunawayBudget,
        RouterFailureKind::Terminal
        | RouterFailureKind::UnreleasedCaller
        | RouterFailureKind::MissingDedupId
        | RouterFailureKind::RespondWithoutCaller
        | RouterFailureKind::SecondVerdict => FailKind::Terminal,
    }
}

fn router_failure_code(kind: RouterFailureKind) -> &'static str {
    match kind {
        RouterFailureKind::Terminal => "terminal",
        RouterFailureKind::RetryExhausted => "retry-exhausted",
        RouterFailureKind::InvalidInput => "invalid-input",
        RouterFailureKind::HopLimit => "hop-limit",
        RouterFailureKind::UnreleasedCaller => "unreleased-caller",
        RouterFailureKind::MissingDedupId => "missing-dedup-id",
        RouterFailureKind::RespondWithoutCaller => "respond-without-caller",
        RouterFailureKind::SecondVerdict => "second-verdict",
    }
}

#[cfg(test)]
mod tests {
    use wamn_run_state::RunStatus;

    use super::*;

    #[test]
    fn candidate_respond_is_stored_without_fabricating_a_durable_caller() {
        let payload = serde_json::json!({"accepted": true});
        let outcome = Outcome {
            status: WalkStatus::Completed,
            result: serde_json::Value::Null,
            failure: None,
            hops: 1,
            verdict: Some(Verdict::Respond {
                payload: payload.clone(),
                node_id: "candidate-respond".into(),
            }),
        };
        let ProductionRouterAction::Complete(completion) =
            production_router_result_action(&outcome).expect("candidate response maps")
        else {
            panic!("candidate response must complete the run");
        };
        assert_eq!(completion.status(), RunStatus::Completed);
        assert_eq!(completion.result(), &payload);
        assert!(completion.caller().is_none());
    }

    #[test]
    fn durable_respond_carries_the_host_terminal_node_through_completion() {
        let payload = serde_json::json!({
            "accepted": true,
            "release-node-id": "guest-forged"
        });
        for status in [
            WalkStatus::Completed,
            WalkStatus::Failed,
            WalkStatus::Cancelled,
        ] {
            let outcome = Outcome {
                status,
                result: serde_json::Value::Null,
                failure: (status == WalkStatus::Failed).then(|| wamn_router::Failure {
                    node: "later".into(),
                    kind: RouterFailureKind::Terminal,
                    detail: wamn_router::ErrorDetail::msg("later work failed"),
                }),
                hops: 2,
                verdict: Some(Verdict::Respond {
                    payload: payload.clone(),
                    node_id: "wiring-terminal".into(),
                }),
            };
            let ProductionRouterAction::Complete(completion) =
                production_router_action(&outcome, true).expect("durable response maps")
            else {
                panic!("durable response must complete the queue run");
            };

            assert_eq!(completion.status(), RunStatus::Completed);
            assert_eq!(completion.result(), &payload);
            let caller = completion.caller().expect("durable caller is released");
            assert_eq!(caller.kind(), "responded");
            assert_eq!(caller.body(), &payload);
            assert_eq!(caller.http_status(), 200);
            assert_eq!(caller.release_node_id(), Some("wiring-terminal"));
        }
    }

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

        let error = production_router_action(&outcome, false)
            .expect_err("detached delivery cannot own a response");
        assert_eq!(error.kind(), ProductionClaimErrorKind::Contract);
        assert!(
            error
                .to_string()
                .contains("router-response-without-result-owner")
        );
    }

    #[test]
    fn router_failure_maps_once_to_run_and_caller_truth() {
        let outcome = Outcome {
            status: WalkStatus::Failed,
            result: serde_json::Value::Null,
            failure: Some(wamn_router::Failure {
                node: "validate".into(),
                kind: RouterFailureKind::InvalidInput,
                detail: wamn_router::ErrorDetail::coded("bad-order", "order is malformed"),
            }),
            hops: 1,
            verdict: None,
        };
        let ProductionRouterAction::Complete(completion) =
            production_router_action(&outcome, true).expect("failed walk maps")
        else {
            panic!("failed walk must complete the run");
        };
        assert_eq!(completion.status(), RunStatus::Failed);
        assert_eq!(completion.fail_kind(), Some(FailKind::InvalidInput));
        assert_eq!(completion.result()["error"]["code"], "bad-order");
        assert_eq!(completion.result()["error"]["node"], "validate");
        let caller = completion.caller().expect("attached caller gets failure");
        assert_eq!(caller.kind(), "failed");
        assert_eq!(caller.release_node_id(), Some("validate"));
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
            production_router_action(&outcome, false).expect("discard maps")
        else {
            panic!("discard must complete the run");
        };
        assert_eq!(completion.status(), RunStatus::Completed);
        assert_eq!(completion.fail_kind(), None);
        assert_eq!(completion.caller(), None);
    }

    #[test]
    fn first_emit_verdict_wins_over_later_frontier_failure_or_cancellation() {
        for status in [WalkStatus::Failed, WalkStatus::Cancelled] {
            let outcome = Outcome {
                status,
                result: serde_json::Value::Null,
                failure: (status == WalkStatus::Failed).then(|| wamn_router::Failure {
                    node: "later".into(),
                    kind: RouterFailureKind::SecondVerdict,
                    detail: wamn_router::ErrorDetail::coded(
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
                production_router_action(&outcome, false).expect("first verdict maps"),
                ProductionRouterAction::Emit {
                    event: serde_json::json!({"order": 42}),
                    dedup_id: "wiring-1:7:first:d1".into(),
                    entity: "orders".into(),
                    operation: wamn_event_wire::Op::Insert,
                },
                "later {status:?} must not suppress the first emit verdict"
            );
        }
    }

    #[test]
    fn candidate_emit_is_a_stored_observable_not_a_boundary_effect() {
        let event = serde_json::json!({"order": 42, "dedup-id": "d1"});
        let outcome = Outcome {
            status: WalkStatus::Completed,
            result: serde_json::Value::Null,
            failure: None,
            hops: 1,
            verdict: Some(Verdict::Emit {
                event: event.clone(),
                dedup_id: "d1".into(),
                entity: "orders".into(),
                operation: wamn_event_wire::Op::Insert,
                node_id: "entry".into(),
            }),
        };
        let ProductionRouterAction::Complete(completion) =
            production_router_result_action(&outcome).expect("candidate observable maps")
        else {
            panic!("candidate emit must not request production publication");
        };
        assert_eq!(completion.result(), &event);
        assert!(completion.caller().is_none());
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

        let error = production_router_action(&outcome, false)
            .expect_err("an in-progress router result is not a queue terminal");
        assert_eq!(error.kind(), ProductionClaimErrorKind::Contract);
        assert!(
            error
                .to_string()
                .contains("router-returned-running-outcome")
        );
    }
}

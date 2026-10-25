//! The wiring arm of the delivery bridge. The router driver walks the wiring
//! and the bridge settles the delivery. A route never reaches this module.

use opentelemetry::KeyValue;
use wamn_engine::router_delivery::{
    DeliveryClass, DeliveryError, DeliveryFailure, DeliveryOutcome,
    EffectOutcome as WireEffectOutcome, Emission, FailedOutcome, FailureType as WireFailureType,
    OperationRefusal, PartialCompletion, ROUTER_DELIVERY_ID, SourceRef, lower_operation_refusal,
};
use wamn_event_wire::Causation;
use wamn_execution_host::{RouterDeliveryBridge, WiringCall, WiringDelivery, WiringPreload};
use wamn_router::{FailureKind, Outcome, Verdict, WalkStatus};
use wamn_runtime::plugins::wamn_jetstream::DerivedPublishRequest;

use crate::router_response::{InterruptedResponse, PartialEvidence};
use crate::{RouterDriver, RouterDriverRequest};

#[async_trait::async_trait]
impl WiringDelivery for RouterDriver {
    async fn preload(&self) -> anyhow::Result<WiringPreload> {
        self.preload_synchronous_wirings().await
    }

    async fn deliver(
        &self,
        bridge: &RouterDeliveryBridge,
        call: WiringCall<'_>,
    ) -> Result<DeliveryOutcome, DeliveryError> {
        let WiringCall {
            source,
            delivery_id,
            package_id,
            wiring_id,
            wiring_version,
            caller_attached,
            payload,
            caller,
            traceparent,
            tracestate,
            causation,
            attributes,
            deadline_adjustments,
        } = call;
        if matches!(source, SourceRef::Registration(_)) {
            // Event workflows enter through the host batch admission boundary.
            return Err(DeliveryError::SourceNotFound);
        }
        let release = bridge.release().scope();
        let request = RouterDriverRequest {
            tenant_id: release.tenant_id.clone(),
            package_id: package_id.to_owned(),
            environment: release.environment.clone(),
            wiring_id: wiring_id.to_owned(),
            wiring_version,
            delivery_id: delivery_id.clone(),
            payload,
            caller_attached,
            caller,
            traceparent,
            tracestate,
        };
        let result = self
            .execute_with_causation(request, causation.clone(), source.platform())
            .await;
        *deadline_adjustments = match &result {
            Ok(delivery) => delivery.deadline_adjustments.clone(),
            Err(error) => error
                .downcast_ref::<crate::DeadlineAdjustments>()
                .map(|adjustments| adjustments.0.clone())
                .unwrap_or_default(),
        };
        match result {
            Ok(delivery) => {
                bridge.record(attributes, DeliveryClass::Delivered);
                publish_emit(
                    bridge,
                    package_id,
                    wiring_id,
                    wiring_version,
                    &delivery.outcome,
                    causation,
                )
                .await?;
                lower_with_evidence(delivery.outcome, delivery.partial)
            }
            Err(error) => refuse(bridge, attributes, &error),
        }
    }
}

/// Settle a delivery that the driver refused or failed to execute. A failure
/// after a declared committed result keeps its evidence. Every other refusal
/// settles as a route refusal does.
fn refuse(
    bridge: &RouterDeliveryBridge,
    attributes: &[KeyValue],
    error: &anyhow::Error,
) -> Result<DeliveryOutcome, DeliveryError> {
    if let Some(interrupted) = error.downcast_ref::<InterruptedResponse>() {
        let failure = interrupted
            .source
            .downcast_ref::<OperationRefusal>()
            .map_or(DeliveryError::ExecutionFailed, lower_operation_refusal);
        bridge.record(attributes, DeliveryClass::ExecutionFailed);
        tracing::warn!(error = %format_args!("{:#}", interrupted.source), "router delivery failed after a declared committed result");
        return partial_outcome(&interrupted.evidence, FailedOutcome::Error(failure));
    }
    bridge.refuse(attributes, error)
}

/// The wiring position this bridge resolved before the walk names the
/// publication on its effect span, and the node comes from the verdict the
/// walk recorded. A route never emits.
async fn publish_emit(
    bridge: &RouterDeliveryBridge,
    package_id: &str,
    wiring_id: &str,
    wiring_version: u32,
    outcome: &Outcome,
    causation: Causation,
) -> Result<(), DeliveryError> {
    let Some(Verdict::Emit {
        event,
        dedup_id,
        entity,
        operation,
        node_id,
    }) = outcome.verdict.as_ref()
    else {
        return Ok(());
    };
    bridge
        .jetstream()
        .publish_derived(DerivedPublishRequest {
            component_id: ROUTER_DELIVERY_ID.to_owned(),
            package_id: package_id.to_owned(),
            wiring_id: wiring_id.to_owned(),
            wiring_version,
            node_id: node_id.clone(),
            entity: entity.clone(),
            operation: *operation,
            payload: event.clone(),
            dedup_id: dedup_id.clone(),
            causation,
        })
        .await
        .map(|_| ())
        .map_err(|error| {
            tracing::warn!(
                error = %error,
                error_kind = ?error.error_type(),
                "derived-event publication did not receive a server ACK"
            );
            DeliveryError::ExecutionFailed
        })
}

pub(crate) fn lower_with_evidence(
    outcome: Outcome,
    evidence: Option<PartialEvidence>,
) -> Result<DeliveryOutcome, DeliveryError> {
    let lowered = lower_outcome(outcome);
    match (lowered, evidence) {
        (Ok(DeliveryOutcome::Failed(failure)), Some(evidence)) => {
            partial_outcome(&evidence, FailedOutcome::Failed(failure))
        }
        (Ok(DeliveryOutcome::Cancelled), Some(evidence)) => {
            partial_outcome(&evidence, FailedOutcome::Cancelled)
        }
        (Err(error), Some(evidence)) => partial_outcome(&evidence, FailedOutcome::Error(error)),
        (outcome, _) => outcome,
    }
}

fn partial_outcome(
    evidence: &PartialEvidence,
    failed_outcome: FailedOutcome,
) -> Result<DeliveryOutcome, DeliveryError> {
    let committed_result = serde_json::to_string(&evidence.committed_result)
        .map_err(|_| DeliveryError::ExecutionFailed)?;
    let effect_outcome = evidence.effect_outcome.map(|outcome| match outcome {
        wamn_execution_contract::EffectOutcome::RefusedBeforeDispatch => {
            WireEffectOutcome::RefusedBeforeDispatch
        }
        wamn_execution_contract::EffectOutcome::Responded => WireEffectOutcome::Responded,
        wamn_execution_contract::EffectOutcome::Timeout => WireEffectOutcome::Timeout,
        wamn_execution_contract::EffectOutcome::Cancelled => WireEffectOutcome::Cancelled,
        wamn_execution_contract::EffectOutcome::EffectUncertain => {
            WireEffectOutcome::EffectUncertain
        }
        wamn_execution_contract::EffectOutcome::ResponseLost => WireEffectOutcome::ResponseLost,
    });
    Ok(DeliveryOutcome::PartiallyCompleted(PartialCompletion {
        committed_result,
        failed_outcome,
        effect_outcome,
    }))
}

pub(crate) fn lower_outcome(outcome: Outcome) -> Result<DeliveryOutcome, DeliveryError> {
    if matches!(outcome.status, WalkStatus::Running) {
        return Err(DeliveryError::ExecutionFailed);
    }
    if let Some(verdict) = outcome.verdict {
        // A terminal may settle the delivery before the rest of the frontier
        // drains. If later work fails (including SecondVerdict), the router's
        // explicit invariant is that the first verdict stands. Preserve that
        // caller truth and keep the later failure observable host-side.
        if let Some(failure) = outcome.failure {
            tracing::warn!(
                failure_kind = ?failure.kind,
                failure_code = failure.detail.code.as_deref(),
                failure_message = %failure.detail.message,
                "router delivery failed after its terminal verdict; first verdict stands"
            );
        }
        return lower_verdict(verdict);
    }
    match outcome.status {
        WalkStatus::Failed => outcome
            .failure
            .map(|failure| {
                DeliveryOutcome::Failed(DeliveryFailure {
                    failure_type: lower_failure_type(failure.kind),
                    code: failure.detail.code,
                    message: failure.detail.message,
                })
            })
            .ok_or(DeliveryError::ExecutionFailed),
        WalkStatus::Cancelled => Ok(DeliveryOutcome::Cancelled),
        WalkStatus::Completed | WalkStatus::Running => Err(DeliveryError::ExecutionFailed),
    }
}

fn lower_verdict(verdict: Verdict) -> Result<DeliveryOutcome, DeliveryError> {
    match verdict {
        Verdict::Respond { payload, .. } => serde_json::to_string(&payload)
            .map(DeliveryOutcome::Respond)
            .map_err(|_| DeliveryError::ExecutionFailed),
        Verdict::Emit {
            event, dedup_id, ..
        } => serde_json::to_string(&event)
            .map(|event| DeliveryOutcome::Emit(Emission { event, dedup_id }))
            .map_err(|_| DeliveryError::ExecutionFailed),
        Verdict::Discard => Ok(DeliveryOutcome::Discard),
    }
}

fn lower_failure_type(kind: FailureKind) -> WireFailureType {
    match kind {
        FailureKind::Terminal => WireFailureType::Terminal,
        FailureKind::RetryExhausted => WireFailureType::RetryExhausted,
        FailureKind::InvalidInput => WireFailureType::InvalidInput,
        FailureKind::HopLimit => WireFailureType::HopLimit,
        FailureKind::UnreleasedCaller => WireFailureType::UnreleasedCaller,
        FailureKind::MissingDedupId => WireFailureType::MissingDedupId,
        FailureKind::RespondWithoutCaller => WireFailureType::RespondWithoutCaller,
        FailureKind::SecondVerdict => WireFailureType::SecondVerdict,
    }
}

#[cfg(test)]
mod tests {
    use wamn_engine::operation::node_types;
    use wamn_engine::router_delivery::settle_route;
    use wamn_router::{ErrorDetail, Failure};

    use super::*;

    #[test]
    fn terminal_mapping_preserves_each_router_class_without_node_coordinates() {
        let outcome = Outcome {
            status: WalkStatus::Failed,
            result: serde_json::Value::Null,
            failure: Some(Failure {
                node: "retired-coordinate".into(),
                kind: FailureKind::InvalidInput,
                detail: ErrorDetail::coded("bad-order", "order is invalid"),
            }),
            hops: 1,
            verdict: None,
        };

        let DeliveryOutcome::Failed(failure) = lower_outcome(outcome).expect("failure maps") else {
            panic!("failed walk must remain a failed delivery")
        };
        assert!(matches!(
            failure.failure_type,
            WireFailureType::InvalidInput
        ));
        assert_eq!(failure.code.as_deref(), Some("bad-order"));
        assert_eq!(failure.message, "order is invalid");
    }

    /// A route answers each node outcome exactly as its one-node respond
    /// wiring does. A retryable or rate-limited error reads as the wiring's
    /// failure after its last attempt, because a route never retries.
    #[test]
    fn a_route_settles_each_node_outcome_as_its_one_node_wiring() {
        let detail = || node_types::ErrorDetail {
            message: "order is invalid".into(),
            code: Some("bad-order".into()),
        };
        let failed = |kind| Outcome {
            status: WalkStatus::Failed,
            result: serde_json::Value::Null,
            failure: Some(Failure {
                node: "operation".into(),
                kind,
                detail: ErrorDetail::coded("bad-order", "order is invalid"),
            }),
            hops: 1,
            verdict: None,
        };
        let cases = [
            (
                Ok(node_types::Emission {
                    payload: r#"{"accepted":true}"#.into(),
                    port: None,
                }),
                Outcome {
                    status: WalkStatus::Completed,
                    result: serde_json::json!({"accepted": true}),
                    failure: None,
                    hops: 1,
                    verdict: Some(Verdict::Respond {
                        payload: serde_json::json!({"accepted": true}),
                        node_id: "operation".into(),
                    }),
                },
            ),
            (
                Err(node_types::NodeError::Retryable(detail())),
                failed(FailureKind::RetryExhausted),
            ),
            (
                Err(node_types::NodeError::RateLimited(
                    node_types::RateLimitDetail {
                        detail: detail(),
                        retry_after_ms: Some(10),
                    },
                )),
                failed(FailureKind::RetryExhausted),
            ),
            (
                Err(node_types::NodeError::Terminal(detail())),
                failed(FailureKind::Terminal),
            ),
            (
                Err(node_types::NodeError::InvalidInput(detail())),
                failed(FailureKind::InvalidInput),
            ),
            (
                Err(node_types::NodeError::Cancelled),
                outcome_of(WalkStatus::Cancelled, None),
            ),
        ];
        for (route, wiring) in cases {
            let settled = settle_route(route).expect("every node outcome settles");
            assert_eq!(
                format!("{:?}", settled.outcome),
                format!(
                    "{:?}",
                    lower_outcome(wiring).expect("the wiring outcome lowers")
                ),
            );
        }

        assert!(
            settle_route(Ok(node_types::Emission {
                payload: "not json".into(),
                port: None,
            }))
            .is_err(),
            "invalid JSON is an execution failure, as on the wiring path"
        );
    }

    #[test]
    fn a_first_verdict_stands_when_later_frontier_work_fails() {
        let outcome = Outcome {
            status: WalkStatus::Failed,
            result: serde_json::Value::Null,
            failure: Some(Failure {
                node: "later-terminal".into(),
                kind: FailureKind::SecondVerdict,
                detail: ErrorDetail::coded("second-verdict", "later terminal refused"),
            }),
            hops: 2,
            verdict: Some(Verdict::Respond {
                payload: serde_json::json!({"accepted": true}),
                node_id: "respond".into(),
            }),
        };

        let DeliveryOutcome::Respond(payload) =
            lower_outcome(outcome).expect("the first verdict remains caller truth")
        else {
            panic!("a later failure must not replace the first terminal verdict")
        };
        assert_eq!(payload, r#"{"accepted":true}"#);
    }

    // ---- the instruments ---------------------------------------------------

    fn outcome_of(status: WalkStatus, verdict: Option<Verdict>) -> Outcome {
        Outcome {
            status,
            result: serde_json::Value::Null,
            failure: None,
            hops: 1,
            verdict,
        }
    }
}

#[cfg(test)]
mod partial_tests {
    use super::*;
    use serde_json::json;

    fn evidence() -> PartialEvidence {
        PartialEvidence {
            committed_result: json!([{"request_id":"move-1","value":{"movement_id":"movement-1"}}]),
            effect_outcome: Some(wamn_execution_contract::EffectOutcome::ResponseLost),
        }
    }

    #[test]
    fn declared_evidence_crosses_the_bridge_with_the_original_failure() {
        let outcome = Outcome {
            status: WalkStatus::Failed,
            result: json!({"unselected":"must not cross the boundary"}),
            failure: Some(wamn_router::Failure {
                node: "store".to_owned(),
                kind: FailureKind::Terminal,
                detail: wamn_router::ErrorDetail::coded("write_failed", "label store failed"),
            }),
            hops: 3,
            verdict: None,
        };
        let DeliveryOutcome::PartiallyCompleted(partial) =
            lower_with_evidence(outcome, Some(evidence())).unwrap()
        else {
            panic!("a declared committed result must survive the downstream failure")
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&partial.committed_result).unwrap(),
            evidence().committed_result
        );
        assert!(matches!(
            partial.effect_outcome,
            Some(WireEffectOutcome::ResponseLost)
        ));
        let FailedOutcome::Failed(failure) = partial.failed_outcome else {
            panic!("original node failure")
        };
        assert!(matches!(failure.failure_type, WireFailureType::Terminal));
        assert_eq!(failure.code.as_deref(), Some("write_failed"));
        assert_eq!(failure.message, "label store failed");
    }

    #[test]
    fn existing_verdict_still_wins_over_later_partial_evidence() {
        let outcome = Outcome {
            status: WalkStatus::Failed,
            result: json!({"later":"ignored"}),
            failure: None,
            hops: 3,
            verdict: Some(Verdict::Respond {
                node_id: "respond".to_owned(),
                payload: json!({"first":true}),
            }),
        };
        let DeliveryOutcome::Respond(payload) =
            lower_with_evidence(outcome, Some(evidence())).unwrap()
        else {
            panic!("first verdict stands")
        };
        assert_eq!(payload, r#"{"first":true}"#);
    }
}

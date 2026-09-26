//! The wiring arm of the delivery bridge. The router driver walks the wiring
//! and the bridge settles the delivery. A route never reaches this module.

use std::borrow::Cow;

use opentelemetry::KeyValue;
use wamn_catalog::{AttachmentTarget, RegistrationDelivery};
use wamn_engine::router_delivery::{
    DeliveryClass, DeliveryError, DeliveryFailure, DeliveryOutcome, EXECUTION_FAILED,
    EffectOutcome as WireEffectOutcome, Emission, FailedOutcome, FailureKind as WireFailureKind,
    OperationRefusal, PartialCompletion, ROUTER_DELIVERY_ID, SourceRef, lower_operation_refusal,
};
use wamn_event_wire::Causation;
use wamn_execution_host::{RouterDeliveryBridge, WiringCall, WiringDelivery, WiringPreload};
use wamn_router::{FailureKind, Outcome, Verdict, WalkStatus};
use wamn_runtime::plugins::wamn_jetstream::{DerivedPublishRequest, RouterTapPhase};
use wamn_runtime::plugins::wamn_postgres::{EventRunAdmission, EventRunAdmitted};

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
            target,
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
        if let SourceRef::Registration(registration_id) = source
            && bridge
                .release()
                .manifest()
                .workflow
                .registrations
                .get(registration_id)
                .is_some_and(|registration| registration.delivery == RegistrationDelivery::Queue)
        {
            return self
                .admit_event_run(
                    bridge,
                    EventCall {
                        registration_id,
                        package_id,
                        wiring_id,
                        wiring_version,
                        delivery_id: &delivery_id,
                        payload: &payload,
                        attributes,
                    },
                )
                .await;
        }
        let release = &bridge.release().manifest().release;
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
                let (outcome, result) = settled_preview(&delivery.outcome);
                bridge
                    .tap(
                        source,
                        &delivery_id,
                        target,
                        RouterTapPhase::Settled(outcome),
                        &result,
                    )
                    .await;
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
            Err(error) => refuse(bridge, source, &delivery_id, target, attributes, &error).await,
        }
    }
}

/// One delivery of a workflow registration, which the host admits as a run.
struct EventCall<'a> {
    registration_id: &'a str,
    package_id: &'a str,
    wiring_id: &'a str,
    wiring_version: u32,
    delivery_id: &'a str,
    payload: &'a serde_json::Value,
    attributes: &'a [KeyValue],
}

impl RouterDriver {
    /// Admit a workflow registration's event as a queued run, under the
    /// authority of the release that declares the workflow (`wamn-upl3.6`).
    /// The queue runs it with no caller. The delivery id is the idempotency
    /// key, so a redelivered event admits no second run.
    async fn admit_event_run(
        &self,
        bridge: &RouterDeliveryBridge,
        call: EventCall<'_>,
    ) -> Result<DeliveryOutcome, DeliveryError> {
        let manifest = bridge.release().manifest();
        let Some(wiring) = manifest.workflow.wirings.iter().find(|wiring| {
            wiring.package_id == call.package_id
                && wiring.wiring_id == call.wiring_id
                && wiring.wiring_version == call.wiring_version
        }) else {
            tracing::warn!(
                registration = call.registration_id,
                "a workflow registration names a wiring absent from the release"
            );
            return Err(DeliveryError::SourceNotFound);
        };
        let (Ok(effective_release_id), Ok(wiring_version)) = (
            i32::try_from(manifest.release.effective_release_id.get()),
            i32::try_from(call.wiring_version),
        ) else {
            return Err(DeliveryError::InvalidRequest);
        };
        let admitted = self
            .operations()
            .postgres
            .admit_event_run(
                crate::queue::QUEUE_CLAIM_SCOPE,
                &EventRunAdmission {
                    package_id: call.package_id,
                    effective_release_id,
                    environment: &manifest.release.environment,
                    wiring_id: call.wiring_id,
                    wiring_version,
                    wiring_hash: wiring.graph_hash.as_str(),
                    registration_id: call.registration_id,
                    idempotency_key: call.delivery_id,
                    input: call.payload,
                },
            )
            .await;
        match admitted {
            Ok(EventRunAdmitted::Queued { run_id }) => {
                tracing::info!(
                    registration = call.registration_id,
                    run_id = %run_id,
                    "an event started a queued workflow run"
                );
                bridge.record(call.attributes, DeliveryClass::Delivered);
                Ok(DeliveryOutcome::Discard)
            }
            Ok(EventRunAdmitted::Conflict) => {
                tracing::warn!(
                    registration = call.registration_id,
                    delivery = call.delivery_id,
                    "the delivery id belongs to a different run"
                );
                Err(DeliveryError::InvalidRequest)
            }
            // The event stays unacknowledged, and JetStream redelivers it.
            Err(error) => {
                tracing::warn!(
                    registration = call.registration_id,
                    error = %error,
                    "the event run was not admitted"
                );
                Err(DeliveryError::ExecutionFailed)
            }
        }
    }
}

/// Settle a delivery that the driver refused or failed to execute. A failure
/// after a declared committed result keeps its evidence. Every other refusal
/// settles as a route refusal does.
async fn refuse(
    bridge: &RouterDeliveryBridge,
    source: SourceRef<'_>,
    delivery_id: &str,
    target: &AttachmentTarget,
    attributes: &[KeyValue],
    error: &anyhow::Error,
) -> Result<DeliveryOutcome, DeliveryError> {
    if let Some(interrupted) = error.downcast_ref::<InterruptedResponse>() {
        let failure = interrupted
            .source
            .downcast_ref::<OperationRefusal>()
            .map_or(DeliveryError::ExecutionFailed, lower_operation_refusal);
        bridge.record(attributes, DeliveryClass::ExecutionFailed);
        bridge
            .tap(
                source,
                delivery_id,
                target,
                RouterTapPhase::Settled(EXECUTION_FAILED),
                &serde_json::Value::Null,
            )
            .await;
        tracing::warn!(error = %format_args!("{:#}", interrupted.source), "router delivery failed after a declared committed result");
        return partial_outcome(&interrupted.evidence, FailedOutcome::Error(failure));
    }
    bridge
        .refuse(source, delivery_id, target, attributes, error)
        .await
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
                error_kind = ?error.kind(),
                "derived-event publication did not receive a server ACK"
            );
            DeliveryError::ExecutionFailed
        })
}

/// How one settled delivery reads in the live view: the outcome label, and the
/// result the caller was given.
///
/// This mirrors [`lower_outcome`] arm for arm, INCLUDING its two order-sensitive
/// rulings — a running walk is a failure whatever verdict it carries, and a
/// first verdict stands over a later frontier failure. A live view that
/// disagreed with what the caller actually received would be worse than none,
/// because it would be believed.
///
/// The verdict payloads are BORROWED. The plugin copies only if it will publish,
/// so a host with no data-plane NATS pays nothing for a preview it drops.
pub(crate) fn settled_preview(outcome: &Outcome) -> (&'static str, Cow<'_, serde_json::Value>) {
    if matches!(outcome.status, WalkStatus::Running) {
        return (EXECUTION_FAILED, Cow::Owned(serde_json::Value::Null));
    }
    match outcome.verdict.as_ref() {
        Some(Verdict::Respond { payload, .. }) => ("respond", Cow::Borrowed(payload)),
        Some(Verdict::Emit { event, .. }) => ("emit", Cow::Borrowed(event)),
        Some(Verdict::Discard) => ("discard", Cow::Owned(serde_json::Value::Null)),
        None => match outcome.status {
            WalkStatus::Cancelled => ("cancelled", Cow::Owned(serde_json::Value::Null)),
            WalkStatus::Failed => (
                "failed",
                Cow::Owned(match outcome.failure.as_ref() {
                    // The kind is deliberately absent: it has no wire spelling
                    // of its own, and a `Debug` rendering on a subject a console
                    // parses would drift the moment the enum is edited.
                    Some(failure) => serde_json::json!({
                        "code": failure.detail.code,
                        "message": failure.detail.message,
                    }),
                    None => serde_json::Value::Null,
                }),
            ),
            // A completed walk with no verdict never settled anything, and
            // `lower_outcome` refuses both of these the same way.
            WalkStatus::Completed | WalkStatus::Running => {
                (EXECUTION_FAILED, Cow::Owned(serde_json::Value::Null))
            }
        },
    }
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
                    kind: lower_failure_kind(failure.kind),
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

fn lower_failure_kind(kind: FailureKind) -> WireFailureKind {
    match kind {
        FailureKind::Terminal => WireFailureKind::Terminal,
        FailureKind::RetryExhausted => WireFailureKind::RetryExhausted,
        FailureKind::InvalidInput => WireFailureKind::InvalidInput,
        FailureKind::HopLimit => WireFailureKind::HopLimit,
        FailureKind::UnreleasedCaller => WireFailureKind::UnreleasedCaller,
        FailureKind::MissingDedupId => WireFailureKind::MissingDedupId,
        FailureKind::RespondWithoutCaller => WireFailureKind::RespondWithoutCaller,
        FailureKind::SecondVerdict => WireFailureKind::SecondVerdict,
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
        assert!(matches!(failure.kind, WireFailureKind::InvalidInput));
        assert_eq!(failure.code.as_deref(), Some("bad-order"));
        assert_eq!(failure.message, "order is invalid");
    }

    /// A route answers each node outcome exactly as its one-node respond
    /// wiring does: the caller's outcome and the live view's label and result
    /// both match. A retryable or rate-limited error reads as the wiring's
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
            let (label, result) = settled_preview(&wiring);
            assert_eq!((settled.label, &settled.result), (label, &*result));
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

    /// The live view shows the caller's truth, not a second opinion: for every
    /// outcome shape, the preview's label agrees with what `lower_outcome`
    /// actually returns — including the two arms whose ORDER decides the answer.
    #[test]
    fn a_settled_preview_never_contradicts_what_the_caller_received() {
        let respond = Verdict::Respond {
            payload: serde_json::json!({"accepted": true}),
            node_id: "respond".into(),
        };

        let responded = outcome_of(WalkStatus::Completed, Some(respond.clone()));
        let (label, result) = settled_preview(&responded);
        assert_eq!(label, "respond");
        assert_eq!(*result, serde_json::json!({"accepted": true}));

        // A running walk is refused whatever verdict it carries — `lower_outcome`
        // tests `Running` BEFORE the verdict, so a preview that read the verdict
        // first would promise a caller a response it never got.
        let (label, _) = settled_preview(&outcome_of(WalkStatus::Running, Some(respond.clone())));
        assert_eq!(label, EXECUTION_FAILED);
        assert!(matches!(
            lower_outcome(outcome_of(WalkStatus::Running, Some(respond.clone()))),
            Err(DeliveryError::ExecutionFailed)
        ));

        // A first verdict stands over a later frontier failure, so the preview
        // shows the verdict rather than the failure.
        let mut second_verdict = outcome_of(WalkStatus::Failed, Some(respond));
        second_verdict.failure = Some(Failure {
            node: "later-terminal".into(),
            kind: FailureKind::SecondVerdict,
            detail: ErrorDetail::coded("second-verdict", "later terminal refused"),
        });
        assert_eq!(settled_preview(&second_verdict).0, "respond");

        // Verdictless walks read off the status alone.
        assert_eq!(
            settled_preview(&outcome_of(WalkStatus::Cancelled, None)).0,
            "cancelled"
        );
        assert_eq!(
            settled_preview(&outcome_of(WalkStatus::Completed, None)).0,
            EXECUTION_FAILED
        );
        assert_eq!(
            settled_preview(&outcome_of(WalkStatus::Completed, Some(Verdict::Discard))).0,
            "discard"
        );

        // A failure's preview carries the caller's own code and message and
        // nothing the caller did not get.
        let mut failed = outcome_of(WalkStatus::Failed, None);
        failed.failure = Some(Failure {
            node: "retired-coordinate".into(),
            kind: FailureKind::InvalidInput,
            detail: ErrorDetail::coded("bad-order", "order is invalid"),
        });
        let (label, result) = settled_preview(&failed);
        assert_eq!(label, "failed");
        assert_eq!(
            *result,
            serde_json::json!({"code": "bad-order", "message": "order is invalid"})
        );
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
        assert!(matches!(failure.kind, WireFailureKind::Terminal));
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

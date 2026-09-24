//! The wiring arm of the delivery bridge. The router driver walks the wiring
//! and the bridge settles the delivery. A route never reaches this module.
//!
//! `wamn-xs9a.4` moves this module with the driver into the workflow layer.

use std::borrow::Cow;

use opentelemetry::KeyValue;
use wamn_catalog::AttachmentTarget;
use wamn_event_wire::Causation;
use wamn_router::{FailureKind, Outcome, Verdict, WalkStatus};
use wamn_runtime::plugins::wamn_jetstream::{DerivedPublishRequest, RouterTapPhase};

use super::bindings::wamn::router_delivery::delivery::{
    DeliveryError, DeliveryFailure, DeliveryOutcome, EffectOutcome as WireEffectOutcome, Emission,
    FailedOutcome, FailureKind as WireFailureKind, PartialCompletion,
};
use super::{
    DeliveryClass, EXECUTION_FAILED, ROUTER_DELIVERY_ID, RouterDeliveryBridge, SourceRef,
    WiringCall, WiringDelivery, WiringPreload, lower_operation_refusal,
};
use crate::operation::OperationRefusal;
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
        let release = &bridge.release.manifest().release;
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
        .jetstream
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
pub(super) fn settled_preview(outcome: &Outcome) -> (&'static str, Cow<'_, serde_json::Value>) {
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

pub(super) fn lower_with_evidence(
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

pub(super) fn lower_outcome(outcome: Outcome) -> Result<DeliveryOutcome, DeliveryError> {
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

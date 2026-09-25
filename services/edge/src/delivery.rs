//! The edge's delivery: a route calls its one operation, and nothing else runs.
//!
//! A route target calls [`invoke_operation`] once and settles its result with
//! the engine's lowering, so the box and the cloud answer a route alike. The
//! call carries the intent context of its route, so a write logs one intent
//! for each item in the SQLite store and runs only new items. A
//! wiring or registration target has no runner on the box and fails as
//! `execution-failed`. The box has no model versions, so a read carries no
//! ETag and ignores `If-None-Match`. The box keeps no tap and no actor labels.

use std::sync::Arc;

use wamn_catalog::AttachmentTarget;
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::operation::{
    IntentContext, OperationCall, OperationClosure, invoke_operation, node_types,
};
use wamn_engine::router_delivery::{
    DeliveryError, DeliveryOutcome, DeliveryReport, DeliveryRequest, OperationRefusal,
    RouteDelivery, Source, SourceRef, bounded_node_deadline_ms, lower_operation_refusal,
    resolve_authorized_target, route_component, settle_route,
};
use wamn_run_state_sqlite::SqliteIntentStore;

use crate::application::EdgeApplication;
use crate::policy::EdgeFacts;
use crate::release::EdgeRelease;

/// The delivery of the edge release over its one application.
pub struct EdgeDelivery {
    release: Arc<EdgeRelease>,
    application: EdgeApplication,
    /// The intent log of every route write.
    intents: SqliteIntentStore,
    /// The tenant of every intent: the organization the box serves.
    tenant: String,
}

impl std::fmt::Debug for EdgeDelivery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EdgeDelivery")
            .field("bundle_digest", &self.release.bundle_digest())
            .finish_non_exhaustive()
    }
}

impl EdgeDelivery {
    /// Deliver routes of `release` to `application`, and log the intents of
    /// `tenant` in `intents`.
    pub fn new(
        release: Arc<EdgeRelease>,
        application: EdgeApplication,
        intents: SqliteIntentStore,
        tenant: String,
    ) -> Self {
        Self {
            release,
            application,
            intents,
            tenant,
        }
    }

    async fn deliver_route(
        &self,
        request: DeliveryRequest,
        caller: Option<AuthenticatedCaller>,
    ) -> Result<DeliveryOutcome, DeliveryError> {
        let DeliveryRequest {
            source,
            delivery_id,
            payload,
            trace,
            parent_causation,
            ..
        } = request;
        if delivery_id.is_empty() || parent_causation.is_some() {
            return Err(DeliveryError::InvalidRequest);
        }
        let payload: serde_json::Value =
            serde_json::from_str(&payload).map_err(|_| DeliveryError::InvalidPayload)?;
        let source = match &source {
            Source::Attachment(id) if !id.is_empty() => SourceRef::Attachment(id),
            Source::Registration(id) if !id.is_empty() => SourceRef::Registration(id),
            Source::Attachment(_) | Source::Registration(_) => {
                return Err(DeliveryError::InvalidRequest);
            }
        };
        let (traceparent, tracestate) = match trace {
            Some(trace) if trace.traceparent.is_empty() => {
                return Err(DeliveryError::InvalidRequest);
            }
            Some(trace) => (Some(trace.traceparent), trace.tracestate),
            None => (None, None),
        };
        let manifest = self.release.release().manifest();
        let target = resolve_authorized_target(manifest, source, caller.as_ref())?;
        let AttachmentTarget::Route {
            component,
            operation,
        } = &target.target
        else {
            tracing::warn!(
                source = source.id(),
                "the edge has no wiring layer; a wiring target fails"
            );
            return Err(DeliveryError::ExecutionFailed);
        };
        let components = self.release.components();
        let release = self.release.release().release().manifest_digest.to_string();
        let intent = manifest
            .route(&target.package_id, component, operation)
            .map(|route| IntentContext {
                store: &self.intents,
                tenant: &self.tenant,
                release: &release,
                package: &target.package_id,
                kind: route.kind,
                key_field: route.idempotency.as_deref(),
            });
        let result = async {
            let fact = route_component(components, &target.package_id, component, operation)?;
            let deadline_ms = bounded_node_deadline_ms(None);
            let context = node_types::NodeContext {
                wiring_id: String::new(),
                wiring_version: 0,
                node_id: String::new(),
                delivery_id: delivery_id.clone(),
                input_port: None,
                occurrence: 0,
                traceparent,
                tracestate,
                deadline_ms: Some(deadline_ms),
                config: "null".to_owned(),
            };
            let outcome = invoke_operation(
                &self.application,
                OperationCall {
                    closure: OperationClosure::Released(components),
                    component: fact,
                    operation,
                    context,
                    input: &payload,
                    deadline_ms,
                    facts: EdgeFacts::entry(caller),
                },
                intent,
            )
            .await?;
            settle_route(outcome)
        }
        .await;
        let error = match result {
            Ok(settled) => return Ok(settled.outcome),
            Err(error) => error,
        };
        if let Some(refusal) = error.downcast_ref::<OperationRefusal>() {
            return Err(lower_operation_refusal(refusal));
        }
        tracing::warn!(
            error = %format_args!("{error:#}"),
            "edge route execution failed"
        );
        Err(DeliveryError::ExecutionFailed)
    }
}

#[async_trait::async_trait]
impl RouteDelivery for EdgeDelivery {
    async fn deliver(
        &self,
        request: DeliveryRequest,
        caller: Option<AuthenticatedCaller>,
    ) -> DeliveryReport {
        DeliveryReport {
            outcome: self.deliver_route(request, caller).await,
            actor_labels: Vec::new(),
            etag: None,
            deadline_adjustments: Vec::new(),
        }
    }
}

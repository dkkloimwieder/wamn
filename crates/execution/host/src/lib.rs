//! The route path: the operation host, the delivery bridge, and release
//! readiness.
//!
//! A route calls its one operation through `wamn_engine::invoke_operation`.
//! A wiring target goes to the wiring layer through [`WiringDelivery`]. That
//! layer is `wamn-workflow`, which sits above this crate. This crate links
//! neither `wamn-workflow` nor `wamn-router`.

mod operation;
mod read_cache;
mod readiness;
mod route;
mod router_delivery;

pub use operation::{
    InvocationSite, NativeFacts, NativePolicy, NodeAcquisition, OperationHost, OperationRefusal,
    OperationRefusalKind, OperationScope, authorize_registered_operation, bounded_node_deadline_ms,
    component_invocation, invocation_span, node_trace_context, remote_trace_context,
};
pub use readiness::{
    RELEASE_READINESS_CHECK_FAILED, RELEASE_READINESS_INVALIDATED, RouterReadinessProbe,
    RouterReadinessSnapshot, RouterReadinessStatus, synchronous_request_kind,
    synchronous_route_count,
};
pub use router_delivery::{
    DeadlineAdjustment, DeliveryClass, DeliveryError, DeliveryFailure, DeliveryOutcome,
    EXECUTION_FAILED, EffectOutcome, Emission, FailedOutcome, FailureKind, PartialCompletion,
    ROUTER_DELIVERY_ID, RouterDeliveryBridge, SourceRef, WiringCall, WiringDelivery, WiringPreload,
    lower_operation_refusal,
};

/// Exercise the production attachment resolver and registered-operation guard
/// from an integration test.
#[cfg(feature = "test-util")]
pub fn authorize_attachment_for_test(
    release: &wamn_engine::release_manifest::LoadedRelease,
    attachment_id: &str,
    caller: Option<&wamn_runtime::plugins::flow_http_routing::AuthenticatedCaller>,
) -> Result<(), Box<str>> {
    router_delivery::authorize_attachment_for_test(release, attachment_id, caller)
}

/// Settle one route outcome, so the wiring layer can test that a route and
/// its one-node wiring answer alike. Returns the outcome, the live view
/// label, and the live view result.
#[cfg(feature = "test-util")]
pub fn settle_route_for_test(
    outcome: Result<
        wamn_engine::operation::node_types::Emission,
        wamn_engine::operation::node_types::NodeError,
    >,
) -> anyhow::Result<(DeliveryOutcome, &'static str, serde_json::Value)> {
    router_delivery::settle_route(outcome)
        .map(|settled| (settled.outcome, settled.label, settled.result))
}

//! The route path: the operation host, the delivery bridge, and release
//! readiness.
//!
//! A route calls its one operation through `wamn_engine::invoke_operation`.
//! A wiring target goes to the wiring layer through [`WiringDelivery`]. That
//! layer is `wamn-workflow`, which sits above this crate. This crate links
//! neither `wamn-workflow` nor `wamn-router`.

mod operation;
mod query_read;
mod read_cache;
mod readiness;
mod route;
mod router_delivery;

pub use operation::{
    InvocationSite, NativeFacts, NativePolicy, NodeAcquisition, OperationHost, OperationScope,
    component_invocation, invocation_span, node_trace_context, remote_trace_context,
};
pub use readiness::{
    RELEASE_READINESS_CHECK_FAILED, RELEASE_READINESS_INVALIDATED, RouterReadinessProbe,
    RouterReadinessSnapshot, RouterReadinessStatus, synchronous_request_kind,
    synchronous_route_count,
};
pub use router_delivery::{
    DeadlineAdjustment, RouterDeliveryBridge, WiringCall, WiringDelivery, WiringPreload,
};

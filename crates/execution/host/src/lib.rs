//! The route path: the operation host, the delivery bridge, and release
//! readiness.
//!
//! A route calls its one operation through `wamn_engine::invoke_operation`.
//! The workflow queue uses this host to call its nodes. This crate links
//! neither `wamn-workflow` nor `wamn-router`.

mod application_route;
mod control_route;
pub mod environment_status;
pub mod file_route;
mod host_route;
mod operation;
mod read_cache;
mod readiness;
mod route;
mod router_delivery;

pub use host_route::{HostRouteDelivery, HostRouteHandlers};
pub use operation::{
    CallIntents, InvocationSite, NativeFacts, NativePolicy, NodeAcquisition, OperationHost,
    OperationScope, component_invocation, invocation_span, node_trace_context,
    remote_trace_context,
};
pub use readiness::{
    RELEASE_READINESS_CHECK_FAILED, RELEASE_READINESS_INVALIDATED, RouterReadinessProbe,
    RouterReadinessSnapshot, RouterReadinessStatus, synchronous_request_kind,
    synchronous_route_count,
};
pub use router_delivery::{DeadlineAdjustment, RouterDeliveryBridge};

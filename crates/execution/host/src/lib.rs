//! The route path: the operation host, the delivery bridge, and release
//! readiness.
//!
//! A route calls its one operation through `wamn_engine::invoke_operation`.
//! The workflow queue uses this host to call its nodes. This crate links
//! neither `wamn-workflow` nor `wamn-router`.

mod application_route;
mod control_route;
pub mod environment_status;
pub mod file_collector;
pub mod file_route;
mod host_route;
pub mod oauth_callback;
mod operation;
pub mod platform_http;
mod read_cache;
mod readiness;
mod route;
mod router_delivery;

pub use host_route::{ControlExecution, HostRouteDelivery, HostRouteHandlers};
pub use operation::{
    CallIntents, InvocationSite, NativeFacts, NativePolicy, NodeAcquisition, OperationHost,
    OperationHostConfig, component_invocation, invocation_span, node_trace_context,
    remote_trace_context,
};
pub use readiness::{
    RELEASE_READINESS_INVALIDATED, RELEASE_READINESS_PROBE_FAILED, RouterReadinessProbe,
    RouterReadinessSnapshot, RouterReadinessStatus, synchronous_request_type,
    synchronous_route_count,
};
pub use router_delivery::{DeadlineAdjustment, RouterDeliveryBridge};

pub mod credential_cleanup;

#[cfg(feature = "test-util")]
pub use control_route::ProjectionPause;

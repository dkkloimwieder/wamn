//! The operation host, native platform endpoints, and release
//! readiness.
//!
//! A route calls its one operation through `wamn_engine::invoke_operation`.
//! The workflow queue uses this host to call its nodes. This crate links
//! no `wamn-workflow`.

mod application_route;
mod control_route;
pub mod environment_status;
pub mod file_collector;
pub mod file_route;
mod host_route;
pub mod oauth_callback;
mod operation;
pub mod platform_http;
mod readiness;

pub use host_route::{ControlExecution, HostRouteDelivery, HostRouteHandlers};
pub use operation::{
    InvocationSite, NativeFacts, NativePolicy, NodeAcquisition, OperationHost, OperationHostConfig,
    component_invocation, invocation_span, node_trace_context, remote_trace_context,
};
pub use readiness::{
    RELEASE_READINESS_INVALIDATED, RELEASE_READINESS_PROBE_FAILED, RouterReadinessProbe,
    RouterReadinessSnapshot, RouterReadinessStatus, synchronous_request_type,
    synchronous_route_count,
};
pub use wamn_engine::native_routes::DeadlineAdjustment;

pub mod credential_cleanup;

#[cfg(feature = "test-util")]
pub use control_route::ProjectionPause;

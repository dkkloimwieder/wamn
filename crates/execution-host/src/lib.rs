//! The operation host, native platform endpoints, and release
//! readiness.
//!
//! A route calls its one operation through `wamn_engine::invoke_operation`.
//! The workflow queue uses this host to call its nodes. This crate links
//! no `wamn-workflow`.

mod application_route;
mod control_route;
pub mod environment_status;
pub mod file_collection;
pub mod file_collector;
pub mod file_route;
mod host_route;
mod number_counter;
pub mod oauth_callback;
mod operation;
pub mod platform_http;

pub use host_route::{ControlExecution, HostRouteDelivery, HostRouteHandlers};
pub use operation::{OperationHost, OperationHostConfig};

pub use wamn_engine::native_routes::DeadlineAdjustment;

pub mod credential_cleanup;

pub mod connection_health;

//! Host routes and ingress route data.

use wamn_registry::AppName;

pub mod exposure;
mod host_route;
pub mod host_wire;
pub mod http_route_data;
pub mod read_query;
mod route_identity;

pub use host_route::{
    HostAttachment, HostHandler, HostRoute, HostRouteAuthority, HostRouteSet, host_route_package,
    host_route_path_prefix, host_route_version, is_granted_host_route_operation,
    is_host_route_operation,
};
pub use route_identity::{route_attachment_id, route_path};

/// The route configuration key read by the HTTP component.
pub const HTTP_ROUTES_CONFIG: &str = "wamn.http.routes";

/// The native ingress path reserved for an application's Request workflows.
pub fn workflow_http_prefix(app: &AppName) -> String {
    format!("/api/{app}/flow")
}

/// The grant reference that persists across versions and Request paths.
pub fn workflow_permission(app: &AppName, id: &str) -> String {
    format!("wamn-workflow:{app}/{id}")
}

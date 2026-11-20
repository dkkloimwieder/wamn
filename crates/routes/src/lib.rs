//! Host routes, ingress route data, and workflow graphs.

use std::fmt;

pub mod exposure;
mod host_route;
pub mod http_route_data;
pub mod read_query;
mod route_identity;
mod wiring;

pub use host_route::{
    HostAttachment, HostHandler, HostRoute, HostRouteAuthority, HostRouteContractError,
    HostRouteSet, host_route_package, host_route_path_prefix, host_route_version,
    is_granted_host_route_operation, is_host_route_operation,
};
pub use route_identity::{route_attachment_id, route_path};
pub use wiring::{
    WIRING_DOCUMENT_FORMAT_VERSION, WiringDocument, WiringEdge, WiringEntry, WiringNode,
    workflow_http_prefix, workflow_permission,
};

/// The route configuration key read by the HTTP component.
pub const HTTP_ROUTES_CONFIG: &str = "wamn.http.routes";

/// Default node output port.
pub const MAIN_PORT: &str = "main";

/// Node output port for failures.
pub const ERROR_PORT: &str = "error";

/// A refused workflow definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WiringError {
    ZeroVersion {
        field: &'static str,
    },
    InvalidDefinition {
        message: String,
    },
    UnresolvedWiringNode {
        node_id: wamn_package::canonical_name::CanonicalName,
    },
}

impl fmt::Display for WiringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroVersion { field } => write!(formatter, "{field} must be greater than zero"),
            Self::InvalidDefinition { message } => write!(formatter, "{message}"),
            Self::UnresolvedWiringNode { node_id } => {
                write!(
                    formatter,
                    "wiring edge names node {node_id:?}, which the document does not declare"
                )
            }
        }
    }
}

impl std::error::Error for WiringError {}

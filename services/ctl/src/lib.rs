//! Local builds and clients of authenticated control routes.

pub mod application_verbs;
pub mod artifact_verbs;
pub mod build_verbs;
pub mod component_verbs;
pub mod dev;
pub mod identity_verbs;
pub mod integration_verbs;
mod operation_verbs;
pub mod package_verbs;
pub mod platform_verbs;
pub mod provisioning_verbs;
pub mod role_verbs;
mod route_request;
pub mod schema_verbs;
#[cfg(target_os = "linux")]
pub mod web;
pub mod workflow_verbs;

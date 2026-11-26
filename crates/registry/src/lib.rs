//! Registry coordinates, resource names, and domain values.

pub mod configuration;
pub mod connections;
mod env_app;
pub mod resource_names;

pub use env_app::{AppDefinition, AppName, EnvApps, ParseAppNameError, app_for_package};
pub mod deployment;
pub mod identifiers;
pub mod instance_suffix;
pub mod integration_code;
pub mod name;
pub mod platform_domain;
mod types;

pub use name::{credential_secret_name, project_env_key};
pub use types::{Env, EnvRow, EnvStatus, Triple};
mod org_id;
mod project_id;
pub use org_id::{OrgId, ParseOrgIdError};
pub use project_id::{DEFAULT_PROJECT, ParseProjectIdError, ProjectId};

mod env_name;
mod project_env_key;
pub use env_name::{EnvName, ParseEnvNameError};
pub use project_env_key::{ParseProjectEnvKeyError, ProjectEnvKey};

mod project_identity;
pub use project_identity::{app_identity_hash, apply_identity_hash, control_identity_hash};

/// Return the package and component artifact bases for an org origin, in that order.
pub fn org_artifact_bases(origin: &str) -> (String, String) {
    (
        format!("{origin}/wamn/packages"),
        format!("{origin}/wamn/components"),
    )
}

/// Whether native tools can use HTTP for this registry endpoint.
pub fn registry_is_loopback(registry: &str) -> bool {
    matches!(
        registry.split([':', '/']).next(),
        Some("localhost" | "127.0.0.1")
    ) || registry.starts_with("[::1]")
}

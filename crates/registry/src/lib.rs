//! Registry coordinates, resource names, and system database statements.

pub mod connections;
pub mod deployment;
pub mod identifiers;
pub mod instance_suffix;
pub mod integration_code;
pub mod migrations;
pub mod name;
pub mod platform_domain;
pub mod sql;
mod types;

pub use name::{control_nats_user, credential_secret_name, environment_nats_user, project_env_key};
pub use types::{Environment, OrgId, ProjectId, Triple};

#[cfg(any(test, feature = "test-util"))]
pub mod test_util;

mod env_name;
mod project_env_key;
pub use env_name::{EnvName, ParseEnvNameError};
pub use project_env_key::{ParseProjectEnvKeyError, ProjectEnvKey};

mod project_identity;
pub use project_identity::app_identity_hash;

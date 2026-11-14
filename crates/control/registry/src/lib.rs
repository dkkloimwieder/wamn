//! Registry coordinates, resource names, and system database statements.

pub mod configuration;
pub mod connections;
pub mod deployment;
pub mod identifiers;
pub mod migrations;
pub mod name;
pub mod sql;
mod types;

pub use name::{control_nats_user, credential_secret_name, environment_nats_user, project_env_key};
pub use types::{Env, Environment, OrgId, ProjectId, Triple};

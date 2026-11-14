//! Environment lifecycle and authorized control execution.

pub mod api_service;
pub mod apply_package;
pub mod capture_gap;
#[cfg(feature = "ops")]
pub mod create_user;
pub mod enable_cdc_project_env;
pub mod env_policies;
pub mod environment;
pub mod ident;
pub mod identity_issuer;
pub mod invite;
pub mod migrations;
pub mod org_connection;
pub use wamn_platform_install::platform;
pub mod platform_migrations;
pub use wamn_platform_install::provision_org;
pub mod provision_project_env;
pub use wamn_platform_install::provision_system;
#[cfg(feature = "ops")]
pub mod prune_record_history;
#[cfg(feature = "ops")]
pub mod prune_run_history;
pub mod reconcile_replica_identity;
pub mod replica_identity;

#[cfg(test)]
mod publish_release_tests;

#[cfg(test)]
mod web_artifact_tests;

mod kcl;

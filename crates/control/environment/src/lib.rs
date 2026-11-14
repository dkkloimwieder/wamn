//! Environment lifecycle and authorized control execution.

pub mod api_service;
pub mod apply_package;
#[cfg(feature = "ops")]
pub mod create_user;
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
mod prune_dev_tags;
pub use wamn_platform_install::provision_system;
#[cfg(feature = "ops")]
pub mod prune_record_history;
#[cfg(feature = "ops")]
pub mod prune_run_history;
pub use wamn_events::reconcile_replica_identity;
pub use wamn_events::replica_identity;

#[cfg(test)]
mod publish_release_tests;

#[cfg(test)]
mod web_artifact_tests;

mod kcl;

//! Environment provisioning, reconciliation, and platform operations for the CLI and test support.

pub mod author_workflow;
pub mod capture_gap;
#[cfg(feature = "ops")]
pub mod create_user;
pub mod enable_cdc_project_env;
pub mod env_policies;
pub mod environment;
pub mod ident;
pub mod identity_issuer;
pub mod invite;
pub mod org_connection;
pub mod platform;
pub mod platform_build;
pub mod platform_migrations;
pub mod provision_org;
pub mod provision_project_env;
pub mod provision_system;
#[cfg(feature = "ops")]
pub mod prune_record_history;
#[cfg(feature = "ops")]
pub mod prune_run_history;
pub mod reconcile_package_data_access;
pub mod reconcile_replica_identity;
pub mod reconcile_run_plane;
pub mod verification_policy;
pub mod workflow_lifecycle;

#[cfg(test)]
mod publish_release_tests;

#[cfg(test)]
mod web_artifact_tests;

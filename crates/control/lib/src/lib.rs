//! The control library that `services/ctl`, the dev loop, and test support call.
//!
//! It holds the admission, release, provisioning, reconcile, package, and
//! delivery operations and the development engine. It does database, filesystem, and network work, and it
//! runs the processes that work needs, including `git`, `docker`, `kubectl`,
//! and `cargo`. The `ops` feature adds the environment lifecycle and reporting
//! operations. It does no CLI presentation and does not own the CLI lifecycle:
//! exit codes, signals, and stdout belong to `services/ctl`.

pub mod apply_package;
pub mod author_workflow;
pub mod bind_connection;
pub mod capture_gap;
pub mod component_admission;
pub mod component_declaration;
#[cfg(feature = "ops")]
pub mod create_user;
pub mod delivery;
pub mod dev;
pub mod enable_cdc_project_env;
pub mod env_policies;
pub mod environment;
#[cfg(feature = "ops")]
pub mod event_advisories;
pub mod event_streams;
pub mod git_source;
pub mod ident;
pub mod identity_issuer;
pub mod invite;
pub mod kubectl;
pub mod org_connection;
pub mod owned_command;
pub mod package_artifact;
mod package_upgrade;
pub mod platform;
pub mod platform_build;
pub use wamn_identity_client as pat_client;
pub mod project_env_membership;
pub mod provision_org;
pub mod provision_project_env;
pub mod provision_system;
#[cfg(feature = "ops")]
pub mod prune_record_history;
#[cfg(feature = "ops")]
pub mod prune_run_history;
pub mod publish_release;
pub mod qualify_upgrade;
pub mod reconcile_package_data_access;
pub mod reconcile_replica_identity;
pub mod reconcile_run_plane;
pub mod release_carrier;
pub mod release_chart;
pub mod release_composition;
pub mod role_permission;
pub mod sql_params;
pub mod terminalize_effect_uncertain;
pub mod upgrade_environment;
pub mod upgrade_schema;
pub mod user_role;
pub mod verification_policy;
pub mod web_scope;
pub mod web_upload;
pub mod workflow_lifecycle;

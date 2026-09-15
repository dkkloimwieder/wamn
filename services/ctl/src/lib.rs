//! One-shot control-plane verbs.
//!
//! MVP outcome: provisioning · publish · additive schema · tenant isolation (T1 minting).
//!
//! Provisioning (`provision-org`, `provision-project-env`,
//! `enable-cdc-project-env`), exact package application, and reconciliation
//! ship in `wamn-ctl`. Environment
//! lifecycle and reporting verbs require the `ops` feature and ship in the
//! separate `wamn-ctl-ops` binary.

pub use wamn_control::author_wiring;
pub mod bind_connection;
pub mod component_verbs;
#[cfg(feature = "ops")]
pub mod copy_project_env;
pub mod delivery;
pub mod dev;
pub mod dev_gate;
#[cfg(feature = "ops")]
pub mod dump_project_env;
#[cfg(feature = "ops")]
pub mod event_advisories;
pub mod identity_issuer;
#[cfg(feature = "ops")]
mod ops_schema;
mod owned_command;
pub mod package_verbs;
pub mod print_release_env;
pub mod project_env_membership;
pub use wamn_control::promote;
pub mod provision_org;
pub mod provisioning_verbs;
#[cfg(feature = "ops")]
pub mod prune_record_history;
#[cfg(feature = "ops")]
pub mod prune_run_history;
pub use wamn_control::publish_release;
pub mod push_release_manifest;
pub mod reconcile_replica_identity;
pub use wamn_control::reconcile_run_plane;
#[cfg(feature = "ops")]
pub mod restore_project_env;
pub mod terminalize_effect_uncertain;
#[cfg(target_os = "linux")]
pub mod ui;
pub use wamn_control::verification_policy;

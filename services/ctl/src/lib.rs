//! One-shot control-plane verbs.
//!
//! MVP outcome: provisioning · publish · additive schema · tenant isolation (T1 minting).
//!
//! Provisioning (`provision-org`, `provision-project-env`,
//! `enable-cdc-project-env`), exact package application, and reconciliation
//! ship in `wamn-ctl`. Environment
//! lifecycle and reporting verbs require the `ops` feature and ship in the
//! separate `wamn-ctl-ops` binary.

pub mod bind_connection;
pub mod component_verbs;
#[cfg(feature = "ops")]
pub mod copy_project_env;
pub mod delivery;
pub mod dev;
#[cfg(feature = "ops")]
pub mod dump_project_env;
#[cfg(feature = "ops")]
pub use wamn_control::event_advisories;
pub mod identity_verbs;
#[cfg(feature = "ops")]
mod ops_schema;
#[cfg(feature = "ops")]
pub mod ops_verbs;
mod owned_command;
pub mod package_verbs;
pub mod print_release_env;
pub mod provision_org;
pub mod provisioning_verbs;
pub mod push_release_manifest;
pub mod release_verbs;
#[cfg(feature = "ops")]
pub mod restore_project_env;
#[cfg(target_os = "linux")]
pub mod ui;

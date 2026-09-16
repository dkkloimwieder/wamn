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
pub mod delivery;
pub mod dev;
pub mod identity_verbs;
#[cfg(feature = "ops")]
pub mod ops_verbs;
mod owned_command;
pub mod package_verbs;
pub use wamn_control::print_release_env;
pub mod provision_org;
pub mod provisioning_verbs;
pub use wamn_control::push_release_manifest;
pub mod release_verbs;
#[cfg(target_os = "linux")]
pub mod ui;

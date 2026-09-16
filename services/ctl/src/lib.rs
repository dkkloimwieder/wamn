//! One-shot control-plane verbs.
//!
//! MVP outcome: provisioning · publish · additive schema · tenant isolation (T1 minting).
//!
//! Provisioning (`provision-org`, `provision-project-env`,
//! `enable-cdc-project-env`), exact package application, and reconciliation
//! ship in `wamn-ctl`. Environment
//! lifecycle and reporting verbs require the `ops` feature and ship in the
//! separate `wamn-ctl-ops` binary.

// `dev/coordinator.rs` still reaches the delivery SQLx helpers and the release
// carrier through `crate::`. Session wamn-52 holds that file, so this private
// alias stands in until it can name `wamn_control` itself.
#[cfg(target_os = "linux")]
use wamn_control::{delivery, print_release_env};

pub mod bind_connection;
pub mod component_verbs;
pub mod delivery_verbs;
pub mod dev;
pub mod identity_verbs;
#[cfg(feature = "ops")]
pub mod ops_verbs;
pub mod package_verbs;
pub mod provision_org;
pub mod provisioning_verbs;
pub mod release_verbs;
#[cfg(target_os = "linux")]
pub mod ui;

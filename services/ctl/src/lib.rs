//! The command line for the one-shot control-plane verbs.
//!
//! MVP outcome: provisioning · publish · additive schema · tenant isolation (T1 minting).
//!
//! `wamn-control` does the work. This crate holds the clap argument
//! definitions, the printed output, the exit codes, and the interrupt,
//! termination, and hangup arms of the deploy verb. Each verb parses its
//! arguments, makes one library call, and prints the result. Environment
//! lifecycle and reporting verbs require the `ops` feature and ship in the
//! separate `wamn-ctl-ops` binary. The development loop in `dev/` is the one
//! part of this crate that is not a verb surface.

// `dev/coordinator.rs` still reaches the delivery SQLx helpers and the release
// carrier through `crate::`. Session wamn-52 holds that file, so this private
// alias stands in until it can name `wamn_control` itself.
#[cfg(target_os = "linux")]
use wamn_control::{delivery, print_release_env};

pub use wamn_control::bind_connection;
pub mod component_verbs;
pub mod delivery_verbs;
pub mod dev;
pub mod identity_verbs;
#[cfg(feature = "ops")]
pub mod ops_verbs;
pub mod package_verbs;
pub mod provisioning_verbs;
pub mod release_verbs;
#[cfg(target_os = "linux")]
pub mod ui;

//! One-shot control-plane verbs.
//!
//! MVP outcome: provisioning · publish · additive schema · tenant isolation (T1 minting).
//!
//! Provisioning (`provision-project`, `provision-org`, `provision-project-env`,
//! `enable-cdc-project-env`), exact package application, and reconciliation
//! ship in `wamn-ctl`. Environment
//! lifecycle and reporting verbs require the `ops` feature and ship in the
//! separate `wamn-ctl-ops` binary.

pub mod apply_package;
pub mod author_wiring;
#[cfg(feature = "ops")]
pub mod copy_project_env;
#[cfg(feature = "ops")]
pub mod dead_letters;
pub mod dev;
pub mod dev_gate;
#[cfg(feature = "ops")]
pub mod dump_project_env;
pub mod enable_cdc_project_env;
mod env_policies;
mod ident;
#[cfg(feature = "ops")]
mod ops_schema;
pub mod print_release_env;
pub mod promote;
pub mod provision;
pub mod provision_org;
pub mod provision_project_env;
#[cfg(feature = "ops")]
pub mod prune_run_history;
pub mod publish_release;
pub mod push_component;
pub mod push_release_manifest;
pub mod reconcile_package_data_access;
pub mod reconcile_replica_identity;
pub mod reconcile_run_plane;
#[cfg(feature = "ops")]
pub mod restore_project_env;
mod sql_params;
pub mod terminalize_effect_uncertain;

//! One-shot control-plane verbs.
//!
//! Provisioning (`provision-project`, `provision-org`, `provision-project-env`,
//! `enable-cdc-project-env`), catalog application (`publish-catalog`,
//! `migrate-catalog`), and reconciliation ship in `wamn-ctl`. Environment
//! lifecycle and reporting verbs require the `ops` feature and ship in the
//! separate `wamn-ctl-ops` binary.

#[cfg(feature = "ops")]
pub mod copy_project_env;
#[cfg(feature = "ops")]
pub mod dump_project_env;
pub mod enable_cdc_project_env;
mod env_policies;
#[cfg(feature = "ops")]
pub mod impact_report;
pub mod migrate_catalog;
#[cfg(feature = "ops")]
mod ops_schema;
pub mod provision;
pub mod provision_org;
pub mod provision_project_env;
#[cfg(feature = "ops")]
pub mod prune_run_history;
pub mod publish_catalog;
pub mod reconcile_replica_identity;
pub mod reconcile_run_plane;
#[cfg(feature = "ops")]
pub mod restore_project_env;

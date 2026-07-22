//! wamn-ctl: the one-shot control-plane verbs (SR9, split out of wamn-host).
//!
//! Provisioning (`provision-project`, `provision-org`, `provision-project-env`,
//! `enable-cdc-project-env`), catalog application (`publish-catalog`,
//! `migrate-catalog`), and env lifecycle (`dump/restore/copy-project-env`).
//! The subcommand surface is UNCHANGED from the pre-split `wamn-host` binary,
//! so Job manifests swap only the image/binary. This crate links no runtime:
//! the washlet artifact (`wamn-host`) no longer carries provisioning or
//! replication-credential code, and this artifact carries no engine.
//! Gates drive these verbs through this library (`wamn-gates`).

pub mod copy_project_env;
pub mod dump_project_env;
pub mod enable_cdc_project_env;
mod env_policies;
pub mod migrate_catalog;
pub mod provision;
pub mod provision_org;
pub mod provision_project_env;
pub mod prune_run_history;
pub mod publish_catalog;
pub mod reconcile_replica_identity;
pub mod reconcile_run_plane;
pub mod restore_project_env;

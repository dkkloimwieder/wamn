//! The control library that `services/ctl`, the dev loop, and test support call.
//!
//! It holds the admission, release, provisioning, reconcile, and package
//! operations. It does database, filesystem, and network work. It does no CLI
//! presentation and owns no process.

pub mod apply_package;
pub mod enable_cdc_project_env;
pub mod env_policies;
pub mod event_streams;
pub mod ident;
pub mod pat_client;
pub mod provision_project_env;
pub mod reconcile_package_data_access;
pub mod sql_params;

//! Environment lifecycle and authorized control execution.

pub mod api_service;
pub mod apply_package;
pub mod configuration;
pub mod environment;
pub mod ident;
pub mod migrations;
pub mod org;
pub mod platform_migrations;
pub mod provision_project_env;
pub mod status;
pub use wamn_platform_install::provision_system;
#[cfg(feature = "ops")]
pub mod prune_record_history;
#[cfg(feature = "ops")]
pub mod prune_run_history;

#[cfg(test)]
mod web_artifact_tests;

pub mod release_chart;

pub mod wasm_authority;

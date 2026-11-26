//! Environment lifecycle and authorized control execution.

pub mod app_chart;
pub mod apply_package;
pub mod audit_retention;
pub mod capture_sql;
pub mod configuration;
pub mod env;
pub mod env_fill;
pub mod env_names;
pub mod env_org;
pub mod env_registry;
pub mod env_schema;
pub mod env_sql;
mod env_sql_effects;
pub mod error;
pub mod migrations;
pub mod name;
pub mod operation_grants;
mod package_catalog;
mod platform_database;
pub mod platform_env;
pub mod platform_migrations;
pub mod project_migration;
pub mod project_setup;
pub mod provision_project_env;
pub mod prune_record_history;
pub mod registry;
pub mod release_requirements;
mod roles;
pub mod schema;
pub mod sql;
pub mod status;

#[cfg(test)]
mod web_artifact_tests;

pub mod release_chart;

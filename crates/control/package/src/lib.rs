//! Package admission, artifacts, and application operations for the CLI and test support.

pub mod apply_package;
pub mod component_admission;
pub mod component_declaration;
pub mod connection_requirement;
pub mod database;
pub mod integration;
pub mod migrations;
pub mod package_artifact;
pub mod schema_baseline;
pub mod sql_params;
#[cfg(any(test, feature = "test-util"))]
pub mod test_support;
pub mod web_artifact;
pub mod web_scope;

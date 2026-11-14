//! Package admission, artifacts, and application operations for the CLI and test support.

pub mod component_admission;
pub mod component_declaration;
pub mod connection_requirement;
pub mod integration;
pub mod package_artifact;
pub mod package_source;
#[cfg(any(test, feature = "test-util"))]
pub mod test_support;
pub mod web_artifact;

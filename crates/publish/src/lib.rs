//! Build, admit, and publish immutable package and release artifacts.

pub mod component_admission;
pub mod component_declaration;
pub mod connection_requirement;
pub mod development;
pub mod integration;
pub mod package_artifact;
pub mod package_source;
#[cfg(any(test, feature = "test-util"))]
pub mod test_support;
pub mod web_artifact;

pub mod palette;
pub mod publish_release;
pub mod release_composition;

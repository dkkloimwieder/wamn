//! Build, admit, and publish immutable package and release artifacts.

pub mod component_admission;
pub mod component_declaration;
pub mod connection_requirement;
#[cfg(test)]
mod development;
pub mod integration;
pub mod package_artifact;
pub mod package_source;
#[cfg(test)]
mod test_support;
/// The old path of the shared source manifest, for crates that still enable
/// `test-util`.
#[cfg(all(feature = "test-util", not(test)))]
pub mod test_support {
    pub use wamn_test_infrastructure::publish::source_manifest;
}
pub mod web_artifact;

pub mod palette;
pub mod publish_release;

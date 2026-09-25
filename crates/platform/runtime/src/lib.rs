//! Shared native runtime configuration and wasmCloud host capability adapters.
//!
//! MVP outcome: crash floor · M0 execution · flow composition.

pub mod component_artifact_source;
pub mod connection_authority;
pub mod connection_generation;
pub mod local_application;
pub mod plugins;
pub mod registry_credentials;
mod registry_transport;
pub mod release_manifest_artifact;
pub mod release_manifest_source;
pub mod session_keys;

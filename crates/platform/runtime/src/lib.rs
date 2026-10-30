//! Shared native runtime configuration and wasmCloud host capability adapters.
//!
//! MVP outcome: crash floor · M0 execution · flow composition.

pub mod auth_cache;
pub mod component_artifact_source;
pub mod connection_authority;
pub mod connection_facts;
pub mod connection_generation;
pub mod connection_memory;
pub mod connectors;
mod kv_memory;
pub mod local_application;
pub mod plugins;
mod registry_transport;
pub mod release_manifest_artifact;
pub mod release_manifest_source;
pub mod session_keys;

pub mod connection_credentials;

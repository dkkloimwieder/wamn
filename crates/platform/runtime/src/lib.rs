//! Shared native runtime configuration and wasmCloud host capability adapters.
//!
//! MVP outcome: crash minimum · M0 execution · flow composition.

pub mod auth_cache;
pub mod component_artifact_source;
pub mod connection_authority;
pub mod connection_facts;
pub mod connection_generation;
pub mod connection_health;
pub mod connection_memory;
pub mod integrations;
mod kv_memory;
pub mod local_application;
pub mod plugins;
mod registry_transport;
pub mod release_manifest_artifact;
pub mod release_manifest_source;
pub mod resource_lease;
pub mod session_keys;

pub mod connection_credentials;

pub mod oauth_authorization;
pub mod oauth_encryption;
pub mod oauth_token;
mod pod_identity;

mod effect_outcome;
mod portable_http_target;

pub use effect_outcome::EffectOutcome;
pub use portable_http_target::{
    CanonicalHttpTarget, PortableHttpTargetError, normalize_portable_http_target,
};

pub mod invocation;

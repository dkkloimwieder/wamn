//! Shared host authority, connection policy and effect evidence.

pub mod auth_cache;
pub mod connection;
pub mod connection_authority;
mod effect_outcome;
pub mod effect_span;
pub mod identity;
pub mod kv_memory;
mod portable_http_target;

pub use effect_outcome::EffectOutcome;
pub use portable_http_target::{
    CanonicalHttpTarget, PortableHttpTargetError, normalize_portable_http_target,
};

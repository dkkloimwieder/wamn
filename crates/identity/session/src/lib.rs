//! Session-token verification over a key source, with no database and no HTTPS
//! client (docs/plan/edge.md section 4.4).
//!
//! This crate exports:
//!
//! - [`token`]: the fixed Ed25519 token profile, its claims, and
//!   [`verify_session_token`](token::verify_session_token).
//! - [`keys`]: the public JWK profile and its decoder.
//! - [`verifier`]: the [`KeySource`](verifier::KeySource) trait and the
//!   [`SessionVerifier`](verifier::SessionVerifier) over it.
//! - [`file_keys`]: [`FileKeys`](file_keys::FileKeys), the key source that
//!   reads a key set from a local file.
//! - [`SessionError`]: the error of this crate.
//! - [`PAT_TOKEN_PREFIX`]: the marker of a personal access token, which
//!   tells a PAT from a session token on the same carrier.
//!
//! The cloud key source, `IssuerKeys`, fetches the issuer's keys over HTTPS and
//! lives in `wamn-runtime`. Signing and every database read stay in
//! `wamn-platform-identity`.
//!
//! This crate refuses to depend on:
//!
//! - Postgres: `tokio-postgres`, and `wamn-platform-identity`, which links it.
//! - HTTP clients: `reqwest`, `hyper`, `hyper-util`, `hyper-rustls`.
//!
//! The edge links this crate, and the edge dependency test refuses Postgres.

use std::fmt;

/// Marker every first-party personal access token starts with. A token reads
/// `wamn_pat_<16 hex lookup digits>_<64 hex secret digits>`; the lookup half is
/// stored in the clear as the index key, the whole string only as a digest.
pub const PAT_TOKEN_PREFIX: &str = "wamn_pat_";

pub mod file_keys;
pub mod keys;
pub mod token;
pub mod verifier;

/// Stable classes of session failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionErrorKind {
    /// A token failed its profile, signature, scope, or age. Every refusal
    /// has the same public message.
    Refused,
    /// A public key does not match the Ed25519 profile.
    InvalidKey,
    /// A key file could not be read or parsed.
    KeyFile,
}

/// A session failure with a stable kind and a diagnostic message.
#[derive(Debug)]
pub struct SessionError {
    kind: SessionErrorKind,
    message: Box<str>,
}

impl SessionError {
    fn new(kind: SessionErrorKind, message: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Return the stable failure class.
    pub const fn kind(&self) -> SessionErrorKind {
        self.kind
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SessionError {}

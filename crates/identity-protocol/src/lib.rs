//! Identity types, credential formats, wire messages and session verification.

use std::fmt;

/// Marker every first-party personal access token starts with. A token reads
/// `wamn_pat_<16 hex lookup digits>_<64 hex secret digits>`; the lookup half is
/// stored in the clear as the index key, the whole string only as a digest.
pub const PAT_TOKEN_PREFIX: &str = "wamn_pat_";

mod principal;
pub use principal::{ParseIdentityError, PrincipalId, RoleName};
pub mod secret;
#[cfg(feature = "test-util")]
pub mod test_util;
pub mod wire;

mod platform;
pub use platform::{
    ADMIN_ROLE, PLATFORM_PRINCIPAL_PREFIX, PlatformComponent, WAMN_NAMESPACE, WORKFLOW_USER_NAME,
    workflow_user_id,
};

pub mod file_keys;
pub mod keys;
pub mod token;
pub mod verifier;

/// Stable classes of session failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionErrorType {
    /// A token failed its profile, signature, audience, or age. Every refusal
    /// has the same public message.
    Refused,
    /// A public key does not match the Ed25519 profile.
    InvalidKey,
    /// A key file could not be read or parsed.
    KeyFile,
}

/// A session failure with a stable type and a diagnostic message.
#[derive(Debug)]
pub struct SessionError {
    type_: SessionErrorType,
    message: Box<str>,
}

impl SessionError {
    fn new(type_: SessionErrorType, message: impl Into<Box<str>>) -> Self {
        Self {
            type_,
            message: message.into(),
        }
    }

    /// Return the stable failure class.
    pub const fn error_type(&self) -> SessionErrorType {
        self.type_
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SessionError {}

/// Credential authority selected by trusted host code.
pub mod authority_class;
pub use authority_class::AuthorityClass;
/// Framed project identity hashing.
pub mod project_identity;
pub use project_identity::app_identity_hash;

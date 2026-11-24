//! Failures reported by an HTTP connection operation.

use std::fmt;

/// The domain failure behind a connection operation result.
#[derive(Debug, Clone)]
pub enum ConnectionError {
    Unbound,
    Incompatible,
    AuthorityDenied,
    AttestationInvalid,
    CredentialUnavailable,
    Timeout,
    Transport(String),
    Unauthorized,
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ConnectionError::{self:?}")
    }
}

impl std::error::Error for ConnectionError {}

//! The fixed-profile public session key and its decoder.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use crate::{SessionError, SessionErrorKind};

/// The fixed-profile public JWK; private and unknown parameters are refused.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublicSessionKey {
    /// Immutable, issuer-scoped generation identifier.
    pub kid: String,
    /// Must be `OKP`.
    pub kty: String,
    /// Must be `Ed25519`.
    pub crv: String,
    /// Must be the fully specified `Ed25519`, not `EdDSA`.
    pub alg: String,
    /// Must be `sig`.
    pub r#use: String,
    /// Canonical unpadded base64url of the 32-byte public key.
    pub x: String,
}

/// A complete issuer key set; callers replace, rather than merge, cached sets.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionJwks {
    /// Published and not-yet-retired verification keys.
    pub keys: Vec<PublicSessionKey>,
}

/// Validate the exact JWK profile and decode its public verification bytes.
pub fn decode_public_key(key: &PublicSessionKey) -> Result<[u8; 32], SessionError> {
    if key.kid.is_empty()
        || key.kty != "OKP"
        || key.crv != "Ed25519"
        || key.alg != "Ed25519"
        || key.r#use != "sig"
    {
        return Err(invalid(
            "session public key does not match the Ed25519 profile",
        ));
    }
    URL_SAFE_NO_PAD
        .decode(&key.x)
        .map_err(|_| invalid("session public key is not canonical base64url"))?
        .try_into()
        .map_err(|_| invalid("session public key must contain 32 bytes"))
}

fn invalid(message: &'static str) -> SessionError {
    SessionError::new(SessionErrorKind::InvalidKey, message)
}

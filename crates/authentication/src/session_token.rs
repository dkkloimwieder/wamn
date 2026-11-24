//! RFC 9068 access-token claims and signing.

use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use wamn_identity_protocol::records::AccessTokenClaims;

use crate::identity_error::{IdentityError, IdentityErrorType};

/// Access tokens live for at most fifteen minutes.
pub const MAXIMUM_LIFETIME: i64 = 900;

/// A bearer token with redacted diagnostics and no persistence behavior.
pub struct IssuedSessionToken {
    token: String,
    claims: AccessTokenClaims,
}

impl IssuedSessionToken {
    /// Borrow the bearer value for the exchange response, never for logging.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Borrow the JWT claims with the signing-time age bounds applied.
    pub fn claims(&self) -> &AccessTokenClaims {
        &self.claims
    }
}

impl fmt::Debug for IssuedSessionToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedSessionToken")
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Bound signing to the original validation start, never the resume time.
pub fn signing_times(
    validation_started_at: i64,
    signing_at: i64,
) -> Result<(i64, i64), IdentityError> {
    let expires_at = validation_started_at
        .checked_add(MAXIMUM_LIFETIME)
        .ok_or_else(refused)?;
    if validation_started_at < 0 || signing_at < validation_started_at || signing_at >= expires_at {
        return Err(refused());
    }
    Ok((signing_at, expires_at))
}

/// Check claim fields before opening the signing key.
pub fn validate_claims(claims: &AccessTokenClaims) -> Result<(), IdentityError> {
    if claims.iss.trim().is_empty()
        || claims.aud.is_empty()
        || claims.jti.is_empty()
        || claims.client_id != "wamn-web"
    {
        return Err(refused());
    }
    Ok(())
}

/// Sign and apply all deadlines after the caller acquires the active key lock.
pub fn sign_session_token(
    mut claims: AccessTokenClaims,
    validation_started_at: i64,
    absolute_expiry: Option<i64>,
    kid: &str,
    key: &jsonwebtoken::EncodingKey,
) -> Result<IssuedSessionToken, IdentityError> {
    let now = unix_seconds()?;
    (claims.iat, claims.exp) = signing_times(validation_started_at, now)?;
    if let Some(deadline) = absolute_expiry {
        claims.exp = claims.exp.min(deadline);
        if now >= claims.exp {
            return Err(refused());
        }
    }
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA);
    header.typ = Some("at+jwt".into());
    header.kid = Some(kid.into());
    let token = jsonwebtoken::encode(&header, &claims, key).map_err(|_| corrupt_key())?;
    // A delayed commit must not deliver an already-expired exchange result.
    if unix_seconds()? >= claims.exp {
        return Err(refused());
    }
    Ok(IssuedSessionToken { token, claims })
}

fn unix_seconds() -> Result<i64, IdentityError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| refused())?
        .as_secs()
        .try_into()
        .map_err(|_| refused())
}

fn refused() -> IdentityError {
    IdentityError::new(IdentityErrorType::InvalidInput, "session token refused")
}

pub fn corrupt_key() -> IdentityError {
    IdentityError::new(
        IdentityErrorType::CorruptData,
        "stored session signing key is invalid",
    )
}

//! RFC 9068 access-token signing with the issuer's active key in Postgres.
//!
//! Password login records own renewal and revocation. No access token is persisted or logged.

use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use wamn_identity_protocol::PrincipalId;

/// Access tokens live for at most fifteen minutes.
pub const MAXIMUM_LIFETIME: i64 = 900;

/// RFC 9068 claims issued to the web client, without authorization or cookie state.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccessTokenClaims {
    /// Configured identity issuer.
    pub iss: String,
    /// The identity principal UUID.
    pub sub: PrincipalId,
    /// The org audience.
    pub aud: String,
    /// Exclusive expiry in Unix seconds.
    pub exp: i64,
    /// Actual signing time in Unix seconds.
    pub iat: i64,
    /// Unique token identifier.
    pub jti: String,
    /// The client that requested the access token.
    pub client_id: String,
}

use crate::{
    IdentityError, IdentityErrorType,
    session_keys::{corrupt_key, sign_message},
};

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
pub fn minting_times(
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

/// Sign within the caller's login transaction and optional absolute deadline.
///
/// The caller retains the signing lock through commit and must not deliver a
/// token before that commit succeeds. Supplied JWT claim times are always replaced.
pub async fn sign_session_token_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    mut claims: AccessTokenClaims,
    validation_started_at: i64,
    absolute_expiry: Option<i64>,
) -> Result<IssuedSessionToken, IdentityError> {
    if claims.iss.trim().is_empty()
        || claims.aud.is_empty()
        || claims.jti.is_empty()
        || claims.client_id != "wamn-web"
    {
        return Err(refused());
    }
    let issuer = claims.iss.clone();
    let token = sign_message(transaction, &issuer, |kid, key| {
        let now = unix_seconds()?;
        (claims.iat, claims.exp) = minting_times(validation_started_at, now)?;
        if let Some(deadline) = absolute_expiry {
            claims.exp = claims.exp.min(deadline);
            if now >= claims.exp {
                return Err(refused());
            }
        }
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA);
        header.typ = Some("at+jwt".into());
        header.kid = Some(kid.into());
        jsonwebtoken::encode(&header, &claims, key).map_err(|_| corrupt_key())
    })
    .await?;
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

/// Refuse legacy session admission until the host verifier is replaced.
pub async fn session_is_active(
    _client: &mut sqlx::PgConnection,
    _claims: &AccessTokenClaims,
    _project: &wamn_registry::ProjectId,
    _environment: &wamn_registry::EnvName,
) -> Result<bool, IdentityError> {
    Ok(false)
}

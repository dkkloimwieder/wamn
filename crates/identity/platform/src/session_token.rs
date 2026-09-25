//! Session-token signing and the active-session read.
//!
//! The token profile and its verification live in `wamn-session`. This module
//! signs with the issuer's active key in Postgres and reads whether a session's
//! authority is still active. No token is persisted or logged.

use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use tokio_postgres::{Client, GenericClient, Transaction};
use wamn_session::token::{
    MAXIMUM_LIFETIME, SessionAuthority, SessionClaims, SessionHeader, validate_claim_shape,
};

use crate::{
    IdentityError, IdentityErrorKind,
    session_keys::{key_database_error, sign_message},
};

/// A bearer token with redacted diagnostics and no persistence behavior.
pub struct IssuedSessionToken {
    token: String,
    claims: SessionClaims,
}

impl IssuedSessionToken {
    /// Borrow the bearer value for the exchange response, never for logging.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Borrow the claims with the signing-time age bounds applied.
    pub fn claims(&self) -> &SessionClaims {
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

/// Sign claims using the issuer's active generation without recording a token.
///
/// The exchange owner must have validated identity, membership, and these roles
/// fresh at `validation_started_at`. This primitive does not do those reads.
/// Supplied `iat` and `exp` are replaced after acquiring the signing lock:
/// issuance uses the actual clock and expiry stays anchored to validation.
pub async fn sign_session_token(
    client: &mut Client,
    claims: SessionClaims,
    validation_started_at: i64,
) -> Result<IssuedSessionToken, IdentityError> {
    let transaction = client.transaction().await.map_err(key_database_error)?;
    let token =
        sign_session_token_in_transaction(&transaction, claims, validation_started_at, None)
            .await?;
    transaction.commit().await.map_err(key_database_error)?;
    if unix_seconds()? >= token.claims.exp {
        return Err(refused());
    }
    Ok(token)
}

/// Sign within the caller's login transaction and optional absolute deadline.
///
/// The caller retains the signing lock through commit and must not deliver a
/// token before that commit succeeds. Supplied claim times are always replaced.
pub async fn sign_session_token_in_transaction(
    transaction: &Transaction<'_>,
    mut claims: SessionClaims,
    validation_started_at: i64,
    absolute_expiry: Option<i64>,
) -> Result<IssuedSessionToken, IdentityError> {
    validate_claim_shape(&claims)?;
    let issuer = claims.iss.clone();
    let token = sign_message(transaction, &issuer, |kid| {
        let now = unix_seconds()?;
        (claims.iat, claims.exp) = minting_times(validation_started_at, now)?;
        if let Some(deadline) = absolute_expiry {
            claims.exp = claims.exp.min(deadline);
            if now >= claims.exp {
                return Err(refused());
            }
        }
        let header = serde_json::to_vec(&SessionHeader::new(kid)).map_err(|_| refused())?;
        let body = serde_json::to_vec(&claims).map_err(|_| refused())?;
        Ok(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header),
            URL_SAFE_NO_PAD.encode(body)
        ))
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
    IdentityError::new(IdentityErrorKind::InvalidInput, "session token refused")
}

/// Check current session authority and membership without caching an approval.
///
/// Signature and exact audience verification must precede this read. The caller
/// supplies project and environment from the loaded release, never the request.
pub async fn session_is_active(
    client: &(impl GenericClient + Sync),
    claims: &SessionClaims,
    project: &str,
    environment: &str,
) -> Result<bool, IdentityError> {
    let (login, pat) = match &claims.authority {
        SessionAuthority::Login(id) => (Some(id.as_str()), None),
        SessionAuthority::Pat(id) => (None, Some(id.as_str())),
    };
    client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM identity.principals p \
         JOIN identity.project_env_memberships m ON m.principal_id=p.id \
         WHERE p.id=$1::text::uuid AND p.kind='human' AND p.status='active' \
         AND m.org=$2 AND m.project=$3 AND m.env=$4 AND ( \
         EXISTS (SELECT 1 FROM identity.password_logins l WHERE l.id=$5::text::uuid \
         AND l.principal_id=p.id AND l.issuer=$7 AND l.audience=$8 AND l.revoked_at IS NULL \
         AND l.expires_at>clock_timestamp() AND l.renewal_expires_at>clock_timestamp()) OR \
         EXISTS (SELECT 1 FROM identity.pats t WHERE t.id=$6::text::uuid \
         AND t.principal_id=p.id AND t.revoked_at IS NULL AND t.expires_at>clock_timestamp())))",
            &[
                &claims.sub,
                &claims.org,
                &project,
                &environment,
                &login,
                &pat,
                &claims.iss,
                &claims.aud,
            ],
        )
        .await
        .map_err(key_database_error)?
        .try_get(0)
        .map_err(key_database_error)
}

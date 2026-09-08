//! Fixed Ed25519 session-token primitives, without route or exchange admission.
//!
//! The caller owns fresh identity validation and JWKS cache freshness. These
//! functions enforce the signed wire profile, scope, and age; returning claims
//! is not an operation-permission grant. No token is persisted or logged.

use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use tokio_postgres::Client;

use crate::{
    IdentityError, IdentityErrorKind, PrincipalId,
    session_keys::{PublicSessionKey, decode_public_key, sign_message},
};

/// Owner-approved maximum lifetime, in seconds.
pub const MAXIMUM_LIFETIME: i64 = 900;
/// Owner-approved clock tolerance, in seconds; expiry is at-or-after exp + 30.
pub const TOLERANCE: i64 = 30;
/// Exact, fully specified signature algorithm identifier from RFC 9864.
pub const SESSION_ALGORITHM: &str = "Ed25519";
/// Exact media type of a WAMN session token.
pub const SESSION_TYPE: &str = "wamn-session+jwt";

/// Required wire claims; deserialization alone does not authenticate them.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionClaims {
    /// Configured identity issuer.
    pub iss: String,
    /// Canonical org-issued human principal UUID.
    pub sub: String,
    /// Organization identity.
    pub org: String,
    /// Exact project-environment identity, not its display name.
    pub aud: String,
    /// Environment role slugs, with no implicit default.
    pub roles: Vec<String>,
    /// Exclusive expiry before clock tolerance, in Unix seconds.
    pub exp: i64,
    /// Actual signing time, in Unix seconds.
    pub iat: i64,
    /// Token identifier only; never a server-side session record.
    pub jti: String,
}

/// Host-configured scope; no field is inferred from the token or its key ID.
#[derive(Clone, Copy, Debug)]
pub struct SessionScope<'a> {
    /// Configured issuer.
    pub issuer: &'a str,
    /// Configured organization identity.
    pub org: &'a str,
    /// Configured exact project-environment identity.
    pub audience: &'a str,
}

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

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionHeader {
    alg: String,
    typ: String,
    kid: String,
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

/// Check lifetime, future issuance, and expiry as three separate age rules.
pub fn validate_session_age(
    issued_at: i64,
    expires_at: i64,
    now: i64,
) -> Result<(), IdentityError> {
    let lifetime = expires_at.checked_sub(issued_at).ok_or_else(refused)?;
    let future_limit = now.checked_add(TOLERANCE).ok_or_else(refused)?;
    let expiry_limit = expires_at.checked_add(TOLERANCE).ok_or_else(refused)?;
    if issued_at < 0
        || now < 0
        || !(1..=MAXIMUM_LIFETIME).contains(&lifetime)
        || issued_at > future_limit
        || now >= expiry_limit
    {
        return Err(refused());
    }
    Ok(())
}

/// Sign claims using the issuer's active generation without recording a token.
///
/// The exchange owner must have validated identity, membership, and these roles
/// fresh at `validation_started_at`. This primitive does not do those reads.
/// Supplied `iat` and `exp` are replaced after acquiring the signing lock:
/// issuance uses the actual clock and expiry stays anchored to validation.
pub async fn sign_session_token(
    client: &mut Client,
    mut claims: SessionClaims,
    validation_started_at: i64,
) -> Result<IssuedSessionToken, IdentityError> {
    validate_claim_shape(&claims)?;
    let issuer = claims.iss.clone();
    let token = sign_message(client, &issuer, |kid| {
        let now = unix_seconds()?;
        (claims.iat, claims.exp) = minting_times(validation_started_at, now)?;
        let header = SessionHeader {
            alg: SESSION_ALGORITHM.into(),
            typ: SESSION_TYPE.into(),
            kid: kid.into(),
        };
        let header = serde_json::to_vec(&header).map_err(|_| refused())?;
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

/// Read an untrusted key selector; this never establishes issuer trust.
pub fn session_key_id(token: &str) -> Result<String, IdentityError> {
    let (header, _, _) = token_parts(token)?;
    Ok(decode_header(header)?.kid)
}

/// Verify the exact profile, signature, configured scope, and claim age.
///
/// `key` must come from the configured issuer's currently fresh complete JWKS.
/// Cache freshness and operation permissions are deliberately outside this
/// primitive. Every token/profile/scope/age refusal has the same public error.
pub fn verify_session_token(
    token: &str,
    key: &PublicSessionKey,
    scope: SessionScope<'_>,
    now: i64,
) -> Result<SessionClaims, IdentityError> {
    let (header, body, signature) = token_parts(token)?;
    let header = decode_header(header)?;
    if header.kid != key.kid {
        return Err(refused());
    }
    let key_bytes = decode_public_key(key).map_err(|_| refused())?;
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| refused())?;
    let signed = token.rsplit_once('.').ok_or_else(refused)?.0;
    UnparsedPublicKey::new(&ED25519, key_bytes)
        .verify(signed.as_bytes(), &signature)
        .map_err(|_| refused())?;
    let body = URL_SAFE_NO_PAD.decode(body).map_err(|_| refused())?;
    let claims: SessionClaims = serde_json::from_slice(&body).map_err(|_| refused())?;
    validate_claim_shape(&claims)?;
    if claims.iss != scope.issuer || claims.org != scope.org || claims.aud != scope.audience {
        return Err(refused());
    }
    validate_session_age(claims.iat, claims.exp, now)?;
    Ok(claims)
}

fn token_parts(token: &str) -> Result<(&str, &str, &str), IdentityError> {
    let mut parts = token.split('.');
    let header = parts.next().ok_or_else(refused)?;
    let body = parts.next().ok_or_else(refused)?;
    let signature = parts.next().ok_or_else(refused)?;
    if parts.next().is_some() || header.is_empty() || body.is_empty() || signature.is_empty() {
        return Err(refused());
    }
    Ok((header, body, signature))
}

fn decode_header(encoded: &str) -> Result<SessionHeader, IdentityError> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| refused())?;
    let header: SessionHeader = serde_json::from_slice(&bytes).map_err(|_| refused())?;
    if header.alg != SESSION_ALGORITHM || header.typ != SESSION_TYPE || header.kid.is_empty() {
        return Err(refused());
    }
    Ok(header)
}

fn validate_claim_shape(claims: &SessionClaims) -> Result<(), IdentityError> {
    let principal = claims.sub.parse::<PrincipalId>().map_err(|_| refused())?;
    if principal.as_str() != claims.sub
        || claims.iss.trim().is_empty()
        || claims.org.is_empty()
        || claims.aud.is_empty()
        || claims.jti.is_empty()
        || claims
            .roles
            .iter()
            .any(|role| !crate::canonical_role(role).is_ok_and(|canonical| canonical == *role))
    {
        return Err(refused());
    }
    Ok(())
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

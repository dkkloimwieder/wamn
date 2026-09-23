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
use tokio_postgres::{Client, GenericClient, Transaction};

use crate::{
    IdentityError, IdentityErrorKind, PrincipalId,
    session_keys::{PublicSessionKey, decode_public_key, key_database_error, sign_message},
};

/// Owner-approved maximum lifetime, in seconds.
pub const MAXIMUM_LIFETIME: i64 = 900;
/// Owner-approved clock tolerance, in seconds; expiry is at-or-after exp + 30.
pub const TOLERANCE: i64 = 30;
/// Exact, fully specified signature algorithm identifier from RFC 9864.
pub const SESSION_ALGORITHM: &str = "Ed25519";
/// Exact media type of a WAMN session token.
pub const SESSION_TYPE: &str = "wamn-session+jwt";

/// Revocable authority that issued a session token.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionAuthority {
    /// An existing password login and its renewal family.
    Login(String),
    /// The source credential of an optional PAT exchange.
    Pat(String),
}

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
    /// Current server-side authority, checked for every new admission.
    pub authority: SessionAuthority,
    /// SHA-256 hex of the CSRF token, on a token that a cookie carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub csrf: Option<String>,
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
    let authority = match &claims.authority {
        SessionAuthority::Login(id) | SessionAuthority::Pat(id) => id,
    };
    if authority
        .parse::<PrincipalId>()
        .map_err(|_| refused())?
        .as_str()
        != authority
    {
        return Err(refused());
    }
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
        || claims.csrf.as_deref().is_some_and(|csrf| {
            csrf.len() != 64
                || !csrf
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
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

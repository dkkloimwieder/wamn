//! Fixed Ed25519 session-token primitives, without route or exchange admission.
//!
//! The caller owns fresh identity validation and key freshness. These functions
//! enforce the signed wire profile, scope, and age; returning claims is not an
//! operation-permission grant. No token is persisted or logged.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};

use crate::keys::{PublicSessionKey, decode_public_key};
use crate::{SessionError, SessionErrorKind};

/// Owner-approved maximum lifetime, in seconds.
pub const MAXIMUM_LIFETIME: i64 = 900;
/// Owner-approved clock tolerance, in seconds; expiry is at-or-after exp + 30.
pub const TOLERANCE: i64 = 30;
/// Exact, fully specified signature algorithm identifier from RFC 9864.
pub const SESSION_ALGORITHM: &str = "Ed25519";
/// Exact media type of a WAMN session token.
pub const SESSION_TYPE: &str = "wamn-session+jwt";
/// Longest role slug, in bytes.
pub const MAX_ROLE_LEN: usize = 64;

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

/// The token header. The signer writes it, and verification requires exactly
/// [`SESSION_ALGORITHM`], [`SESSION_TYPE`], and a nonempty key ID.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHeader {
    pub alg: String,
    pub typ: String,
    pub kid: String,
}

impl SessionHeader {
    /// The header of a token signed by the key `kid`.
    pub fn new(kid: &str) -> Self {
        Self {
            alg: SESSION_ALGORITHM.into(),
            typ: SESSION_TYPE.into(),
            kid: kid.into(),
        }
    }
}

/// Check lifetime, future issuance, and expiry as three separate age rules.
pub fn validate_session_age(issued_at: i64, expires_at: i64, now: i64) -> Result<(), SessionError> {
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

/// Read an untrusted key selector; this never establishes issuer trust.
pub fn session_key_id(token: &str) -> Result<String, SessionError> {
    let (header, _, _) = token_parts(token)?;
    Ok(decode_header(header)?.kid)
}

/// Verify the exact profile, signature, configured scope, and claim age.
///
/// `key` must come from the configured issuer's currently fresh complete key
/// set. Key freshness and operation permissions are deliberately outside this
/// primitive. Every token/profile/scope/age refusal has the same public error.
pub fn verify_session_token(
    token: &str,
    key: &PublicSessionKey,
    scope: SessionScope<'_>,
    now: i64,
) -> Result<SessionClaims, SessionError> {
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

/// Refuse claims whose principal, authority, roles, or CSRF digest are not
/// canonical. The signer checks the same shape before it signs.
pub fn validate_claim_shape(claims: &SessionClaims) -> Result<(), SessionError> {
    let authority = match &claims.authority {
        SessionAuthority::Login(id) | SessionAuthority::Pat(id) => id,
    };
    if !is_principal_id(authority)
        || !is_principal_id(&claims.sub)
        || claims.iss.trim().is_empty()
        || claims.org.is_empty()
        || claims.aud.is_empty()
        || claims.jti.is_empty()
        || claims.roles.iter().any(|role| !is_role_slug(role))
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

/// A canonical principal ID: a hyphenated UUID in lowercase hex.
pub fn is_principal_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                matches!(byte, b'0'..=b'9' | b'a'..=b'f')
            }
        })
}

/// A canonical role slug: lowercase letters, digits, and inner hyphens, at
/// most [`MAX_ROLE_LEN`] bytes.
pub fn is_role_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ROLE_LEN
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || (index > 0 && byte == b'-')
        })
}

fn token_parts(token: &str) -> Result<(&str, &str, &str), SessionError> {
    let mut parts = token.split('.');
    let header = parts.next().ok_or_else(refused)?;
    let body = parts.next().ok_or_else(refused)?;
    let signature = parts.next().ok_or_else(refused)?;
    if parts.next().is_some() || header.is_empty() || body.is_empty() || signature.is_empty() {
        return Err(refused());
    }
    Ok((header, body, signature))
}

fn decode_header(encoded: &str) -> Result<SessionHeader, SessionError> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| refused())?;
    let header: SessionHeader = serde_json::from_slice(&bytes).map_err(|_| refused())?;
    if header.alg != SESSION_ALGORITHM || header.typ != SESSION_TYPE || header.kid.is_empty() {
        return Err(refused());
    }
    Ok(header)
}

/// The one public refusal of a token.
pub(crate) fn refused() -> SessionError {
    SessionError::new(SessionErrorKind::Refused, "session token refused")
}

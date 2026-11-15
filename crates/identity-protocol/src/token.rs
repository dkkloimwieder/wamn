//! Fixed Ed25519 session-token primitives, without route or exchange admission.
//!
//! The caller owns fresh identity validation and key freshness. These functions
//! enforce the signed wire profile, audience, and age; returning JWT claims is not an
//! operation-permission grant. No token is persisted or logged.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::keys::{PublicSessionKey, decode_public_key};
use crate::{PrincipalId, RoleName, SessionError, SessionErrorType};

/// Owner-approved maximum lifetime, in seconds.
pub const MAXIMUM_LIFETIME: i64 = 900;
/// Owner-approved clock tolerance, in seconds; expiry is at-or-after exp + 30.
pub(crate) const TOLERANCE: i64 = 30;
/// Exact media type of a WAMN session token.
const SESSION_TYPE: &str = "wamn-session+jwt";

/// Revocable authority that issued a session token.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionAuthority {
    /// An existing password login and its renewal family.
    Login(PrincipalId),
}

/// Required wire JWT claims; deserialization alone does not authenticate them.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionClaims {
    /// Configured identity issuer.
    pub iss: String,
    /// Canonical org-issued user principal UUID.
    pub sub: PrincipalId,
    /// Organization identity.
    pub org: String,
    /// Exact project-environment identity, not its display name.
    pub aud: String,
    /// Environment role slugs, with no implicit default.
    pub roles: Vec<RoleName>,
    /// Exclusive expiry before clock tolerance, in Unix seconds.
    pub exp: i64,
    /// Actual signing time, in Unix seconds.
    pub iat: i64,
    /// Token identifier only; never a server-side session record.
    pub jti: String,
    /// Current server-side authority, validated for every new admission.
    pub authority: SessionAuthority,
    /// SHA-256 hex of the CSRF token, on a token that a cookie carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub csrf: Option<String>,
}

/// Host-configured audience; no field is inferred from the token or its key ID.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionAudience<'a> {
    /// Configured issuer.
    pub issuer: &'a str,
    /// Configured organization identity.
    pub org: &'a str,
    /// Configured exact project-environment identity.
    pub audience: &'a str,
}

/// The token header. The signer writes it, and verification requires exactly
/// EdDSA, [`SESSION_TYPE`], and a nonempty key ID.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionHeader {
    pub alg: Algorithm,
    pub typ: String,
    pub kid: String,
}

/// Build the fixed session header for a signing key.
pub fn session_header(kid: &str) -> Header {
    let mut header = Header::new(Algorithm::EdDSA);
    header.typ = Some(SESSION_TYPE.into());
    header.kid = Some(kid.into());
    header
}

impl SessionClaims {
    /// Make sure of the identity and credential fields before signing.
    pub fn validate(&self) -> Result<(), SessionError> {
        validate_claim_shape(self)
    }
}

/// Make sure of lifetime, future issuance, and expiry as three separate age rules.
pub(crate) fn validate_session_age(
    issued_at: i64,
    expires_at: i64,
    now: i64,
) -> Result<(), SessionError> {
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
pub(crate) fn session_key_id(token: &str) -> Result<String, SessionError> {
    let header = token.split_once('.').ok_or_else(refused)?.0;
    Ok(decode_header(header)?.kid)
}

/// Verify the exact profile, signature, configured audience, and JWT claim age.
///
/// `key` must come from the configured issuer's currently fresh complete key
/// set. Key freshness and operation permissions are deliberately outside this
/// primitive. Every token/profile/audience/age refusal has the same public error.
pub(crate) fn verify_session_token(
    token: &str,
    key: &PublicSessionKey,
    audience: SessionAudience<'_>,
    now: i64,
) -> Result<SessionClaims, SessionError> {
    let kid = session_key_id(token)?;
    if kid != key.kid {
        return Err(refused());
    }
    let key_bytes = decode_public_key(key).map_err(|_| refused())?;
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.set_issuer(&[audience.issuer]);
    validation.set_audience(&[audience.audience]);
    // The verifier owns the clock, including the admission-time test again.
    validation.validate_exp = false;
    let claims = jsonwebtoken::decode::<SessionClaims>(
        token,
        &DecodingKey::from_ed_der(&key_bytes),
        &validation,
    )
    .map_err(|_| refused())?
    .claims;
    validate_claim_shape(&claims)?;
    if claims.org != audience.org {
        return Err(refused());
    }
    validate_session_age(claims.iat, claims.exp, now)?;
    Ok(claims)
}

/// Refuse JWT claims whose principal, authority, roles, or CSRF digest are not
/// canonical. The signer tests the same shape before it signs.
pub(crate) fn validate_claim_shape(claims: &SessionClaims) -> Result<(), SessionError> {
    if claims.iss.trim().is_empty()
        || claims.org.is_empty()
        || claims.aud.is_empty()
        || claims.jti.is_empty()
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

fn decode_header(encoded: &str) -> Result<SessionHeader, SessionError> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| refused())?;
    let header: SessionHeader = serde_json::from_slice(&bytes).map_err(|_| refused())?;
    if header.alg != Algorithm::EdDSA || header.typ != SESSION_TYPE || header.kid.is_empty() {
        return Err(refused());
    }
    Ok(header)
}

/// The one public refusal of a token.
pub(crate) fn refused() -> SessionError {
    SessionError::new(SessionErrorType::Refused, "session token refused")
}

#[cfg(test)]
mod tests;

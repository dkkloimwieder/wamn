//! Password login records and renewal authentication.

use std::fmt;

use wamn_digest::Digest;
use wamn_identity_protocol::{
    PrincipalId,
    secret::{SecretToken, SecretType},
};
use zeroize::Zeroizing;

use crate::{
    identity_error::IdentityError,
    principal::{AuthenticatedPrincipal, Principal},
};

/// Maximum login duration from password authentication, in seconds.
pub const ABSOLUTE_LIFETIME: i64 = 8 * 60 * 60;
/// Maximum time between successful renewals, in seconds.
pub const INACTIVITY_LIFETIME: i64 = 30 * 60;

/// A stored login's immutable identity and absolute deadline.
#[derive(Debug, Clone)]
pub struct Login {
    pub id: String,
    pub principal: PrincipalId,
    pub authenticated_at: i64,
    pub expires_at: i64,
}

/// One replacement credential, returned only after its transaction commits.
pub struct Renewal {
    pub login: Login,
    secret: Zeroizing<String>,
}
impl Renewal {
    /// Return the new credential and its login for delivery after commit.
    pub fn new(login: Login, secret: Zeroizing<String>) -> Self {
        Self { login, secret }
    }

    /// The opaque credential for private delivery to the authenticated caller.
    pub fn secret(&self) -> &str {
        &self.secret
    }
}
impl fmt::Debug for Renewal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Renewal")
            .field("login", &self.login)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

/// Read and revoke a renewal under the caller's transaction.
#[async_trait::async_trait]
pub trait RenewalStore: Send {
    async fn bind_actor(&mut self, principal: &PrincipalId) -> Result<(), IdentityError>;
    async fn lookup(
        &mut self,
        issuer: &str,
        audience: &str,
        digest: &Digest,
    ) -> Result<Option<(Login, bool, bool)>, IdentityError>;
    async fn revoke_family(&mut self, login: &Login) -> Result<(), IdentityError>;
    async fn principal(
        &mut self,
        principal: &PrincipalId,
    ) -> Result<Option<Principal>, IdentityError>;
}

/// Authenticate a renewal credential and bind its user actor under the principal lock.
///
/// Commit a refused result: replay revokes its family. This does not establish
/// current environment authority; the service tests that before issuing tokens.
pub async fn authenticate_renewal(
    store: &mut impl RenewalStore,
    principal: &PrincipalId,
    issuer: &str,
    audience: &str,
    secret: &str,
) -> Result<Option<AuthenticatedPrincipal>, IdentityError> {
    store.bind_actor(principal).await?;
    let Some(digest) = SecretToken::parse(SecretType::Renewal, secret)
        .ok()
        .map(|token| token.digest())
    else {
        return Ok(None);
    };
    let Some((login, consumed, usable)) = store.lookup(issuer, audience, &digest).await? else {
        return Ok(None);
    };
    if &login.principal != principal {
        return Ok(None);
    }
    if consumed {
        store.revoke_family(&login).await?;
        return Ok(None);
    }
    if !usable {
        return Ok(None);
    }
    Ok(store
        .principal(&login.principal)
        .await?
        .map(AuthenticatedPrincipal::admitted))
}

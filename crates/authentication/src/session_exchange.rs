//! Password sessions and renewal under the issuer's transaction.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ring::rand::{SecureRandom as _, SystemRandom};
use wamn_envelope::encryption::Envelope;
use wamn_identity_protocol::{PrincipalId, records::AccessTokenClaims};
use wamn_registry::OrgId;

use crate::control_admission::org_audience_org;
use crate::identity_error::IdentityError;
use crate::password::{Password, PasswordError, PasswordWork};
use crate::password_login::Renewal;
use crate::principal::AuthenticatedPrincipal;
use crate::session_token::IssuedSessionToken;

/// The default signing key rotation interval.
pub const DEFAULT_KEY_ROTATION: Duration = Duration::from_hours(30 * 24);

/// An exchange outcome the HTTP route can report without exposing storage failures.
#[derive(Debug)]
pub enum ExchangeFailure {
    Unauthorized,
    Unavailable,
    Password(PasswordError),
}

/// A session transaction borrowed from the issuer's connection.
pub type SessionTransaction<'connection> = Box<dyn ExchangeTransaction + 'connection>;

/// The active issuer storage used to begin a password or renewal transaction.
#[async_trait::async_trait]
pub trait ExchangeStore: Send {
    async fn begin_login(
        &mut self,
        work: &PasswordWork,
        email: &str,
        password: Password,
    ) -> Result<Option<(SessionTransaction<'_>, AuthenticatedPrincipal)>, ExchangeFailure>;

    async fn begin_renewal(
        &mut self,
        issuer: &str,
        audience: &str,
        secret: &str,
    ) -> Result<Option<(SessionTransaction<'_>, PrincipalId)>, IdentityError>;
}

/// The operations that must share the same transaction through commit.
#[async_trait::async_trait]
pub trait ExchangeTransaction: Send {
    async fn active_member(
        &mut self,
        principal: &PrincipalId,
        org: &OrgId,
    ) -> Result<bool, IdentityError>;

    async fn create_login(
        &mut self,
        principal: &PrincipalId,
        issuer: &str,
        audience: &str,
    ) -> Result<Option<Renewal>, IdentityError>;

    async fn authenticate_renewal(
        &mut self,
        principal: &PrincipalId,
        issuer: &str,
        audience: &str,
        secret: &str,
    ) -> Result<Option<AuthenticatedPrincipal>, IdentityError>;

    async fn rotate_login(
        &mut self,
        issuer: &str,
        audience: &str,
        secret: &str,
    ) -> Result<Option<Renewal>, IdentityError>;

    async fn revoke_login(
        &mut self,
        issuer: &str,
        audience: &str,
        secret: &str,
    ) -> Result<(), IdentityError>;

    async fn revoke_all(&mut self, principal: &PrincipalId) -> Result<(), IdentityError>;

    async fn sign(
        &mut self,
        claims: AccessTokenClaims,
        started_at: i64,
        expires_at: i64,
        envelope: &Envelope,
    ) -> Result<IssuedSessionToken, IdentityError>;

    async fn commit(self: Box<Self>) -> Result<(), IdentityError>;
}

/// The issuer and signing deadline selected by the caller.
#[derive(Debug)]
pub struct Exchange<'a> {
    pub issuer: &'a str,
    pub audience: &'a str,
    pub started_at: i64,
    pub envelope: &'a Envelope,
}

/// The access token and replacement credential, available after commit.
#[derive(Debug)]
pub struct Session {
    pub token: IssuedSessionToken,
    pub renewal: Renewal,
}

/// Create a session after password authentication and current membership.
pub async fn login(
    store: &mut dyn ExchangeStore,
    exchange: Exchange<'_>,
    work: &PasswordWork,
    email: &str,
    password: Password,
) -> Result<Session, ExchangeFailure> {
    let (mut tx, principal) = store
        .begin_login(work, email, password)
        .await?
        .ok_or(ExchangeFailure::Unauthorized)?;
    let renewal = tx
        .create_login(
            principal.principal().id(),
            exchange.issuer,
            exchange.audience,
        )
        .await
        .map_err(|_| ExchangeFailure::Unavailable)?
        .ok_or(ExchangeFailure::Unauthorized)?;
    finish_session(tx, &principal, exchange, renewal).await
}

/// The operation presented with a renewal credential.
#[derive(Debug, Clone, Copy)]
pub enum RenewalAction {
    Renew,
    Logout,
    LogoutAll,
}

/// Renew or revoke a session. Single-session logout also succeeds for an absent login.
pub async fn renewal(
    store: &mut dyn ExchangeStore,
    exchange: Exchange<'_>,
    action: RenewalAction,
    secret: &str,
) -> Result<Option<Session>, ExchangeFailure> {
    let absent = || match action {
        RenewalAction::Logout => Ok(None),
        RenewalAction::Renew | RenewalAction::LogoutAll => Err(ExchangeFailure::Unauthorized),
    };
    let Some((mut tx, actor)) = store
        .begin_renewal(exchange.issuer, exchange.audience, secret)
        .await
        .map_err(|_| ExchangeFailure::Unavailable)?
    else {
        return absent();
    };
    let Some(principal) = tx
        .authenticate_renewal(&actor, exchange.issuer, exchange.audience, secret)
        .await
        .map_err(|_| ExchangeFailure::Unavailable)?
    else {
        // A replay refusal can revoke the family.
        tx.commit()
            .await
            .map_err(|_| ExchangeFailure::Unavailable)?;
        return absent();
    };
    let outcome = match action {
        RenewalAction::Renew => {
            let Some(renewal) = tx
                .rotate_login(exchange.issuer, exchange.audience, secret)
                .await
                .map_err(|_| ExchangeFailure::Unavailable)?
            else {
                tx.commit()
                    .await
                    .map_err(|_| ExchangeFailure::Unavailable)?;
                return Err(ExchangeFailure::Unauthorized);
            };
            return finish_session(tx, &principal, exchange, renewal)
                .await
                .map(Some);
        }
        RenewalAction::Logout => {
            tx.revoke_login(exchange.issuer, exchange.audience, secret)
                .await
        }
        RenewalAction::LogoutAll => tx.revoke_all(principal.principal().id()).await,
    };
    outcome.map_err(|_| ExchangeFailure::Unavailable)?;
    tx.commit()
        .await
        .map_err(|_| ExchangeFailure::Unavailable)?;
    Ok(None)
}

async fn finish_session(
    mut tx: SessionTransaction<'_>,
    principal: &AuthenticatedPrincipal,
    exchange: Exchange<'_>,
    renewal: Renewal,
) -> Result<Session, ExchangeFailure> {
    let org = org_audience_org(exchange.audience).ok_or(ExchangeFailure::Unauthorized)?;
    let principal = principal.principal().id();
    if !tx
        .active_member(principal, &org)
        .await
        .map_err(|_| ExchangeFailure::Unavailable)?
    {
        return Err(ExchangeFailure::Unauthorized);
    }
    let mut random = [0u8; 32];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| ExchangeFailure::Unavailable)?;
    let claims = AccessTokenClaims {
        iss: exchange.issuer.to_owned(),
        sub: *principal,
        aud: exchange.audience.to_owned(),
        exp: 0,
        iat: 0,
        jti: hex::encode(random),
        client_id: "wamn-web".into(),
    };
    let token = tx
        .sign(
            claims,
            exchange.started_at,
            renewal.login.expires_at,
            exchange.envelope,
        )
        .await
        .map_err(|_| ExchangeFailure::Unavailable)?;
    tx.commit()
        .await
        .map_err(|_| ExchangeFailure::Unavailable)?;
    if unix_seconds().is_none_or(|now| now >= token.claims().exp) {
        return Err(ExchangeFailure::Unavailable);
    }
    Ok(Session { token, renewal })
}

pub fn unix_seconds() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs()
        .try_into()
        .ok()
}

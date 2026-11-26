//! Password sessions and renewal under the issuer's transaction.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ring::rand::{SecureRandom as _, SystemRandom};
use wamn_envelope::encryption::Envelope;
use wamn_identity_protocol::{PrincipalId, records::AccessTokenClaims};

use crate::control_admission::org_audience_org;
use crate::identity::{
    issuer::Issuer, password_login, session_token::sign_session_token_in_transaction,
};
use crate::password::{Password, PasswordError, PasswordWork};
use crate::password_login::Renewal;
use crate::principal::AuthenticatedPrincipal;
use crate::session_token::IssuedSessionToken;
use sqlx::{Postgres, Row as _, Transaction};
use uuid::Uuid;
use wamn_authentication::identity_error::{IdentityError, database_error};
use wamn_registry::OrgId;

/// The default signing key rotation interval.
pub const DEFAULT_KEY_ROTATION: Duration = Duration::from_hours(30 * 24);

/// An exchange outcome the HTTP route can report without exposing storage failures.
#[derive(Debug)]
pub enum ExchangeFailure {
    Unauthorized,
    Unavailable,
    Password(PasswordError),
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
    store: &mut Issuer<'_>,
    exchange: Exchange<'_>,
    work: &PasswordWork,
    email: &str,
    password: Password,
) -> Result<Session, ExchangeFailure> {
    let (mut tx, principal) = store
        .begin_login(work, email, password)
        .await?
        .ok_or(ExchangeFailure::Unauthorized)?;
    let renewal = password_login::create_login(
        &mut tx,
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
    store: &mut Issuer<'_>,
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
    let Some(principal) = password_login::authenticate_renewal(
        &mut tx,
        &actor,
        exchange.issuer,
        exchange.audience,
        secret,
    )
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
            let Some(renewal) =
                password_login::rotate_login(&mut tx, exchange.issuer, exchange.audience, secret)
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
            password_login::revoke_login(&mut tx, exchange.issuer, exchange.audience, secret).await
        }
        RenewalAction::LogoutAll => {
            password_login::revoke_all(&mut tx, principal.principal().id()).await
        }
    };
    outcome.map_err(|_| ExchangeFailure::Unavailable)?;
    tx.commit()
        .await
        .map_err(|_| ExchangeFailure::Unavailable)?;
    Ok(None)
}

async fn finish_session(
    mut tx: Transaction<'_, Postgres>,
    principal: &AuthenticatedPrincipal,
    exchange: Exchange<'_>,
    renewal: Renewal,
) -> Result<Session, ExchangeFailure> {
    let org = org_audience_org(exchange.audience).ok_or(ExchangeFailure::Unauthorized)?;
    let principal = principal.principal().id();
    if !active_member(&mut tx, principal, &org)
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
    let token = sign_session_token_in_transaction(
        &mut tx,
        claims,
        exchange.started_at,
        Some(renewal.login.expires_at),
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

async fn active_member(
    client: &mut sqlx::PgConnection,
    principal: &PrincipalId,
    org: &OrgId,
) -> Result<bool, IdentityError> {
    let member = sqlx::query("SELECT EXISTS (SELECT 1 FROM identity.org_membership m JOIN identity.principal p ON p.id=m.principal_id JOIN registry.org o ON o.id=m.org AND o.active WHERE m.principal_id=$1 AND m.org=$2 AND m.active AND p.type='user' AND p.active)")
        .bind(Uuid::from(*principal)).bind(org.as_str()).fetch_one(client).await.map_err(|error| database_error(&error))?.get(0);
    Ok(member)
}

pub fn unix_seconds() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs()
        .try_into()
        .ok()
}

//! Password sessions and renewal under the issuer's transaction.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ring::rand::{SecureRandom as _, SystemRandom};
use wamn_envelope::encryption::Envelope;
use wamn_identity_protocol::{PrincipalId, records::AccessTokenClaims};

use crate::authentication_event::{Event, Outcome, commit_success};
use crate::control_admission::org_audience_org;
use crate::identity::{
    issuer::Issuer, password_login, session_token::sign_session_token_in_transaction,
};
use crate::password::{Password, PasswordError, PasswordWork};
use crate::password_login::Renewal;
use crate::principal::AuthenticatedPrincipal;
use crate::session_token::IssuedSessionToken;
use sqlx::{Postgres, Transaction};
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
    mut event: Option<&mut Event>,
) -> Result<Session, ExchangeFailure> {
    let (mut tx, principal) = store
        .begin_login(work, email, password, event.as_deref_mut())
        .await?
        .ok_or(ExchangeFailure::Unauthorized)?;
    if let Some(event) = &mut event {
        event.principal_id = Some(*principal.principal().id());
    }
    let renewal = password_login::create_login(
        &mut tx,
        principal.principal().id(),
        exchange.issuer,
        exchange.audience,
    )
    .await
    .map_err(|_| ExchangeFailure::Unavailable)?
    .ok_or(ExchangeFailure::Unauthorized)?;
    finish_session(tx, &principal, exchange, renewal, event).await
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
    event: &mut Event,
) -> Result<Option<Session>, ExchangeFailure> {
    let absent = || match action {
        RenewalAction::Logout => Ok(None),
        RenewalAction::Renew | RenewalAction::LogoutAll => Err(ExchangeFailure::Unauthorized),
    };
    let Some((mut tx, actor)) = store
        .begin_renewal(exchange.issuer, exchange.audience, secret, event)
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
        event,
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
            let renewal = match password_login::rotate_login_decision(
                &mut tx,
                exchange.issuer,
                exchange.audience,
                secret,
            )
            .await
            .map_err(|_| ExchangeFailure::Unavailable)?
            {
                Ok(renewal) => renewal,
                Err(outcome) => {
                    tx.commit()
                        .await
                        .map_err(|_| ExchangeFailure::Unavailable)?;
                    event.outcome = Some(outcome);
                    return Err(ExchangeFailure::Unauthorized);
                }
            };
            return finish_session(tx, &principal, exchange, renewal, Some(event))
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
    // Revocation is durable even if the request's event cannot be appended.
    event.outcome = Some(Outcome::Succeeded);
    Ok(None)
}

async fn finish_session(
    mut tx: Transaction<'_, Postgres>,
    principal: &AuthenticatedPrincipal,
    exchange: Exchange<'_>,
    renewal: Renewal,
    mut event: Option<&mut Event>,
) -> Result<Session, ExchangeFailure> {
    let Some(org) = org_audience_org(exchange.audience) else {
        if let Some(event) = event {
            event.outcome = Some(Outcome::AudienceInvalid);
        }
        return Err(ExchangeFailure::Unauthorized);
    };
    let principal = principal.principal().id();
    if let Some(outcome) = membership_refusal(&mut tx, principal, &org)
        .await
        .map_err(|_| ExchangeFailure::Unavailable)?
    {
        if let Some(event) = event {
            event.outcome = Some(outcome);
        }
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
    .map_err(|_| {
        if let Some(event) = &mut event {
            event.outcome = Some(Outcome::SigningFailed);
        }
        ExchangeFailure::Unavailable
    })?;
    if let Some(event) = &mut event {
        event.login_id = renewal.login.id.parse().ok();
    }
    commit_success(tx, event, Outcome::Succeeded, Some(&token.claims().jti))
        .await
        .map_err(|_| ExchangeFailure::Unavailable)?;
    if unix_seconds().is_none_or(|now| now >= token.claims().exp) {
        return Err(ExchangeFailure::Unavailable);
    }
    Ok(Session { token, renewal })
}

async fn membership_refusal(
    client: &mut sqlx::PgConnection,
    principal: &PrincipalId,
    org: &OrgId,
) -> Result<Option<Outcome>, IdentityError> {
    let (org_active, member_active, user_active): (Option<bool>, Option<bool>, Option<bool>) =
        sqlx::query_as(
            "SELECT o.active, m.active, p.active FROM (VALUES ($1::uuid, $2::text)) request(principal, org)
             LEFT JOIN registry.org o ON o.id = request.org
             LEFT JOIN identity.org_membership m ON m.org = o.id AND m.principal_id = request.principal
             LEFT JOIN identity.principal p ON p.id = request.principal AND p.type = 'user'",
        )
        .bind(Uuid::from(*principal))
        .bind(org.as_str())
        .fetch_one(client)
        .await
        .map_err(|error| database_error(&error))?;
    Ok(
        org_membership_refusal(org_active, member_active).or(match user_active {
            Some(true) => None,
            Some(false) => Some(Outcome::PrincipalInactive),
            None => Some(Outcome::PrincipalUnknown),
        }),
    )
}

pub(crate) fn org_membership_refusal(
    org_active: Option<bool>,
    member_active: Option<bool>,
) -> Option<Outcome> {
    match (org_active, member_active) {
        (None, _) => Some(Outcome::OrgUnknown),
        (Some(false), _) => Some(Outcome::OrgInactive),
        (_, None) => Some(Outcome::MembershipMissing),
        (_, Some(false)) => Some(Outcome::MembershipInactive),
        (Some(true), Some(true)) => None,
    }
}

pub fn unix_seconds() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs()
        .try_into()
        .ok()
}

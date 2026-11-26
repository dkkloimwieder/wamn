//! Issuer storage for password requests, exchanges and workload admission.

use crate::authentication_event::{Event, Outcome};
use crate::password::{Password, PasswordError, PasswordWork};
use crate::principal::AuthenticatedPrincipal;
use crate::session_exchange::ExchangeFailure;
use sqlx::{Connection as _, PgConnection, Postgres, Row as _, Transaction};
use uuid::Uuid;
use wamn_authentication::identity_error::IdentityError;
use wamn_identity_protocol::{PlatformComponent, PrincipalId};
use wamn_registry::OrgId;

use super::{password, password_login, transaction};
use wamn_authentication::identity_error::database_error;

/// The first denial owns the event for its bucket and window.
#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    Allowed,
    Denied(Option<Outcome>),
}

/// An issuer connection used by the identity rules.
#[derive(Debug)]
pub struct Issuer<'a>(pub &'a mut PgConnection);

async fn bind_issuer_operation(
    tx: &mut Transaction<'_, Postgres>,
    actor: &PrincipalId,
) -> Result<(), sqlx::Error> {
    wamn_postgres::bind_transaction_identity(
        tx,
        actor,
        "",
        "identity",
        Some(&PlatformComponent::IdentityService.principal_name()),
        None,
    )
    .await
}

/// Open the issuer's transaction for a user administration write.
pub async fn begin_user_write(
    client: &mut PgConnection,
) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
    let mut tx = client
        .begin_with("BEGIN ISOLATION LEVEL READ COMMITTED")
        .await?;
    bind_issuer_operation(&mut tx, &PlatformComponent::IdentityService.principal_id()).await?;
    Ok(tx)
}

impl Issuer<'_> {
    /// Read the org's active flag, if the org is registered.
    pub async fn org_activity(&mut self, org: &OrgId) -> Result<Option<bool>, IdentityError> {
        let actor = PlatformComponent::IdentityService.principal_id();
        let mut tx = transaction::read_connection(self.0, &actor)
            .await
            .map_err(|error| database_error(&error))?;
        let active = sqlx::query_scalar("SELECT active FROM registry.org WHERE id = $1")
            .bind(org.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| database_error(&error))?;
        tx.commit().await.map_err(|error| database_error(&error))?;
        Ok(active)
    }

    pub async fn admit(
        &mut self,
        issuer: &str,
        buckets: &[(&str, i32)],
    ) -> Result<Admission, IdentityError> {
        let mut tx = self
            .0
            .begin()
            .await
            .map_err(|error| database_error(&error))?;
        bind_issuer_operation(&mut tx, &PlatformComponent::IdentityService.principal_id())
            .await
            .map_err(|error| database_error(&error))?;
        // The upsert owns the row lock, including when another replica inserts the bucket first.
        // An over-limit earlier bucket prevents creation of subsequent buckets.
        let mut admission = Admission::Allowed;
        for (bucket, limit) in buckets {
            let (previous, count): (Option<i32>, i32) = sqlx::query_as(
                "INSERT INTO identity.password_attempt (bucket, started_at, attempts)
                 VALUES ($1, clock_timestamp(), 1)
                 ON CONFLICT (bucket) DO UPDATE SET
                   attempts = CASE WHEN identity.password_attempt.started_at <= EXCLUDED.started_at - interval '60 seconds'
                                   THEN 1 ELSE LEAST(identity.password_attempt.attempts + 1, $2 + 1) END,
                   started_at = CASE WHEN identity.password_attempt.started_at <= EXCLUDED.started_at - interval '60 seconds'
                                     THEN EXCLUDED.started_at ELSE identity.password_attempt.started_at END
                 RETURNING OLD.attempts, NEW.attempts",
            )
            .bind(bucket)
            .bind(limit)
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| database_error(&error))?;
            if count > *limit {
                admission = Admission::Denied((previous == Some(*limit)).then(|| {
                    if *bucket == "global" {
                        Outcome::GlobalRateLimited
                    } else if bucket.starts_with("source:") {
                        Outcome::SourceRateLimited
                    } else {
                        Outcome::AccountRateLimited
                    }
                }));
                break;
            }
        }
        if buckets.first().is_some_and(|(key, _)| *key == "global") {
            password_login::prune_expired(&mut tx, issuer).await?;
            sqlx::query("DELETE FROM identity.password_attempt WHERE started_at < clock_timestamp() - interval '120 seconds'").execute(&mut *tx).await.map_err(|error| database_error(&error))?;
        }
        tx.commit().await.map_err(|error| database_error(&error))?;
        Ok(admission)
    }

    pub(crate) async fn recovery_principal(
        &mut self,
        email: &str,
        event: &mut Event,
    ) -> Result<Option<PrincipalId>, IdentityError> {
        let mut tx = transaction::read_connection(
            self.0,
            &PlatformComponent::IdentityService.principal_id(),
        )
        .await
        .map_err(|error| database_error(&error))?;
        let account: Option<(Uuid, bool, bool)> = sqlx::query_as(
            "SELECT p.id, p.active, c.principal_id IS NOT NULL
             FROM identity.principal p LEFT JOIN identity.password_credential c ON c.principal_id=p.id
             WHERE p.email=$1 AND p.type='user'",
        )
        .bind(email)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| database_error(&error))?;
        tx.commit().await.map_err(|error| database_error(&error))?;
        let Some((id, active, enrolled)) = account else {
            event.outcome = Some(Outcome::UnknownEmail);
            return Ok(None);
        };
        let principal = PrincipalId::from(id);
        event.principal_id = Some(principal);
        if !active || !enrolled {
            event.outcome = Some(if active {
                Outcome::CredentialMissing
            } else {
                Outcome::PrincipalInactive
            });
            return Ok(None);
        }
        Ok(Some(principal))
    }

    pub async fn enrollment_email(
        &mut self,
        principal: &PrincipalId,
    ) -> Result<Option<String>, IdentityError> {
        let mut tx = transaction::read_connection(self.0, principal)
            .await
            .map_err(|error| database_error(&error))?;
        let Ok(Some(row)) = sqlx::query("SELECT email FROM identity.principal WHERE id = $1")
            .bind(Uuid::from(*principal))
            .fetch_optional(&mut *tx)
            .await
        else {
            return Ok(None);
        };
        let _ = tx.commit().await;
        Ok(Some(row.get(0)))
    }

    pub(crate) async fn invitation_email(
        &mut self,
        org: &OrgId,
        principal: &PrincipalId,
        event: &mut Event,
    ) -> Result<Option<String>, IdentityError> {
        let mut tx = transaction::read_connection(
            self.0,
            &PlatformComponent::IdentityService.principal_id(),
        )
        .await
        .map_err(|error| database_error(&error))?;
        let (org_active, member_active, account_active, email): (Option<bool>, Option<bool>, Option<bool>, Option<String>) =
            sqlx::query_as(
                "SELECT o.active, m.active, p.active, p.email
                 FROM (VALUES ($1::uuid, $2::text)) request(principal, org)
                 LEFT JOIN registry.org o ON o.id = request.org
                 LEFT JOIN identity.org_membership m ON m.org = o.id AND m.principal_id = request.principal
                 LEFT JOIN identity.principal p ON p.id = request.principal AND p.type = 'user'",
            )
            .bind(Uuid::from(*principal))
            .bind(org.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| database_error(&error))?;
        tx.commit().await.map_err(|error| database_error(&error))?;
        if account_active.is_some() {
            event.principal_id = Some(*principal);
        }
        if let Some(outcome) = match account_active {
            None => Some(Outcome::PrincipalUnknown),
            Some(false) => Some(Outcome::PrincipalInactive),
            Some(true) => {
                crate::session_exchange::org_membership_refusal(org_active, member_active)
            }
        } {
            event.outcome = Some(outcome);
            return Ok(None);
        }
        Ok(email)
    }

    pub(crate) async fn consume_email_token(
        &mut self,
        token: &crate::password::Invitation,
    ) -> Result<(), sqlx::Error> {
        let digest = wamn_digest::bytes_sha256(token.secret().as_bytes());
        let mut tx = begin_user_write(self.0).await?;
        sqlx::raw_sql("SET LOCAL statement_timeout = '750ms'")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE identity.password_token SET consumed_at = clock_timestamp()
             WHERE token_hash = $1 AND consumed_at IS NULL",
        )
        .bind(digest.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }

    pub async fn enroll(
        &mut self,
        work: &PasswordWork,
        principal: &PrincipalId,
        secret: &str,
        password: Password,
        event: &mut Event,
    ) -> Result<(), PasswordError> {
        password::establish_password(
            self.0,
            work,
            principal,
            secret,
            password,
            crate::password::INVITATION_PURPOSE,
            Some(event),
        )
        .await
    }

    pub async fn reset(
        &mut self,
        work: &PasswordWork,
        principal: &PrincipalId,
        secret: &str,
        password: Password,
        event: &mut Event,
    ) -> Result<(), PasswordError> {
        password::establish_password(
            self.0,
            work,
            principal,
            secret,
            password,
            crate::password::RESET_PURPOSE,
            Some(event),
        )
        .await
    }
}

impl Issuer<'_> {
    pub(crate) async fn begin_login(
        &mut self,
        work: &PasswordWork,
        email: &str,
        password: Password,
        mut event: Option<&mut Event>,
    ) -> Result<Option<(Transaction<'_, Postgres>, AuthenticatedPrincipal)>, ExchangeFailure> {
        let mut read = transaction::read_connection(
            self.0,
            &PlatformComponent::IdentityService.principal_id(),
        )
        .await
        .map_err(|_| ExchangeFailure::Unavailable)?;
        let row =
            sqlx::query("SELECT id::text FROM identity.principal WHERE email=$1 AND type='user'")
                .bind(email)
                .fetch_optional(&mut *read)
                .await
                .map_err(|_| ExchangeFailure::Unavailable)?;
        let Some(row) = row else {
            let result =
                password::authenticate_password_with_event(&mut read, work, email, password, event)
                    .await;
            read.commit()
                .await
                .map_err(|_| ExchangeFailure::Unavailable)?;
            result.map_err(ExchangeFailure::Password)?;
            return Ok(None);
        };
        read.commit()
            .await
            .map_err(|_| ExchangeFailure::Unavailable)?;
        let id = row
            .get::<String, _>(0)
            .parse::<PrincipalId>()
            .map_err(|_| ExchangeFailure::Unavailable)?;
        if let Some(event) = &mut event {
            event.principal_id = Some(id);
        }
        let mut tx = self
            .0
            .begin()
            .await
            .map_err(|_| ExchangeFailure::Unavailable)?;
        bind_issuer_operation(&mut tx, &id)
            .await
            .map_err(|_| ExchangeFailure::Unavailable)?;
        password_login::lock_principal(&mut tx, &id)
            .await
            .map_err(|_| ExchangeFailure::Unavailable)?;
        let principal =
            password::authenticate_password_with_event(&mut tx, work, email, password, event)
                .await
                .map_err(ExchangeFailure::Password)?;
        Ok(principal.map(|principal| (tx, principal)))
    }

    pub(crate) async fn begin_renewal(
        &mut self,
        issuer: &str,
        audience: &str,
        secret: &str,
        event: &mut Event,
    ) -> Result<Option<(Transaction<'_, Postgres>, PrincipalId)>, IdentityError> {
        let (actor, login_id) =
            match password_login::renewal_principal(self.0, issuer, audience, secret).await? {
                Ok(login) => login,
                Err(outcome) => {
                    event.outcome = Some(outcome);
                    return Ok(None);
                }
            };
        event.principal_id = Some(actor);
        event.login_id = Some(login_id);
        let tx = self
            .0
            .begin()
            .await
            .map_err(|error| database_error(&error))?;
        Ok(Some((tx, actor)))
    }
}

/// Context for a failed stage in issuer maintenance.
#[derive(Debug)]
pub struct MaintenanceError {
    pub context: &'static str,
    pub source: sqlx::Error,
}

/// Prune old transaction identities with the issuer's fixed statement deadline.
pub async fn prune_tx_identity(client: &mut PgConnection) -> Result<(), MaintenanceError> {
    let mut tx = client.begin().await.map_err(|source| MaintenanceError {
        context: "begin transaction identity cleanup failed",
        source,
    })?;
    sqlx::raw_sql("SET LOCAL statement_timeout = '5s'")
        .execute(&mut *tx)
        .await
        .map_err(|source| MaintenanceError {
            context: "set transaction identity cleanup timeout failed",
            source,
        })?;
    sqlx::query("SELECT platform.prune_tx_identity()")
        .execute(&mut *tx)
        .await
        .map_err(|source| MaintenanceError {
            context: "prune system transaction identities failed",
            source,
        })?;
    tx.commit().await.map_err(|source| MaintenanceError {
        context: "commit transaction identity cleanup failed",
        source,
    })
}

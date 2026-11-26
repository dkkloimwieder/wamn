//! Issuer storage for password requests, exchanges and workload admission.

use crate::password::{Invitation, Password, PasswordError, PasswordWork};
use crate::password_requests::EmailDelivery;
use crate::principal::AuthenticatedPrincipal;
use crate::session_exchange::ExchangeFailure;
use sqlx::{Connection as _, PgConnection, Postgres, Row as _, Transaction};
use uuid::Uuid;
use wamn_authentication::identity_error::IdentityError;
use wamn_digest::Digest;
use wamn_identity_protocol::{PlatformComponent, PrincipalId};
use wamn_registry::OrgId;

use super::{password, password_login, transaction};
use wamn_authentication::identity_error::database_error;

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
    pub async fn registered(&mut self, org: &OrgId) -> Result<bool, IdentityError> {
        let actor = PlatformComponent::IdentityService.principal_id();
        let mut tx = transaction::read_connection(self.0, &actor)
            .await
            .map_err(|error| database_error(&error))?;
        let exists = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM registry.org WHERE id = $1)")
            .bind(org.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| database_error(&error))?;
        tx.commit().await.map_err(|error| database_error(&error))?;
        Ok(exists)
    }

    pub async fn admit(
        &mut self,
        issuer: &str,
        buckets: &[(&str, i32)],
    ) -> Result<bool, IdentityError> {
        let mut tx = self
            .0
            .begin()
            .await
            .map_err(|error| database_error(&error))?;
        bind_issuer_operation(&mut tx, &PlatformComponent::IdentityService.principal_id())
            .await
            .map_err(|error| database_error(&error))?;
        // An over-limit earlier bucket prevents creation of subsequent buckets.
        let mut allowed = true;
        for (bucket, limit) in buckets {
            let count: i32 = sqlx::query("INSERT INTO identity.password_attempt (bucket, started_at, attempts) VALUES ($1, clock_timestamp(), 1) ON CONFLICT (bucket) DO UPDATE SET attempts = CASE WHEN identity.password_attempt.started_at <= EXCLUDED.started_at - interval '60 seconds' THEN 1 ELSE LEAST(identity.password_attempt.attempts + 1, $2 + 1) END, started_at = CASE WHEN identity.password_attempt.started_at <= EXCLUDED.started_at - interval '60 seconds' THEN EXCLUDED.started_at ELSE identity.password_attempt.started_at END RETURNING attempts")
                .bind(bucket).bind(limit).fetch_one(&mut *tx).await.map_err(|error| database_error(&error))?.get(0);
            if count > *limit {
                allowed = false;
                break;
            }
        }
        if buckets.first().is_some_and(|(key, _)| *key == "global") {
            password_login::prune_expired(&mut tx, issuer).await?;
            sqlx::query("DELETE FROM identity.password_attempt WHERE started_at < clock_timestamp() - interval '120 seconds'").execute(&mut *tx).await.map_err(|error| database_error(&error))?;
        }
        tx.commit().await.map_err(|error| database_error(&error))?;
        Ok(allowed)
    }

    pub async fn recovery_principal(
        &mut self,
        email: &str,
    ) -> Result<Option<PrincipalId>, IdentityError> {
        let mut tx = transaction::read_connection(
            self.0,
            &PlatformComponent::IdentityService.principal_id(),
        )
        .await
        .map_err(|error| database_error(&error))?;
        let row = sqlx::query("SELECT p.id FROM identity.principal p JOIN identity.password_credential c ON c.principal_id=p.id WHERE p.email=$1 AND p.type='user' AND p.active")
            .bind(email).fetch_optional(&mut *tx).await.map_err(|error| database_error(&error))?;
        tx.commit().await.map_err(|error| database_error(&error))?;
        row.map(|row| {
            row.try_get::<Uuid, _>(0)
                .map(PrincipalId::from)
                .map_err(|error| database_error(&error))
        })
        .transpose()
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

    pub async fn invitation_email(
        &mut self,
        org: &OrgId,
        principal: &PrincipalId,
    ) -> Result<Option<String>, IdentityError> {
        let mut tx = transaction::read_connection(
            self.0,
            &PlatformComponent::IdentityService.principal_id(),
        )
        .await
        .map_err(|error| database_error(&error))?;
        let email = sqlx::query_scalar(
            "SELECT p.email FROM identity.principal p
             JOIN identity.org_membership m ON m.principal_id = p.id
             WHERE p.id = $1 AND p.type = 'user' AND p.active
                   AND m.org = $2 AND m.active",
        )
        .bind(Uuid::from(*principal))
        .bind(org.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| database_error(&error))?;
        tx.commit().await.map_err(|error| database_error(&error))?;
        Ok(email)
    }

    pub async fn enroll(
        &mut self,
        work: &PasswordWork,
        principal: &PrincipalId,
        secret: &str,
        password: Password,
    ) -> Result<(), PasswordError> {
        password::enroll_password(self.0, work, principal, secret, password).await
    }

    pub async fn reset(
        &mut self,
        work: &PasswordWork,
        principal: &PrincipalId,
        secret: &str,
        password: Password,
    ) -> Result<(), PasswordError> {
        password::reset_password(self.0, work, principal, secret, password).await
    }

    pub async fn issue_reset(
        &mut self,
        principal: &PrincipalId,
        email: &str,
    ) -> Result<Invitation, PasswordError> {
        password::issue_reset(
            self.0,
            &PlatformComponent::IdentityService.principal_id(),
            principal,
            email,
        )
        .await
    }

    pub async fn issue_invitation(
        &mut self,
        principal: &PrincipalId,
    ) -> Result<Invitation, PasswordError> {
        password::issue_invitation(
            self.0,
            &PlatformComponent::IdentityService.principal_id(),
            principal,
        )
        .await
    }

    pub async fn consume_email_token(
        &mut self,
        digest: &Digest,
        delivery: EmailDelivery,
    ) -> Result<(), IdentityError> {
        let result = match delivery {
            EmailDelivery::Invitation => begin_user_write(self.0).await,
            EmailDelivery::Recovery => {
                async {
                    let mut tx = self.0.begin().await?;
                    bind_issuer_operation(
                        &mut tx,
                        &PlatformComponent::IdentityService.principal_id(),
                    )
                    .await?;
                    Ok(tx)
                }
                .await
            }
        };
        let mut tx = result.map_err(|error| database_error(&error))?;
        sqlx::query(
            "UPDATE identity.password_token SET consumed_at = clock_timestamp()
             WHERE token_hash = $1 AND consumed_at IS NULL",
        )
        .bind(digest.to_string())
        .execute(&mut *tx)
        .await
        .map_err(|error| database_error(&error))?;
        tx.commit().await.map_err(|error| database_error(&error))
    }
}

impl Issuer<'_> {
    pub(crate) async fn begin_login(
        &mut self,
        work: &PasswordWork,
        email: &str,
        password: Password,
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
            let result = password::authenticate_password(&mut read, work, email, password).await;
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
        let principal = password::authenticate_password(&mut tx, work, email, password)
            .await
            .map_err(ExchangeFailure::Password)?;
        Ok(principal.map(|principal| (tx, principal)))
    }

    pub(crate) async fn begin_renewal(
        &mut self,
        issuer: &str,
        audience: &str,
        secret: &str,
    ) -> Result<Option<(Transaction<'_, Postgres>, PrincipalId)>, IdentityError> {
        let Some(actor) =
            password_login::renewal_principal(self.0, issuer, audience, secret).await?
        else {
            return Ok(None);
        };
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

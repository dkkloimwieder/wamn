//! Issuer storage for password requests, exchanges and workload admission.

use sqlx::{Connection as _, PgConnection, Postgres, Row as _, Transaction};
use uuid::Uuid;
use wamn_authentication::identity_error::IdentityError;
use wamn_authentication::password::{Invitation, Password, PasswordError, PasswordWork};
use wamn_authentication::password_login::Renewal;
use wamn_authentication::password_requests::{EmailDelivery, PasswordStore};
use wamn_authentication::principal::AuthenticatedPrincipal;
use wamn_authentication::session_exchange::{
    ExchangeFailure, ExchangeStore, ExchangeTransaction, SessionTransaction,
};
use wamn_authentication::session_token::IssuedSessionToken;
use wamn_authentication::workload_identity::WorkloadRegistry;
use wamn_digest::Digest;
use wamn_envelope::encryption::Envelope;
use wamn_identity_protocol::{PlatformComponent, PrincipalId, records::AccessTokenClaims};
use wamn_registry::OrgId;

use super::{password, password_login, session_token, transaction};
use crate::identity_error::database_error;

/// An issuer connection used by the identity rules.
#[derive(Debug)]
pub struct Issuer<'a>(pub &'a mut PgConnection);

/// A connection bound to the issuer's session transaction.
#[derive(Debug)]
struct IssuerTransaction<'a>(Transaction<'a, Postgres>);

async fn bind_issuer_operation(
    tx: &mut Transaction<'_, Postgres>,
    actor: &PrincipalId,
) -> Result<(), sqlx::Error> {
    crate::bind_transaction_identity(
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

#[async_trait::async_trait]
impl WorkloadRegistry for Issuer<'_> {
    async fn registered(&mut self, org: &OrgId) -> Result<bool, IdentityError> {
        let actor = PlatformComponent::IdentityService.principal_id();
        let mut tx = transaction::read_connection(self.0, &actor)
            .await
            .map_err(|error| database_error(&error))?;
        let exists =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM registry.orgs WHERE id = $1)")
                .bind(org.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(|error| database_error(&error))?;
        tx.commit().await.map_err(|error| database_error(&error))?;
        Ok(exists)
    }
}

#[async_trait::async_trait]
impl PasswordStore for Issuer<'_> {
    async fn admit(
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
            let count: i32 = sqlx::query("INSERT INTO identity.password_attempts (bucket, started_at, attempts) VALUES ($1, clock_timestamp(), 1) ON CONFLICT (bucket) DO UPDATE SET attempts = CASE WHEN identity.password_attempts.started_at <= EXCLUDED.started_at - interval '60 seconds' THEN 1 ELSE LEAST(identity.password_attempts.attempts + 1, $2 + 1) END, started_at = CASE WHEN identity.password_attempts.started_at <= EXCLUDED.started_at - interval '60 seconds' THEN EXCLUDED.started_at ELSE identity.password_attempts.started_at END RETURNING attempts")
                .bind(bucket).bind(limit).fetch_one(&mut *tx).await.map_err(|error| database_error(&error))?.get(0);
            if count > *limit {
                allowed = false;
                break;
            }
        }
        if buckets.first().is_some_and(|(key, _)| *key == "global") {
            password_login::prune_expired(&mut tx, issuer).await?;
            sqlx::query("DELETE FROM identity.password_attempts WHERE started_at < clock_timestamp() - interval '120 seconds'").execute(&mut *tx).await.map_err(|error| database_error(&error))?;
        }
        tx.commit().await.map_err(|error| database_error(&error))?;
        Ok(allowed)
    }

    async fn recovery_principal(
        &mut self,
        email: &str,
    ) -> Result<Option<PrincipalId>, IdentityError> {
        let mut tx = transaction::read_connection(
            self.0,
            &PlatformComponent::IdentityService.principal_id(),
        )
        .await
        .map_err(|error| database_error(&error))?;
        let row = sqlx::query("SELECT p.id FROM identity.principals p JOIN identity.password_credentials c ON c.principal_id=p.id WHERE p.email=$1 AND p.type='user' AND p.status='active'")
            .bind(email).fetch_optional(&mut *tx).await.map_err(|error| database_error(&error))?;
        tx.commit().await.map_err(|error| database_error(&error))?;
        row.map(|row| {
            row.try_get::<Uuid, _>(0)
                .map(PrincipalId::from)
                .map_err(|error| database_error(&error))
        })
        .transpose()
    }

    async fn enrollment_email(
        &mut self,
        principal: &PrincipalId,
    ) -> Result<Option<String>, IdentityError> {
        let mut tx = transaction::read_connection(self.0, principal)
            .await
            .map_err(|error| database_error(&error))?;
        let Ok(Some(row)) = sqlx::query("SELECT email FROM identity.principals WHERE id = $1")
            .bind(Uuid::from(*principal))
            .fetch_optional(&mut *tx)
            .await
        else {
            return Ok(None);
        };
        let _ = tx.commit().await;
        Ok(Some(row.get(0)))
    }

    async fn invitation_email(
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
            "SELECT p.email FROM identity.principals p
             JOIN identity.org_memberships m ON m.principal_id = p.id
             WHERE p.id = $1 AND p.type = 'user' AND p.status = 'active'
                   AND m.org = $2 AND m.status = 'active'",
        )
        .bind(Uuid::from(*principal))
        .bind(org.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| database_error(&error))?;
        tx.commit().await.map_err(|error| database_error(&error))?;
        Ok(email)
    }

    async fn enroll(
        &mut self,
        work: &PasswordWork,
        principal: &PrincipalId,
        secret: &str,
        password: Password,
    ) -> Result<(), PasswordError> {
        password::enroll_password(self.0, work, principal, secret, password).await
    }

    async fn reset(
        &mut self,
        work: &PasswordWork,
        principal: &PrincipalId,
        secret: &str,
        password: Password,
    ) -> Result<(), PasswordError> {
        password::reset_password(self.0, work, principal, secret, password).await
    }

    async fn issue_reset(
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

    async fn issue_invitation(
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

    async fn consume_email_token(
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
            "UPDATE identity.password_tokens SET consumed_at = clock_timestamp()
             WHERE token_hash = $1 AND consumed_at IS NULL",
        )
        .bind(digest.to_string())
        .execute(&mut *tx)
        .await
        .map_err(|error| database_error(&error))?;
        tx.commit().await.map_err(|error| database_error(&error))
    }
}

#[async_trait::async_trait]
impl ExchangeStore for Issuer<'_> {
    async fn begin_login(
        &mut self,
        work: &PasswordWork,
        email: &str,
        password: Password,
    ) -> Result<Option<(SessionTransaction<'_>, AuthenticatedPrincipal)>, ExchangeFailure> {
        let mut read = transaction::read_connection(
            self.0,
            &PlatformComponent::IdentityService.principal_id(),
        )
        .await
        .map_err(|_| ExchangeFailure::Unavailable)?;
        let row =
            sqlx::query("SELECT id::text FROM identity.principals WHERE email=$1 AND type='user'")
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
        Ok(principal.map(|principal| {
            let tx: SessionTransaction<'_> = Box::new(IssuerTransaction(tx));
            (tx, principal)
        }))
    }

    async fn begin_renewal(
        &mut self,
        issuer: &str,
        audience: &str,
        secret: &str,
    ) -> Result<Option<(SessionTransaction<'_>, PrincipalId)>, IdentityError> {
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
        Ok(Some((Box::new(IssuerTransaction(tx)), actor)))
    }
}

#[async_trait::async_trait]
impl ExchangeTransaction for IssuerTransaction<'_> {
    async fn active_member(
        &mut self,
        principal: &PrincipalId,
        org: &OrgId,
    ) -> Result<bool, IdentityError> {
        let member = sqlx::query("SELECT EXISTS (SELECT 1 FROM identity.org_memberships m JOIN identity.principals p ON p.id=m.principal_id WHERE m.principal_id=$1 AND m.org=$2 AND m.status='active' AND p.type='user' AND p.status='active')")
            .bind(Uuid::from(*principal)).bind(org.as_str()).fetch_one(&mut *self.0).await.map_err(|error| database_error(&error))?.get(0);
        Ok(member)
    }

    async fn create_login(
        &mut self,
        principal: &PrincipalId,
        issuer: &str,
        audience: &str,
    ) -> Result<Option<Renewal>, IdentityError> {
        password_login::create_login(&mut self.0, principal, issuer, audience).await
    }

    async fn authenticate_renewal(
        &mut self,
        principal: &PrincipalId,
        issuer: &str,
        audience: &str,
        secret: &str,
    ) -> Result<Option<AuthenticatedPrincipal>, IdentityError> {
        password_login::authenticate_renewal(&mut self.0, principal, issuer, audience, secret).await
    }

    async fn rotate_login(
        &mut self,
        issuer: &str,
        audience: &str,
        secret: &str,
    ) -> Result<Option<Renewal>, IdentityError> {
        password_login::rotate_login(&mut self.0, issuer, audience, secret).await
    }

    async fn revoke_login(
        &mut self,
        issuer: &str,
        audience: &str,
        secret: &str,
    ) -> Result<(), IdentityError> {
        password_login::revoke_login(&mut self.0, issuer, audience, secret).await
    }

    async fn revoke_all(&mut self, principal: &PrincipalId) -> Result<(), IdentityError> {
        password_login::revoke_all(&mut self.0, principal).await
    }

    async fn sign(
        &mut self,
        claims: AccessTokenClaims,
        started_at: i64,
        expires_at: i64,
        envelope: &Envelope,
    ) -> Result<IssuedSessionToken, IdentityError> {
        session_token::sign_session_token_in_transaction(
            &mut self.0,
            claims,
            started_at,
            Some(expires_at),
            envelope,
        )
        .await
    }

    async fn commit(self: Box<Self>) -> Result<(), IdentityError> {
        self.0
            .commit()
            .await
            .map_err(|error| database_error(&error))
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

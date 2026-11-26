//! Persist NATS signing keys and revisions around the authentication signer.

use std::error;

use crate::nats::{self, Authority, Bootstrap, BootstrapKeys, Error, LimitsPerProject, User};
use nkeys::KeyPairType;
use sqlx::{Connection as _, PgConnection};
use wamn_envelope::encryption::Envelope;
use wamn_registry::{EnvName, OrgId};

use super::nats_store;

/// Create or load the platform's keys in one transaction under the issuer lock.
pub async fn bootstrap(
    client: &mut PgConnection,
    issuer: &str,
    envelope: &Envelope,
    max_projects: i64,
    limits_per_project: LimitsPerProject,
) -> Result<Bootstrap, Error> {
    limits_per_project.validate()?;
    let mut tx = client.begin().await.map_err(nats_store::database_error)?;
    nats_store::lock(&mut tx, issuer).await?;
    let root = nats_store::platform_key(
        &mut tx,
        envelope,
        issuer,
        "operator-root",
        KeyPairType::Operator,
    )
    .await?;
    let signing = nats_store::platform_key(
        &mut tx,
        envelope,
        issuer,
        "operator-signing",
        KeyPairType::Operator,
    )
    .await?;
    let system = nats_store::platform_key(
        &mut tx,
        envelope,
        issuer,
        "system-account",
        KeyPairType::Account,
    )
    .await?;
    let system_signing = nats_store::platform_key(
        &mut tx,
        envelope,
        issuer,
        "system-signing",
        KeyPairType::Account,
    )
    .await?;
    let platform = nats_store::platform_key(
        &mut tx,
        envelope,
        issuer,
        "platform-account",
        KeyPairType::Account,
    )
    .await?;
    let platform_signing = nats_store::platform_key(
        &mut tx,
        envelope,
        issuer,
        "platform-signing",
        KeyPairType::Account,
    )
    .await?;
    let runtime_operator = nats_store::platform_key(
        &mut tx,
        envelope,
        issuer,
        "runtime-operator",
        KeyPairType::User,
    )
    .await?;
    tx.commit().await.map_err(nats_store::database_error)?;

    nats::bootstrap(
        BootstrapKeys {
            root,
            signing,
            system,
            system_signing,
            platform,
            platform_signing,
            runtime_operator,
        },
        max_projects,
        limits_per_project,
    )
}

/// Persist and publish the environment's current account JWT under its session lock.
/// For an existing account, `lookup` returns the resolver's JWT or empty bytes.
/// The dedicated database connection is consumed. Publication errors leave the
/// committed revision available for a retry.
///
/// # Errors
///
/// Returns an error if the lock, key storage, signing, or resolver operation fails,
/// or if the account would exceed the installation's project limit.
pub async fn ensure_env<B, E>(
    authority: &Authority,
    client: PgConnection,
    envelope: &Envelope,
    (issuer, org, env): (&str, &OrgId, &EnvName),
    project_count: i32,
    lookup: impl AsyncFnOnce(&str) -> Result<B, E>,
    publish: impl AsyncFnOnce(&str, String) -> Result<(), E>,
) -> Result<String, Error>
where
    B: AsRef<[u8]>,
    E: error::Error + Send + Sync + 'static,
{
    let limits = authority.env_limits(project_count)?;
    nats_store::with_env_lock(client, issuer, (org, env), async |client| {
        let key = nats_store::ensure_env(
            client,
            envelope,
            issuer,
            (org, env),
            project_count,
            authority.max_projects(),
            async |subject, issued_at| {
                let jwt = lookup(subject).await.map_err(|source| {
                    Error::new("look up NATS environment account claim", source)
                })?;
                nats::account_revision_to_replace(jwt.as_ref(), issued_at, &limits)
            },
        )
        .await?;
        let (public_key, jwt) = authority.env_account(org, env, project_count, key, &limits)?;
        publish(&public_key, jwt)
            .await
            .map_err(|source| Error::new("publish NATS environment account", source))?;
        Ok(public_key)
    })
    .await
}

/// Advance the account revision and publish its removal under the same session lock.
/// The dedicated database connection is consumed.
///
/// # Errors
///
/// Returns an error if the lock, key storage, signing, or resolver operation fails.
pub async fn retire_env<E: error::Error + Send + Sync + 'static>(
    authority: &Authority,
    client: PgConnection,
    issuer: &str,
    org: &OrgId,
    env: &EnvName,
    publish: impl AsyncFnOnce(String) -> Result<(), E>,
) -> Result<Option<String>, Error> {
    nats_store::with_env_lock(client, issuer, (org, env), async |client| {
        let Some((account, issued_at)) = nats_store::retire_env(client, issuer, org, env).await?
        else {
            return Ok(None);
        };
        let jwt = authority.retired_account(&account, issued_at)?;
        publish(jwt)
            .await
            .map_err(|source| Error::new("retire NATS environment account", source))?;
        Ok(Some(account))
    })
    .await
}

/// Issue a scoped JWT in an existing environment account.
pub async fn env_user(
    client: &mut PgConnection,
    envelope: &Envelope,
    issuer: &str,
    org: &OrgId,
    env: &EnvName,
    user: &User<'_>,
) -> Result<Option<String>, Error> {
    nats_store::env_signer(client, envelope, issuer, org, env)
        .await?
        .map(|key| nats::sign_user(&key.signer, &key.subject, user))
        .transpose()
}

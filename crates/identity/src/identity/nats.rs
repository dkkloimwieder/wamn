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

/// Persist the environment key and issue its current account JWT.
/// For an existing account, `lookup` returns the resolver's JWT or empty bytes.
pub async fn ensure_env<B, E>(
    authority: &Authority,
    client: &mut PgConnection,
    envelope: &Envelope,
    issuer: &str,
    (org, env): (&OrgId, &EnvName),
    project_count: i32,
    lookup: impl AsyncFnOnce(&str) -> Result<B, E>,
) -> Result<(String, String), Error>
where
    B: AsRef<[u8]>,
    E: error::Error + Send + Sync + 'static,
{
    let limits = authority.env_limits(project_count)?;
    let key = nats_store::ensure_env(
        client,
        envelope,
        issuer,
        (org, env),
        project_count,
        authority.max_projects(),
        async |subject, issued_at| {
            let jwt = lookup(subject)
                .await
                .map_err(|source| Error::new("look up NATS environment account claim", source))?;
            nats::account_limits_match(jwt.as_ref(), issued_at, &limits)
        },
    )
    .await?;
    authority.env_account(org, env, project_count, key, &limits)
}

/// Advance the account revision and sign a full-resolver delete request.
pub async fn retire_env(
    authority: &Authority,
    client: &mut PgConnection,
    issuer: &str,
    org: &OrgId,
    env: &EnvName,
) -> Result<Option<(String, String)>, Error> {
    let Some((account, issued_at)) = nats_store::retire_env(client, issuer, org, env).await? else {
        return Ok(None);
    };
    let jwt = authority.retired_account(&account, issued_at)?;
    Ok(Some((account, jwt)))
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

//! Persist NATS signing keys and revisions around the authentication signer.

use std::error;

use crate::nats::{self, Authority, Bootstrap, BootstrapKeys, Error, User};
use nkeys::KeyPairType;
use sqlx::PgConnection;
use wamn_envelope::encryption::Envelope;
use wamn_identity_protocol::nats::{SignUser, UserPurpose};
use wamn_registry::{EnvName, OrgId};

use crate::workload_identity;

use super::nats_store;

/// Create or load the platform's immutable keys before signing its credentials.
pub async fn bootstrap(
    client: &mut PgConnection,
    issuer: &str,
    envelope: &Envelope,
    file_store_bytes: i64,
) -> Result<Bootstrap, Error> {
    let root = nats_store::platform_key(
        client,
        envelope,
        issuer,
        "operator-root",
        KeyPairType::Operator,
    )
    .await?;
    let signing = nats_store::platform_key(
        client,
        envelope,
        issuer,
        "operator-signing",
        KeyPairType::Operator,
    )
    .await?;
    let system = nats_store::platform_key(
        client,
        envelope,
        issuer,
        "system-account",
        KeyPairType::Account,
    )
    .await?;
    let system_signing = nats_store::platform_key(
        client,
        envelope,
        issuer,
        "system-signing",
        KeyPairType::Account,
    )
    .await?;
    let platform = nats_store::platform_key(
        client,
        envelope,
        issuer,
        "platform-account",
        KeyPairType::Account,
    )
    .await?;
    let platform_signing = nats_store::platform_key(
        client,
        envelope,
        issuer,
        "platform-signing",
        KeyPairType::Account,
    )
    .await?;
    let runtime_operator = nats_store::platform_key(
        client,
        envelope,
        issuer,
        "runtime-operator",
        KeyPairType::User,
    )
    .await?;
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
        file_store_bytes,
    )
}

/// List registered accounts with an unpublished revision or changed limits.
pub async fn accounts_to_refresh(
    client: &mut PgConnection,
    issuer: &str,
) -> Result<Vec<(String, String)>, Error> {
    nats_store::accounts_to_refresh(client, issuer).await
}

/// Persist and publish the environment account under its session lock.
///
/// A project count from the apply or teardown request sets stream allocations.
/// With no count, refresh the stored allocation if the account is still in use.
/// The dedicated database connection is consumed. The system-account request
/// returns NATS resolver and monitoring payloads.
///
/// # Errors
///
/// Returns an error on key, SQL or NATS failure, or if the limits conflict with
/// current use or the installation's file store capacity.
pub async fn ensure_env<B, E, F>(
    authority: &Authority,
    client: PgConnection,
    envelope: &Envelope,
    (issuer, org, env): (&str, &OrgId, &EnvName),
    project_count: Option<i32>,
    request: impl Fn(String, &'static [u8]) -> F + Sync,
    publish: impl AsyncFnOnce(&str, String) -> Result<(), E>,
) -> Result<Option<String>, Error>
where
    B: AsRef<[u8]>,
    F: Future<Output = Result<B, E>> + Send,
    E: error::Error + Send + Sync + 'static,
{
    nats_store::with_env_lock(client, issuer, (org, env), async |client| {
        nats_store::stage_revocations(client, issuer, org, env).await?;
        let Some((key, limits, revocations)) = nats_store::ensure_env(
            client,
            envelope,
            issuer,
            (org, env),
            project_count,
            authority.file_store_bytes(),
            async |subject, desired, committed| {
                let lookup = nats::LOOKUP_SUBJECT.replacen('*', subject, 1);
                let jwt = request(lookup, b"").await.map_err(|source| {
                    Error::new("look up NATS environment account claim", source)
                })?;
                let observed = nats::account_revision(jwt.as_ref())?;
                let current = observed.as_ref().map(nats::AccountRevision::limits);
                if let Some(current) = current.or(committed)
                    && current.exceeds(desired)
                {
                    nats::check_shrink(subject, current, desired, &request).await?;
                }
                Ok(observed)
            },
        )
        .await?
        else {
            return Ok(None);
        };
        let (public_key, jwt) = authority.env_account(org, env, key, limits, &revocations)?;
        publish(&public_key, jwt)
            .await
            .map_err(|source| Error::new("publish NATS environment account", source))?;
        nats_store::acknowledge(client, issuer, (org, env), limits.disk_storage).await?;
        Ok(Some(public_key))
    })
    .await
}

/// Authorize the workload against its current mapping and sign under the account lock.
/// A project key is recorded before the JWT leaves identity so deactivation can revoke it.
pub async fn env_user(
    client: PgConnection,
    envelope: &Envelope,
    issuer: &str,
    (namespace, account): (&str, &str),
    request: &SignUser,
    user: &User<'_>,
) -> Result<Option<String>, Error> {
    let (org, env) = (&request.org, &request.env);
    nats_store::with_env_lock(client, issuer, (org, env), async |client| {
        let Some(key) = nats_store::env_signer(client, envelope, issuer, org, env).await? else {
            return Ok(None);
        };
        let Some(mapping) = workload_identity::mapped_env(client, namespace)
            .await
            .map_err(nats_store::database_error)?
        else {
            return Err(Error::scope_refused());
        };
        let project = workload_identity::project(&request.purpose);
        let project_active = if let Some(project) = project {
            workload_identity::project_active(client, &mapping, project)
                .await
                .map_err(nats_store::database_error)?
        } else {
            false
        };
        if !workload_identity::user_allowed(&mapping, account, request, project_active) {
            return Err(Error::scope_refused());
        }
        let issued_at = nats::unix_seconds()?;
        let job_user = matches!(
            request.purpose,
            UserPurpose::StreamAdmin | UserPurpose::Recovery { .. }
        );
        if job_user != user.expires_at.is_some()
            || user.expires_at.is_some_and(|expiry| expiry <= issued_at)
        {
            return Err(Error::scope_refused());
        }
        let jwt = nats::sign_user(&key.signer, &key.subject, user, issued_at)?;
        if let Some(project) = project
            && !nats_store::record_user(
                client,
                issuer,
                (org, env, project),
                user.public_key,
                issued_at,
            )
            .await?
        {
            return Err(Error::scope_refused());
        }
        Ok(Some(jwt))
    })
    .await
}

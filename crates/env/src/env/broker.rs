//! Environment-owned NATS credentials and the environment's resolver account.

use anyhow::Context as _;
use wamn_event::deploy_broker::CREDENTIALS_KEY;
use wamn_identity_protocol::nats::{
    BrokerIdentity, EnsureAccount, SignUser, UserPurpose, UserRefusal,
};

use wamn_registry::{EnvName, OrgId, Triple, name};
use zeroize::Zeroizing;

use super::Platform;

fn env_users() -> [(&'static str, UserPurpose); 4] {
    [
        ("data", UserPurpose::HostData),
        ("workflow", UserPurpose::Workflow),
        ("ingress", UserPurpose::Ingress),
        ("workflow-native", UserPurpose::WorkflowNative),
    ]
}

pub(super) async fn configure(
    platform: &Platform,
    org: &OrgId,
    env: &EnvName,
    count: u32,
) -> anyhow::Result<()> {
    let identity = (platform.broker_identity)(&platform.identity_issuer)?;
    set_limit(&*identity, org, env, count).await?;
    for (purpose_name, user) in env_users() {
        ensure_user(
            platform,
            &*identity,
            org,
            env,
            &name::env_nats_secret(org, env, purpose_name),
            user,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn configure_project(
    platform: &Platform,
    triple: &Triple,
) -> anyhow::Result<String> {
    let identity = (platform.broker_identity)(&platform.identity_issuer)?;
    let secret = name::project_nats_secret(triple, "cdc");
    let request = |public_key| SignUser {
        org: triple.org.clone(),
        env: triple.env.clone(),
        purpose: UserPurpose::Cdc {
            project: triple.project.clone(),
        },
        public_key,
    };
    if let Some(creds) = read_user(platform, &secret).await? {
        let inbox = platform.broker.credentials_inbox(&creds)?;
        let (_, public_key) = inbox
            .rsplit_once('.')
            .context("CDC credentials have no NATS user key")?;
        let existing = request(public_key.to_owned());
        match identity.sign_user(&existing).await {
            Ok(_) => return Ok(existing.public_key),
            Err(error) if error.downcast_ref::<UserRefusal>() == Some(&UserRefusal::Scope) => {
                // A project may be active again while its previous key stays revoked.
            }
            Err(error) => return Err(error),
        }
    }
    let key = nkeys::KeyPair::new_user();
    let request = request(key.public_key());
    let signed = identity.sign_user(&request).await?;
    let creds = credentials(platform, &key, &signed)?;
    platform
        .cluster
        .write_credentials(&secret, CREDENTIALS_KEY, creds.as_bytes())
        .await?;
    Ok(request.public_key)
}

pub(super) async fn delete_project_user(
    platform: &Platform,
    triple: &Triple,
) -> anyhow::Result<()> {
    delete_secret(platform, &name::project_nats_secret(triple, "cdc")).await
}

pub(super) async fn after_delete(
    platform: &Platform,
    org: &OrgId,
    env: &EnvName,
    remaining: u32,
) -> anyhow::Result<()> {
    let identity = (platform.broker_identity)(&platform.identity_issuer)?;
    if remaining == 0 {
        for (purpose_name, _) in env_users() {
            delete_secret(platform, &name::env_nats_secret(org, env, purpose_name)).await?;
        }
    }
    set_limit(&*identity, org, env, remaining).await
}

async fn set_limit(
    identity: &dyn BrokerIdentity,
    org: &OrgId,
    env: &EnvName,
    count: u32,
) -> anyhow::Result<()> {
    identity
        .ensure_account(&EnsureAccount {
            org: org.clone(),
            env: env.clone(),
            project_count: count,
        })
        .await?;
    Ok(())
}

async fn ensure_user(
    platform: &Platform,
    identity: &dyn BrokerIdentity,
    org: &OrgId,
    env: &EnvName,
    secret_name: &str,
    purpose: UserPurpose,
) -> anyhow::Result<()> {
    if read_user(platform, secret_name).await?.is_some() {
        return Ok(());
    }
    let key = nkeys::KeyPair::new_user();
    let signed = identity
        .sign_user(&SignUser {
            org: org.clone(),
            env: env.clone(),
            purpose,
            public_key: key.public_key(),
        })
        .await?;
    let creds = credentials(platform, &key, &signed)?;
    platform
        .cluster
        .write_credentials(secret_name, CREDENTIALS_KEY, creds.as_bytes())
        .await
}

fn credentials(
    platform: &Platform,
    key: &nkeys::KeyPair,
    signed: &wamn_identity_protocol::nats::SignedUser,
) -> anyhow::Result<Zeroizing<String>> {
    let creds = wamn_identity::nats::credentials(&signed.jwt, key)?;
    anyhow::ensure!(
        platform.broker.credentials_inbox(&creds)? == signed.inbox_prefix,
        "identity returned an unexpected NATS inbox"
    );
    Ok(creds)
}

async fn read_user(platform: &Platform, name: &str) -> anyhow::Result<Option<Zeroizing<String>>> {
    let Some(bytes) = platform
        .cluster
        .read_credentials(name, CREDENTIALS_KEY)
        .await?
    else {
        return Ok(None);
    };
    let bytes = Zeroizing::new(bytes);
    let credentials = std::str::from_utf8(&bytes).context("broker credentials are invalid")?;
    Ok(Some(Zeroizing::new(credentials.to_owned())))
}

async fn delete_secret(platform: &Platform, name: &str) -> anyhow::Result<()> {
    platform.cluster.delete_secret(name).await.map(drop)
}

pub(super) async fn inbox(
    platform: &Platform,
    org: &OrgId,
    env: &EnvName,
    purpose_name: &str,
) -> anyhow::Result<String> {
    let name = name::env_nats_secret(org, env, purpose_name);
    let creds = read_user(platform, &name)
        .await?
        .context("host NATS credential is absent")?;
    platform.broker.credentials_inbox(&creds)
}

/// Obtain short-lived credentials bound to this Job's key and projected pod token.
pub(super) async fn job_user(
    platform: &Platform,
    org: &OrgId,
    env: &EnvName,
    purpose: UserPurpose,
) -> anyhow::Result<Zeroizing<String>> {
    let identity = (platform.broker_identity)(&platform.identity_issuer)?;
    let key = nkeys::KeyPair::new_user();
    let signed = identity
        .sign_user(&SignUser {
            org: org.clone(),
            env: env.clone(),
            purpose,
            public_key: key.public_key(),
        })
        .await?;
    credentials(platform, &key, &signed)
}

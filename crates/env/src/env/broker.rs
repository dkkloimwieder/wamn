//! Org-owned NATS credentials and the environment's resolver account.

use anyhow::Context as _;
use wamn_event::deploy_broker::CREDENTIALS_KEY;
use wamn_identity_protocol::nats::{BrokerIdentity, EnsureAccount, SignUser, UserPurpose};

use wamn_registry::{EnvName, OrgId, Triple, name};
use zeroize::Zeroizing;

use super::Platform;

fn env_users() -> [(&'static str, UserPurpose); 5] {
    [
        ("data", UserPurpose::HostData),
        ("workflow", UserPurpose::Workflow),
        ("ingress", UserPurpose::Ingress),
        ("workflow-native", UserPurpose::WorkflowNative),
        ("stream-admin", UserPurpose::StreamAdmin),
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

pub(super) async fn configure_project(platform: &Platform, triple: &Triple) -> anyhow::Result<()> {
    let identity = (platform.broker_identity)(&platform.identity_issuer)?;
    for (purpose_name, user) in [
        (
            "cdc",
            UserPurpose::Cdc {
                project: triple.project.clone(),
            },
        ),
        (
            "recovery",
            UserPurpose::Recovery {
                project: triple.project.clone(),
            },
        ),
    ] {
        ensure_user(
            platform,
            &*identity,
            &triple.org,
            &triple.env,
            &name::project_nats_secret(triple, purpose_name),
            user,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn after_delete(
    platform: &Platform,
    triple: &Triple,
    remaining: u32,
) -> anyhow::Result<()> {
    let identity = (platform.broker_identity)(&platform.identity_issuer)?;
    for purpose_name in ["cdc", "recovery"] {
        delete_secret(platform, &name::project_nats_secret(triple, purpose_name)).await?;
    }
    if remaining == 0 {
        for (purpose_name, _) in env_users() {
            delete_secret(
                platform,
                &name::env_nats_secret(&triple.org, &triple.env, purpose_name),
            )
            .await?;
        }
    }
    set_limit(&*identity, &triple.org, &triple.env, remaining).await
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
    let creds = wamn_identity::nats::credentials(&signed.jwt, &key)?;
    anyhow::ensure!(
        platform.broker.credentials_inbox(&creds)? == signed.inbox_prefix,
        "identity returned an unexpected NATS inbox"
    );
    platform
        .cluster
        .write_credentials(secret_name, CREDENTIALS_KEY, creds.as_bytes())
        .await
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

pub(super) async fn admin(
    platform: &Platform,
    org: &OrgId,
    env: &EnvName,
) -> anyhow::Result<Option<Zeroizing<String>>> {
    let name = name::env_nats_secret(org, env, "stream-admin");
    read_user(platform, &name).await
}

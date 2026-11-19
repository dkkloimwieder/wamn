//! Org-owned NATS credentials and the environment's resolver account.

use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;
use wamn_broker::nats::{CA_FILE, CREDENTIALS_KEY};
use wamn_identity_client::nats::{EnsureAccount, NatsClient, SignUser, UserKind};
use wamn_kubernetes::{kubectl, secret::Secret};
use wamn_registry::{EnvName, OrgId, Triple, name};
use zeroize::Zeroizing;

use super::Platform;

const IDENTITY_CA: &str = "/etc/identity-ca/ca.crt";

fn client(platform: &Platform) -> anyhow::Result<NatsClient> {
    NatsClient::new(&platform.identity_issuer, std::path::Path::new(IDENTITY_CA))
}

fn environment_users() -> [(&'static str, UserKind); 6] {
    [
        ("scheduler", UserKind::HostScheduler),
        ("data", UserKind::HostData),
        ("workflow", UserKind::Workflow),
        ("ingress", UserKind::Ingress),
        ("workflow-native", UserKind::WorkflowNative),
        ("stream-admin", UserKind::StreamAdmin),
    ]
}

pub(super) async fn project_count(
    db: &mut sqlx::PgConnection,
    org: &OrgId,
    env: &EnvName,
) -> anyhow::Result<u32> {
    let mut tx = crate::provision_project_env::provisioning_transaction(db).await?;
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM registry.environment_projects WHERE org=$1 AND env=$2",
    )
    .bind(org)
    .bind(env)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(u32::try_from(count)?)
}

pub(super) async fn configure(
    platform: &Platform,
    org: &OrgId,
    env: &EnvName,
    count: u32,
) -> anyhow::Result<()> {
    let identity = client(platform)?;
    set_limit(&identity, org, env, count).await?;
    for (kind, user) in environment_users() {
        ensure_user(
            platform,
            &identity,
            org,
            env,
            &name::environment_nats_secret(org, env, kind),
            user,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn configure_project(platform: &Platform, triple: &Triple) -> anyhow::Result<()> {
    let identity = client(platform)?;
    for (kind, user) in [
        (
            "cdc",
            UserKind::Cdc {
                project: triple.project.clone(),
            },
        ),
        (
            "recovery",
            UserKind::Recovery {
                project: triple.project.clone(),
            },
        ),
    ] {
        ensure_user(
            platform,
            &identity,
            &triple.org,
            &triple.env,
            &name::project_nats_secret(triple, kind),
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
    let identity = client(platform)?;
    for kind in ["cdc", "recovery"] {
        delete_secret(platform, &name::project_nats_secret(triple, kind)).await?;
    }
    if remaining == 0 {
        for (kind, _) in environment_users() {
            delete_secret(
                platform,
                &name::environment_nats_secret(&triple.org, &triple.env, kind),
            )
            .await?;
        }
    }
    set_limit(&identity, &triple.org, &triple.env, remaining).await
}

async fn set_limit(
    identity: &NatsClient,
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
    identity: &NatsClient,
    org: &OrgId,
    env: &EnvName,
    secret_name: &str,
    kind: UserKind,
) -> anyhow::Result<()> {
    if read_user(platform, secret_name).await?.is_some() {
        return Ok(());
    }
    let key = nkeys::KeyPair::new_user();
    let signed = identity
        .sign_user(&SignUser {
            org: org.clone(),
            env: env.clone(),
            kind,
            public_key: key.public_key(),
        })
        .await?;
    let creds = wamn_identity::nats::credentials(&signed.jwt, &key)?;
    anyhow::ensure!(
        wamn_broker::nats::credentials_inbox(&creds)? == signed.inbox_prefix,
        "identity returned an unexpected NATS inbox"
    );
    let mut secret = Secret::new(&platform.target.namespace, secret_name);
    secret
        .data
        .insert(CREDENTIALS_KEY.into(), STANDARD.encode(creds.as_bytes()));
    kubectl::apply(
        &platform.target.kubeconfig,
        &platform.target.context,
        &platform.target.namespace,
        &secret,
    )
    .await
}

async fn read_user(platform: &Platform, name: &str) -> anyhow::Result<Option<Zeroizing<String>>> {
    let Some(secret) = kubectl::get::<Secret>(
        &platform.target.kubeconfig,
        &platform.target.context,
        &platform.target.namespace,
        "secret",
        name,
    )
    .await?
    else {
        return Ok(None);
    };
    let bytes = Zeroizing::new(
        STANDARD.decode(
            secret
                .data
                .get(CREDENTIALS_KEY)
                .context("broker Secret is missing its credentials")?,
        )?,
    );
    let creds = std::str::from_utf8(&bytes).context("broker credentials are invalid")?;
    Ok(Some(Zeroizing::new(creds.to_owned())))
}

async fn delete_secret(platform: &Platform, name: &str) -> anyhow::Result<()> {
    super::ensure::kubectl(
        platform,
        &platform.target.namespace,
        &["delete", "secret", name, "--ignore-not-found"],
        None,
    )
    .await
    .map(drop)
}

pub(super) async fn binding(
    platform: &Platform,
    org: &OrgId,
    env: &EnvName,
    kind: &str,
) -> anyhow::Result<serde_json::Value> {
    let name = name::environment_nats_secret(org, env, kind);
    let creds = read_user(platform, &name)
        .await?
        .context("host NATS credential is absent")?;
    let inbox = wamn_broker::nats::credentials_inbox(&creds)?;
    Ok(json!({"secret": name, "inbox": inbox}))
}

pub(super) async fn admin(
    platform: &Platform,
    org: &OrgId,
    env: &EnvName,
) -> anyhow::Result<Option<async_nats::jetstream::Context>> {
    let name = name::environment_nats_secret(org, env, "stream-admin");
    let Some(creds) = read_user(platform, &name).await? else {
        return Ok(None);
    };
    let options = wamn_broker::nats::user_options(async_nats::ConnectOptions::new(), &creds)?;
    let options = wamn_broker::nats::tls_options(options, Some(std::path::Path::new(CA_FILE)));
    let client = wamn_broker::nats::connect(
        options,
        &platform.nats_url,
        wamn_broker::nats::InitialConnect::Once,
    )
    .await?;
    Ok(Some(async_nats::jetstream::new(client)))
}

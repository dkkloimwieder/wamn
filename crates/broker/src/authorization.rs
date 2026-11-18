//! NATS accounts and client certificates for the shared broker.

use std::{collections::BTreeMap, time::Duration};

use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_kubernetes::{Target, kubectl, secret::Secret};
use wamn_registry::{Triple, name};

const NAMESPACE: &str = "platform";
const AUTHORIZATION: &str = "wamn-nats-authorization";
const FILE: &str = "authorization.conf";
const ACCOUNTS_PREFIX: &str = "accounts: ";
const PLATFORM: &str = "PLATFORM";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Account {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    jetstream: Option<JetStreamLimits>,
    users: Vec<User>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JetStreamLimits {
    max_mem: u64,
    max_file: u64,
    max_streams: i64,
    max_consumers: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct User {
    user: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    permissions: Option<Permissions>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Permissions {
    publish: Subjects,
    subscribe: Subjects,
}

#[derive(Debug, Serialize, Deserialize)]
struct Subjects {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    deny: Vec<String>,
}

fn scoped_user(user: &str, publish: &[&str], subscribe: &[&str]) -> User {
    let subjects = |allow: &[&str]| Subjects {
        allow: allow.iter().map(|subject| (*subject).into()).collect(),
        deny: if allow.is_empty() { vec![">".into()] } else { Vec::new() },
    };
    User {
        user: user.into(),
        permissions: Some(Permissions {
            publish: subjects(publish),
            subscribe: subjects(subscribe),
        }),
    }
}

fn platform_users() -> Vec<User> {
    let control = ["runtime.host.>", "runtime.operator.>", "_INBOX.>"];
    vec![
        scoped_user("wasmcloud-operator", &control, &control),
        scoped_user("wasmcloud-runtime", &control, &control),
        scoped_user("wasmcloud-data", &[], &[]),
    ]
}

type Accounts = BTreeMap<String, Account>;

/// Seed the shared broker Secret before Helm. Preserve the installed environment accounts.
pub async fn bootstrap(target: &Target) -> anyhow::Result<()> {
    if authorization_secret(target).await?.is_none() {
        update(target, |_| {}).await?;
    }
    Ok(())
}

/// Grant the environment host full rights and restrict the reader to its own project.
pub async fn provision_project(target: &Target, coordinate: &Triple) -> anyhow::Result<()> {
    update(target, |accounts| {
        let (org, project, env) = (&coordinate.org, &coordinate.project, &coordinate.env);
        let account = accounts.entry(name::environment_nats_account(org, env)).or_insert_with(|| {
            Account {
                jetstream: Some(JetStreamLimits {
                    max_mem: 0,
                    max_file: 1024 * 1024 * 1024,
                    max_streams: -1,
                    max_consumers: -1,
                }),
                users: vec![User { user: name::environment_nats_user(org, env), permissions: None }],
            }
        });
        let user = name::cdc_nats_user(coordinate);
        let events = name::stream_subjects(org, project, env);
        let stream = name::stream_name(org, project, env);
        let info = format!("$JS.API.STREAM.INFO.{stream}");
        account.users.retain(|entry| entry.user != user);
        account.users.push(scoped_user(&user, &[&events, &info], &["_INBOX.>"]));
        account.users.sort_unstable_by(|left, right| left.user.cmp(&right.user));
    })
    .await
}

/// Remove the reader user. Return whether the shared credential can be retired.
pub async fn remove_project(target: &Target, coordinate: &Triple) -> anyhow::Result<bool> {
    let account = name::environment_nats_account(&coordinate.org, &coordinate.env);
    let reader = name::cdc_nats_user(coordinate);
    let mut last = false;
    update(target, |accounts| {
        last = accounts.get_mut(&account).is_none_or(|entry| {
            entry.users.retain(|user| user.user != reader);
            entry.users.len() == 1
        });
        if last {
            accounts.remove(&account);
        }
    })
    .await?;
    Ok(last)
}

async fn update(target: &Target, mut edit: impl FnMut(&mut Accounts)) -> anyhow::Result<()> {
    wamn_kubernetes::secret::update(
        &target.kubeconfig,
        &target.context,
        NAMESPACE,
        AUTHORIZATION,
        |secret| {
            let mut accounts = if secret.metadata.resource_version.is_some() {
                read_accounts(secret)?
            } else {
                let mut accounts = Accounts::new();
                accounts.insert(PLATFORM.into(), Account { users: platform_users(), jetstream: None });
                accounts
            };
            edit(&mut accounts);
            let document = serde_json::to_string(&accounts).context("encode broker accounts")?;
            secret.data.insert(FILE.into(), STANDARD.encode(format!("{ACCOUNTS_PREFIX}{document}\n")));
            Ok(())
        },
    )
    .await
}

async fn authorization_secret(target: &Target) -> anyhow::Result<Option<Secret>> {
    kubectl::get(&target.kubeconfig, &target.context, NAMESPACE, "secret", AUTHORIZATION).await
}

fn read_accounts(secret: &Secret) -> anyhow::Result<Accounts> {
    let data = secret.data.get(FILE).context("broker Secret has no authorization.conf")?;
    let bytes = STANDARD.decode(data).context("decode broker authorization data")?;
    let text = std::str::from_utf8(&bytes).context("broker authorization is not UTF-8")?;
    let payload = text.strip_prefix(ACCOUNTS_PREFIX).context("broker Secret has no accounts document")?;
    serde_json::from_str(payload).context("decode broker accounts")
}

/// Check whether a project can still have a source stream. On an interrupted delete,
/// removing the account can succeed before cleanup removes the certificate Secret.
pub async fn environment_account_exists(target: &Target, coordinate: &Triple) -> anyhow::Result<bool> {
    let Some(secret) = authorization_secret(target).await? else {
        return Ok(false);
    };
    Ok(read_accounts(&secret)?.contains_key(
        &name::environment_nats_account(&coordinate.org, &coordinate.env),
    ))
}

/// Parameters of the Helm-owned client Certificate mapped into its account by DNS SAN.
pub fn certificate(name: &str, secret: &str, user: &str) -> Value {
    json!({
        "name": name,
        "spec": {
            "secretName": secret,
            "commonName": user,
            "dnsNames": [user],
            "usages": ["client auth", "digital signature", "key encipherment"],
            "issuerRef": {"kind": "ClusterIssuer", "name": "wasmcloud-ca", "group": "cert-manager.io"}
        }
    })
}

/// Open the account from the issued Secret. Allow the server up to 60 seconds to reload it.
pub async fn environment_context(
    target: &Target,
    coordinate: &Triple,
    server: &str,
) -> anyhow::Result<Option<async_nats::jetstream::Context>> {
    let secret_name = name::environment_nats_secret(&coordinate.org, &coordinate.env);
    let Some(secret) = kubectl::get::<Secret>(
        &target.kubeconfig, &target.context, coordinate.org.as_str(), "secret", &secret_name,
    ).await? else {
        return Ok(None);
    };
    let directory = tempfile::tempdir().context("create directory for the environment broker credential")?;
    for key in ["ca.crt", "tls.crt", "tls.key"] {
        let value = secret.data.get(key).with_context(|| format!("Secret {secret_name} lacks {key}"))?;
        std::fs::write(directory.path().join(key), STANDARD.decode(value)?)?;
    }
    let connection = crate::nats::PlatformNats {
        nats_url: server.into(),
        nats_tls_ca: directory.path().join("ca.crt"),
        nats_tls_cert: directory.path().join("tls.crt"),
        nats_tls_key: directory.path().join("tls.key"),
        nats_tls_server_name: None,
    };
    let broker = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let result = async {
                let broker = async_nats::jetstream::new(connection.connect().await?);
                broker.query_account().await.context("read JetStream account info")?;
                Ok::<_, anyhow::Error>(broker)
            }.await;
            match result {
                Ok(broker) => return broker,
                Err(error) => tracing::debug!(%error, "waiting for the environment broker account"),
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }).await.with_context(|| format!("wait for the JetStream account of {coordinate}"))?;
    Ok(Some(broker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_certificate_users_keep_scheduler_permissions_targeted() {
        let users = platform_users();
        for name in ["wasmcloud-operator", "wasmcloud-runtime"] {
            let user = users.iter().find(|user| user.user == name).unwrap();
            let permissions = user.permissions.as_ref().unwrap();
            for subjects in [&permissions.publish, &permissions.subscribe] {
                assert_eq!(
                    subjects.allow,
                    ["runtime.host.>", "runtime.operator.>", "_INBOX.>"]
                );
            }
        }
    }
}

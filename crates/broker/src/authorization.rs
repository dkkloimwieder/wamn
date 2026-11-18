//! NATS accounts and client certificates for the shared broker.

use std::{collections::BTreeMap, path::Path, time::Duration};

use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_kubernetes::{Target, kubectl, secret::Secret};
use wamn_registry::{EnvName, OrgId, Triple, name};

use crate::events::SOURCE_STREAM_MAX_BYTES;

const NAMESPACE: &str = "platform";
const FILE: &str = "accounts.conf";

/// The mount for the broker administration credential in the delete Job.
pub const ADMIN_CREDENTIAL_DIR: &str = "/etc/wamn/nats-admin";

/// Host-owned NATS binding for the workflow service.
pub const WORKFLOW_BINDING: &str = "workflow";

/// Host-owned NATS binding for the ingress component.
pub const INGRESS_BINDING: &str = "ingress";

#[derive(Debug, Serialize, Deserialize)]
struct Account {
    jetstream: JetStreamLimits,
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
    allow: Vec<String>,
}

fn scoped_user(user: &str, publish: &[&str], subscribe: &[&str]) -> User {
    let subjects = |allow: &[&str]| Subjects {
        allow: allow.iter().map(|subject| (*subject).into()).collect(),
    };
    User {
        user: user.into(),
        permissions: Some(Permissions {
            publish: subjects(publish),
            subscribe: subjects(subscribe),
        }),
    }
}

fn workload_user(org: &OrgId, env: &EnvName) -> User {
    let user = name::environment_nats_user(org, env);
    scoped_user(
        &user,
        &[
            "$JS.API.STREAM.INFO.*",
            "$JS.API.CONSUMER.INFO.>",
            "$JS.API.CONSUMER.CREATE.>",
            "$JS.API.CONSUMER.DELETE.>",
            "$JS.API.CONSUMER.MSG.NEXT.>",
            "$JS.ACK.>",
        ],
        &[&inbox_subject(&user), "_nats_push.>"],
    )
}

fn inbox_subject(user: &str) -> String {
    format!("{}.>", crate::nats::inbox_prefix(user))
}

type Accounts = BTreeMap<String, Account>;

/// Refresh the file limit for a retained account and return its project count.
fn update_file_limit(account: &mut Account) -> usize {
    let project_count = account
        .users
        .iter()
        .filter(|user| user.user.starts_with("wamn-cdc-"))
        .count();
    if project_count > 0 {
        account.jetstream.max_file = u64::from(SOURCE_STREAM_MAX_BYTES) * project_count as u64;
    }
    project_count
}

/// Provision the environment's host and admin users and the project's scoped users.
pub async fn provision_project(target: &Target, coordinate: &Triple) -> anyhow::Result<()> {
    let (org, project, env) = (&coordinate.org, &coordinate.project, &coordinate.env);
    update(target, org, |accounts| {
        let account = accounts
            .entry(name::environment_nats_account(org, env))
            .or_insert_with(|| Account {
                jetstream: JetStreamLimits {
                    max_mem: 0,
                    max_file: 0,
                    max_streams: -1,
                    max_consumers: -1,
                },
                users: vec![
                    workload_user(org, env),
                    User {
                        user: name::environment_nats_admin(org, env),
                        permissions: None,
                    },
                ],
            });
        let user = name::cdc_nats_user(org, project, env);
        let events = name::stream_subjects(org, project, env);
        let stream = name::stream_name(org, project, env);
        let info = format!("$JS.API.STREAM.INFO.{stream}");
        let recovery = name::recovery_nats_user(coordinate);
        let message = format!("$JS.API.STREAM.MSG.GET.{stream}");
        let inbox = inbox_subject(&recovery);
        account
            .users
            .retain(|entry| entry.user != user && entry.user != recovery);
        account.users.push(scoped_user(
            &user,
            &[&events, &info],
            &[&inbox_subject(&user)],
        ));
        account
            .users
            .push(scoped_user(&recovery, &[&info, &message], &[&inbox]));
        update_file_limit(account);
        account
            .users
            .sort_unstable_by(|left, right| left.user.cmp(&right.user));
    })
    .await
}

/// Remove the reader user. Return whether the shared credential can be retired.
pub async fn remove_project(target: &Target, coordinate: &Triple) -> anyhow::Result<bool> {
    let account = name::environment_nats_account(&coordinate.org, &coordinate.env);
    let reader = name::cdc_nats_user(&coordinate.org, &coordinate.project, &coordinate.env);
    let recovery = name::recovery_nats_user(coordinate);
    let mut last = false;
    update(target, &coordinate.org, |accounts| {
        last = accounts.get_mut(&account).is_none_or(|entry| {
            entry
                .users
                .retain(|user| user.user != reader && user.user != recovery);
            update_file_limit(entry) == 0
        });
        if last {
            accounts.remove(&account);
        }
    })
    .await?;
    Ok(last)
}

async fn update(
    target: &Target,
    org: &OrgId,
    mut edit: impl FnMut(&mut Accounts),
) -> anyhow::Result<()> {
    let name = name::org_nats_accounts_secret(org);
    wamn_kubernetes::secret::update(
        &target.kubeconfig,
        &target.context,
        NAMESPACE,
        &name,
        |secret| {
            let mut accounts = read_accounts(secret)?;
            edit(&mut accounts);
            require_org_accounts(org, &accounts)?;
            let document = serde_json::to_vec(&accounts).context("encode broker accounts")?;
            secret.data.insert(FILE.into(), STANDARD.encode(document));
            Ok(())
        },
    )
    .await
}

fn read_accounts(secret: &Secret) -> anyhow::Result<Accounts> {
    let data = secret
        .data
        .get(FILE)
        .context("broker Secret has no accounts.conf")?;
    let bytes = STANDARD
        .decode(data)
        .context("decode broker accounts data")?;
    if bytes.is_empty() {
        return Ok(Accounts::new());
    }
    serde_json::from_slice(&bytes).context("decode broker accounts")
}

#[derive(Debug)]
struct ForeignAccount {
    org: OrgId,
    account: String,
}

impl std::fmt::Display for ForeignAccount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "invalid broker account {:?} for org {}",
            self.account, self.org
        )
    }
}

impl std::error::Error for ForeignAccount {}

fn require_org_accounts(org: &OrgId, accounts: &Accounts) -> Result<(), ForeignAccount> {
    let prefix = format!("ENV_{org}_");
    for account in accounts.keys() {
        if account
            .strip_prefix(&prefix)
            .is_none_or(|env| env.parse::<EnvName>().is_err())
        {
            return Err(ForeignAccount {
                org: org.clone(),
                account: account.clone(),
            });
        }
    }
    Ok(())
}

/// Check whether a project can still have a source stream. On an interrupted delete,
/// removing the account can succeed before cleanup removes the certificate Secret.
pub async fn environment_account_exists(
    target: &Target,
    coordinate: &Triple,
) -> anyhow::Result<bool> {
    let org = &coordinate.org;
    let Some(secret) = kubectl::get::<Secret>(
        &target.kubeconfig,
        &target.context,
        NAMESPACE,
        "secret",
        &name::org_nats_accounts_secret(org),
    )
    .await?
    else {
        return Ok(false);
    };
    let accounts = read_accounts(&secret)?;
    require_org_accounts(org, &accounts)?;
    Ok(accounts.contains_key(&name::environment_nats_account(org, &coordinate.env)))
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

/// Open the account from the issued admin Secret after its Certificate is ready.
pub async fn issued_admin_context(
    target: &Target,
    coordinate: &Triple,
    server: &str,
) -> anyhow::Result<async_nats::jetstream::Context> {
    let secret_name = name::environment_nats_admin(&coordinate.org, &coordinate.env);
    let secret = kubectl::get::<Secret>(
        &target.kubeconfig,
        &target.context,
        coordinate.org.as_str(),
        "secret",
        &secret_name,
    )
    .await?
    .with_context(|| format!("environment broker Secret {secret_name} is absent"))?;
    let directory =
        tempfile::tempdir().context("create directory for the environment broker credential")?;
    for key in ["ca.crt", "tls.crt", "tls.key"] {
        let value = secret.data.get(key).with_context(|| format!("Secret {secret_name} lacks {key}"))?;
        std::fs::write(directory.path().join(key), STANDARD.decode(value)?)?;
    }
    admin_context(coordinate, server, directory.path()).await
}

/// Open the account from admin credential files. Give NATS up to 60 seconds to reload.
pub async fn admin_context(
    coordinate: &Triple,
    server: &str,
    directory: &Path,
) -> anyhow::Result<async_nats::jetstream::Context> {
    let connection = crate::nats::PlatformNats {
        nats_url: server.into(),
        nats_tls_ca: directory.join("ca.crt"),
        nats_tls_cert: directory.join("tls.crt"),
        nats_tls_key: directory.join("tls.key"),
        nats_tls_server_name: None,
    };
    tokio::time::timeout(Duration::from_secs(60), async {
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
    })
    .await
    .with_context(|| format!("wait for the JetStream account of {coordinate}"))
}

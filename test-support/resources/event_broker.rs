//! Private broker credentials for one application's cluster tests.

use std::collections::BTreeSet;
use std::fs::{DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use async_nats::jetstream::{consumer::pull, stream};
use serde::Serialize;
use wamn_registry::Triple;

use crate::nats_tls::{Authority, ClientCertificate, DEVELOPMENT_USER};

/// A user and its client certificate (docs/plan/alignment.md R4).
pub type Credentials = ClientCertificate;

/// The broker files and separate credentials for one test environment.
#[derive(Debug)]
pub struct EventBroker {
    pub configuration: PathBuf,
    pub provisioning: Credentials,
    pub runtime: Credentials,
    pub publisher: Credentials,
    pub observer: Credentials,
    /// The full-permission development certificate of `wamn dev` and the
    /// processes a test starts.
    pub development: Credentials,
}

#[derive(Serialize)]
struct BrokerConfiguration {
    http: &'static str,
    tls: serde_json::Value,
    authorization: Authorization,
}

#[derive(Serialize)]
struct Authorization {
    users: Vec<Account>,
}

#[derive(Serialize)]
struct Account {
    user: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    permissions: Option<Permissions>,
}

#[derive(Serialize)]
struct Permissions {
    publish: Subjects,
    subscribe: Subjects,
}

#[derive(Serialize)]
struct Subjects {
    allow: Vec<String>,
}

/// Write the broker configuration before the caller starts its owned container.
/// The broker has its own CA.
pub fn prepare(
    work: &Path,
    target: &Triple,
    project_env_key: &wamn_registry::ProjectEnvKey,
    source: &stream::Config,
    advisory: &stream::Config,
    consumers: &[pull::Config],
) -> anyhow::Result<EventBroker> {
    prepare_with(
        None,
        work,
        target,
        project_env_key,
        source,
        advisory,
        consumers,
    )
}

/// [`prepare`] with the CA of `authority`, for brokers of several
/// environments on one server.
pub fn prepare_in(
    authority: &Authority,
    work: &Path,
    target: &Triple,
    project_env_key: &wamn_registry::ProjectEnvKey,
    source: &stream::Config,
    advisory: &stream::Config,
    consumers: &[pull::Config],
) -> anyhow::Result<EventBroker> {
    prepare_with(
        Some(authority),
        work,
        target,
        project_env_key,
        source,
        advisory,
        consumers,
    )
}

fn prepare_with(
    authority: Option<&Authority>,
    work: &Path,
    target: &Triple,
    project_env_key: &wamn_registry::ProjectEnvKey,
    source: &stream::Config,
    advisory: &stream::Config,
    consumers: &[pull::Config],
) -> anyhow::Result<EventBroker> {
    for token in [
        target.org.as_str(),
        target.project.as_str(),
        project_env_key.as_str(),
        source.name.as_str(),
        advisory.name.as_str(),
    ] {
        ensure!(
            subject_token(token),
            "event broker identity must use subject-safe tokens"
        );
    }
    let subject =
        wamn_registry::name::event_subject(&(target.org), &(target.project), &target.env, ">");
    ensure!(
        source.subjects == [subject.clone()],
        "source stream subjects differ from the environment"
    );
    let mut provisioning = Vec::new();
    let mut runtime = vec![subject.clone()];
    let mut publisher = vec![subject];
    let mut observer = Vec::new();
    for name in [&source.name, &advisory.name] {
        for operation in ["INFO", "CREATE", "UPDATE", "DELETE"] {
            provisioning.push(format!("$JS.API.STREAM.{operation}.{name}"));
        }
        let info = format!("$JS.API.STREAM.INFO.{name}");
        runtime.push(info.clone());
        publisher.push(info.clone());
        observer.extend([info, format!("$JS.API.STREAM.MSG.GET.{name}")]);
    }
    let mut names = BTreeSet::new();
    for consumer in consumers {
        let name = consumer
            .durable_name
            .as_deref()
            .context("event consumer declaration lacks a durable name")?;
        ensure!(
            subject_token(name),
            "event durable must use subject-safe tokens"
        );
        ensure!(names.insert(name), "event consumers repeat a durable name");
        let suffix = format!("{}.{name}", source.name);
        let info = format!("$JS.API.CONSUMER.INFO.{suffix}");
        provisioning.extend([
            info.clone(),
            format!("$JS.API.CONSUMER.CREATE.{suffix}"),
            format!("$JS.API.CONSUMER.CREATE.{suffix}.>"),
            format!("$JS.API.CONSUMER.DURABLE.CREATE.{suffix}"),
            format!("$JS.API.CONSUMER.DELETE.{suffix}"),
        ]);
        runtime.extend([
            info.clone(),
            format!("$JS.API.CONSUMER.MSG.NEXT.{suffix}"),
            format!("$JS.ACK.{suffix}.>"),
        ]);
        observer.push(info);
    }
    let directory = work.join("event-nats");
    DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .context("create private event broker directory")?;
    let own;
    let authority = if let Some(authority) = authority {
        authority
    } else {
        own = Authority::new(&directory)?;
        &own
    };
    let mut users = Vec::new();
    let provisioning = credentials(
        authority,
        &directory,
        "provisioning",
        &source.name,
        provisioning,
        &mut users,
    )?;
    let runtime = credentials(
        authority,
        &directory,
        "runtime",
        &source.name,
        runtime,
        &mut users,
    )?;
    let publisher = credentials(
        authority,
        &directory,
        "publisher",
        &source.name,
        publisher,
        &mut users,
    )?;
    let observer = credentials(
        authority,
        &directory,
        "observer",
        &source.name,
        observer,
        &mut users,
    )?;
    let development = authority.client(DEVELOPMENT_USER)?;
    users.push(Account {
        user: DEVELOPMENT_USER.into(),
        permissions: None,
    });
    let configuration = directory.join("nats.conf");
    write_private(
        &configuration,
        &serde_json::to_vec(&BrokerConfiguration {
            http: "127.0.0.1:8222",
            tls: authority.server_tls(),
            authorization: Authorization { users },
        })?,
    )?;
    Ok(EventBroker {
        configuration,
        provisioning,
        runtime,
        publisher,
        observer,
        development,
    })
}

/// Read back the broker files that `prepare` wrote into `work`, for a later
/// process that attaches to the same environment.
pub fn load(work: &Path) -> anyhow::Result<EventBroker> {
    let directory = work.join("event-nats");
    let user = |user: String| -> anyhow::Result<Credentials> {
        let cert = directory.join(format!("client-{user}.crt"));
        ensure!(cert.is_file(), "the {user} broker certificate is missing");
        Ok(Credentials {
            ca: directory.join("ca.crt"),
            key: directory.join(format!("client-{user}.key")),
            cert,
            inbox: format!("_INBOX_{user}"),
            user,
        })
    };
    let role = |name: &str| -> anyhow::Result<Credentials> {
        user(
            std::fs::read_to_string(directory.join(format!("{name}-username")))
                .with_context(|| format!("read the {name} broker username"))?,
        )
    };
    Ok(EventBroker {
        configuration: directory.join("nats.conf"),
        provisioning: role("provisioning")?,
        runtime: role("runtime")?,
        publisher: role("publisher")?,
        observer: role("observer")?,
        development: user(DEVELOPMENT_USER.into())?,
    })
}

fn subject_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn credentials(
    authority: &Authority,
    directory: &Path,
    role: &str,
    stream: &str,
    publish: Vec<String>,
    users: &mut Vec<Account>,
) -> anyhow::Result<Credentials> {
    let username = format!("{role}_{stream}");
    write_private(
        &directory.join(format!("{role}-username")),
        username.as_bytes(),
    )?;
    let subscribe = vec![format!("_INBOX_{username}.>")];
    users.push(Account {
        user: username.clone(),
        permissions: Some(Permissions {
            publish: Subjects { allow: publish },
            subscribe: Subjects { allow: subscribe },
        }),
    });
    authority.client(&username)
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create private file {}", path.display()))?;
    file.write_all(bytes).context("write private broker file")
}

/// Connect with the selected role and its private reply subjects.
pub async fn connect(
    credentials: &Credentials,
    server: &str,
) -> anyhow::Result<async_nats::Client> {
    credentials
        .connect(server)
        .await
        .context("connect targeted event broker client")
}

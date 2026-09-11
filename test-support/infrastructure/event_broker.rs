//! Private broker credentials for one application's cluster tests.

use std::collections::BTreeSet;
use std::fs::{DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use async_nats::jetstream::{consumer::pull, stream};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::Serialize;
use wamn_control_registry::Triple;

/// A username and its private password file, without the password in memory.
#[derive(Debug)]
pub struct Credentials {
    pub username: String,
    pub password_file: PathBuf,
}

/// The broker files and separate credentials for one test environment.
#[derive(Debug)]
pub struct EventBroker {
    pub configuration: PathBuf,
    pub binding: PathBuf,
    pub provisioning: Credentials,
    pub runtime: Credentials,
    pub publisher: Credentials,
    pub materializer: Credentials,
    pub observer: Credentials,
}

#[derive(Serialize)]
struct BrokerConfiguration {
    http: &'static str,
    authorization: Authorization,
}

#[derive(Serialize)]
struct Authorization {
    users: Vec<User>,
}

#[derive(Serialize)]
struct User {
    user: String,
    password: String,
    permissions: Permissions,
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
pub fn prepare(
    work: &Path,
    scope: &Triple,
    tenant: &str,
    source: &stream::Config,
    advisory: &stream::Config,
    consumers: &[pull::Config],
) -> anyhow::Result<EventBroker> {
    for token in [
        scope.org.as_str(),
        scope.project.as_str(),
        scope.env.as_str(),
        tenant,
        source.name.as_str(),
        advisory.name.as_str(),
    ] {
        ensure!(
            subject_token(token),
            "event broker identity must use subject-safe tokens"
        );
    }
    let subject = format!(
        "evt.{}.{}.{}.>",
        scope.org,
        scope.project,
        scope.env.as_str()
    );
    ensure!(
        source.subjects == [subject.clone()],
        "source stream subjects differ from the environment"
    );
    let tap = format!("tap.{tenant}.{}.{}.>", scope.project, scope.env.as_str());
    let mut provisioning = Vec::new();
    let mut runtime = vec![subject.clone(), tap.clone()];
    let mut publisher = vec![subject];
    let mut materializer = vec![format!("$JS.API.STREAM.INFO.{}", source.name)];
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
            .context("materializer declaration lacks a durable name")?;
        ensure!(
            subject_token(name),
            "materializer durable must use subject-safe tokens"
        );
        ensure!(
            names.insert(name),
            "event registrations repeat a durable name"
        );
        let suffix = format!("{}.{name}", source.name);
        let info = format!("$JS.API.CONSUMER.INFO.{suffix}");
        provisioning.extend([
            info.clone(),
            format!("$JS.API.CONSUMER.CREATE.{suffix}"),
            format!("$JS.API.CONSUMER.CREATE.{suffix}.>"),
            format!("$JS.API.CONSUMER.DURABLE.CREATE.{suffix}"),
            format!("$JS.API.CONSUMER.DELETE.{suffix}"),
        ]);
        materializer.extend([
            info.clone(),
            format!("$JS.API.CONSUMER.MSG.NEXT.{suffix}"),
            format!("$JS.ACK.{suffix}.>"),
        ]);
        runtime.push(info.clone());
        observer.push(info);
    }
    let directory = work.join("event-nats");
    DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .context("create private event broker directory")?;
    let mut users = Vec::new();
    let provisioning = credentials(
        &directory,
        "provisioning",
        &source.name,
        provisioning,
        None,
        &mut users,
    )?;
    let runtime = credentials(
        &directory,
        "runtime",
        &source.name,
        runtime,
        Some(tap.clone()),
        &mut users,
    )?;
    let publisher = credentials(
        &directory,
        "publisher",
        &source.name,
        publisher,
        None,
        &mut users,
    )?;
    let materializer = credentials(
        &directory,
        "materializer",
        &source.name,
        materializer,
        None,
        &mut users,
    )?;
    let observer = credentials(
        &directory,
        "observer",
        &source.name,
        observer,
        Some(tap),
        &mut users,
    )?;
    let configuration = directory.join("nats.conf");
    write_private(
        &configuration,
        &serde_json::to_vec(&BrokerConfiguration {
            http: "127.0.0.1:8222",
            authorization: Authorization { users },
        })?,
    )?;
    Ok(EventBroker {
        configuration,
        binding: directory.join("binding.json"),
        provisioning,
        runtime,
        publisher,
        materializer,
        observer,
    })
}

fn subject_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn credentials(
    directory: &Path,
    role: &str,
    stream: &str,
    publish: Vec<String>,
    tap: Option<String>,
    users: &mut Vec<User>,
) -> anyhow::Result<Credentials> {
    let username = format!("{role}_{stream}");
    let mut random = [0u8; 32];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| anyhow::anyhow!("generate broker password"))?;
    let password = hex::encode(random);
    let password_file = directory.join(format!("{role}-password"));
    write_private(&password_file, password.as_bytes())?;
    write_private(
        &directory.join(format!("{role}-username")),
        username.as_bytes(),
    )?;
    let mut subscribe = vec![format!("_INBOX_{username}.>")];
    subscribe.extend(tap);
    users.push(User {
        user: username.clone(),
        password,
        permissions: Permissions {
            publish: Subjects { allow: publish },
            subscribe: Subjects { allow: subscribe },
        },
    });
    Ok(Credentials {
        username,
        password_file,
    })
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

/// Write the native binding after container inspection supplies the broker address.
pub fn write_binding(
    broker: &EventBroker,
    server: &str,
    source: &stream::Config,
) -> anyhow::Result<()> {
    let password = std::fs::read_to_string(&broker.materializer.password_file)
        .context("read materializer password file")?;
    let binding = serde_json::json!({
        "servers": server,
        "username": broker.materializer.username,
        "password": password,
        "inbox-prefix": format!("_INBOX_{}", broker.materializer.username),
        "stream-allow": source.name,
        "subject-allow": source.subjects.join(","),
        "subscription-capacity-bytes": "4194304",
        "subscription-capacity": "64",
        "max-in-flight": "64"
    });
    write_private(&broker.binding, &serde_json::to_vec(&binding)?)
}

/// Connect with the selected role and its private reply subjects.
pub async fn connect(
    credentials: &Credentials,
    server: &str,
) -> anyhow::Result<async_nats::Client> {
    let password =
        std::fs::read_to_string(&credentials.password_file).context("read broker password file")?;
    async_nats::ConnectOptions::new()
        .user_and_password(credentials.username.clone(), password)
        .custom_inbox_prefix(format!("_INBOX_{}", credentials.username))
        .connect(server)
        .await
        .context("connect scoped event broker client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::ScratchRoot;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn directory() -> ScratchRoot {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = ScratchRoot(std::env::temp_dir().join(format!(
            "event-broker-tests-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        std::fs::create_dir(root.path()).unwrap();
        root
    }

    fn declarations() -> (Triple, stream::Config, stream::Config, pull::Config) {
        let scope = Triple::new("acme", "receiving", "dev");
        let source = stream::Config {
            name: "EVT_4_acme_9_receiving_3_dev".into(),
            subjects: vec!["evt.acme.receiving.dev.>".into()],
            ..Default::default()
        };
        let advisory = stream::Config {
            name: format!("WAMN_EVENT_ADVISORIES_{}", source.name),
            ..Default::default()
        };
        let consumer = pull::Config {
            durable_name: Some("mat_acme_wamn_receiving_registered".into()),
            ..Default::default()
        };
        (scope, source, advisory, consumer)
    }

    #[test]
    fn private_files_keep_passwords_out_of_debug_and_bind_the_inspected_server() {
        let root = directory();
        let (scope, source, advisory, consumer) = declarations();
        let broker = prepare(root.path(), &scope, "acme", &source, &advisory, &[consumer]).unwrap();
        assert!(!broker.binding.exists());
        write_binding(&broker, "nats://172.20.0.9:4222", &source).unwrap();
        assert_eq!(
            std::fs::metadata(root.path().join("event-nats"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for entry in std::fs::read_dir(root.path().join("event-nats")).unwrap() {
            assert_eq!(
                entry.unwrap().metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let binding: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&broker.binding).unwrap()).unwrap();
        let password = std::fs::read_to_string(&broker.materializer.password_file).unwrap();
        assert_eq!(binding["password"], password);
        assert!(!format!("{broker:?}").contains(&password));
        assert_eq!(binding["servers"], "nats://172.20.0.9:4222");
        assert_eq!(binding["stream-allow"], source.name);
        assert_eq!(binding["subject-allow"], source.subjects[0]);
        assert_eq!(binding["subscription-capacity-bytes"], "4194304");
        assert!(write_binding(&broker, "nats://other:4222", &source).is_err());
    }

    #[test]
    fn runtime_roles_cannot_manage_streams_or_consumers() {
        let root = directory();
        let (scope, source, advisory, consumer) = declarations();
        let broker = prepare(root.path(), &scope, "acme", &source, &advisory, &[consumer]).unwrap();
        let configuration: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&broker.configuration).unwrap()).unwrap();
        assert_eq!(configuration["http"], "127.0.0.1:8222");
        let mut passwords = BTreeSet::new();
        for user in configuration["authorization"]["users"].as_array().unwrap() {
            assert!(passwords.insert(user["password"].as_str().unwrap()));
            let provisioning = user["user"] == broker.provisioning.username;
            let mut manages = false;
            for subject in user["permissions"]["publish"]["allow"].as_array().unwrap() {
                let subject = subject.as_str().unwrap();
                let management = [".CREATE.", ".UPDATE.", ".DELETE."]
                    .iter()
                    .any(|operation| subject.contains(operation));
                manages |= management;
                assert!(
                    provisioning || !management,
                    "runtime management permission: {subject}"
                );
                assert!(
                    subject.contains(&source.name)
                        || subject == "evt.acme.receiving.dev.>"
                        || subject == "tap.acme.receiving.dev.>"
                );
            }
            assert_eq!(manages, provisioning);
            let subscriptions = user["permissions"]["subscribe"]["allow"]
                .as_array()
                .unwrap();
            assert_eq!(
                subscriptions[0],
                format!("_INBOX_{}.>", user["user"].as_str().unwrap())
            );
            assert_eq!(
                subscriptions.len(),
                if user["user"] == broker.observer.username
                    || user["user"] == broker.runtime.username
                {
                    2
                } else {
                    1
                }
            );
        }
        assert_eq!(passwords.len(), 5);
    }

    #[test]
    fn invalid_scope_and_repeated_durables_leave_no_broker_files() {
        let root = directory();
        let (scope, mut source, advisory, consumer) = declarations();
        source.subjects = vec!["evt.acme.*.dev.>".into()];
        assert!(
            prepare(
                root.path(),
                &scope,
                "acme",
                &source,
                &advisory,
                &[consumer.clone()]
            )
            .is_err()
        );
        source.subjects = vec!["evt.acme.receiving.dev.>".into()];
        assert!(
            prepare(
                root.path(),
                &scope,
                "*",
                &source,
                &advisory,
                &[consumer.clone()]
            )
            .is_err()
        );
        assert!(
            prepare(
                root.path(),
                &scope,
                "acme",
                &source,
                &advisory,
                &[consumer.clone(), consumer]
            )
            .is_err()
        );
        assert!(!root.path().join("event-nats").exists());
    }
}

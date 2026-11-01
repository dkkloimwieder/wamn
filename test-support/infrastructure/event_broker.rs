//! Private broker credentials for one application's cluster tests.

use std::collections::BTreeSet;
use std::fs::{DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use async_nats::jetstream::{consumer::pull, stream};
use serde::Serialize;
use wamn_control_registry::Triple;

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
    pub materializer: Credentials,
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
    scope: &Triple,
    env_key: &str,
    source: &stream::Config,
    advisory: &stream::Config,
    consumers: &[pull::Config],
) -> anyhow::Result<EventBroker> {
    prepare_with(None, work, scope, env_key, source, advisory, consumers)
}

/// [`prepare`] with the CA of `authority`, for brokers of several
/// environments on one server.
pub fn prepare_in(
    authority: &Authority,
    work: &Path,
    scope: &Triple,
    env_key: &str,
    source: &stream::Config,
    advisory: &stream::Config,
    consumers: &[pull::Config],
) -> anyhow::Result<EventBroker> {
    prepare_with(
        Some(authority),
        work,
        scope,
        env_key,
        source,
        advisory,
        consumers,
    )
}

fn prepare_with(
    authority: Option<&Authority>,
    work: &Path,
    scope: &Triple,
    env_key: &str,
    source: &stream::Config,
    advisory: &stream::Config,
    consumers: &[pull::Config],
) -> anyhow::Result<EventBroker> {
    for token in [
        scope.org.as_str(),
        scope.project.as_str(),
        scope.env.as_str(),
        env_key,
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
    let mut provisioning = Vec::new();
    let mut runtime = vec![subject.clone()];
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
    let materializer = credentials(
        authority,
        &directory,
        "materializer",
        &source.name,
        materializer,
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
        materializer,
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
        materializer: role("materializer")?,
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
        let scope = Triple::new("example", "fixture", "dev");
        let source = stream::Config {
            name: "EVT_7_example_7_fixture_3_dev".into(),
            subjects: vec!["evt.example.fixture.dev.>".into()],
            ..Default::default()
        };
        let advisory = stream::Config {
            name: format!("WAMN_EVENT_ADVISORIES_{}", source.name),
            ..Default::default()
        };
        let consumer = pull::Config {
            durable_name: Some("mat_example_platform_fixture_registered".into()),
            ..Default::default()
        };
        (scope, source, advisory, consumer)
    }

    #[test]
    fn the_broker_files_are_private() {
        let root = directory();
        let (scope, source, advisory, consumer) = declarations();
        prepare(
            root.path(),
            &scope,
            "example",
            &source,
            &advisory,
            &[consumer],
        )
        .unwrap();
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
    }

    #[test]
    fn runtime_roles_cannot_manage_streams_or_consumers() {
        let root = directory();
        let (scope, source, advisory, consumer) = declarations();
        let broker = prepare(
            root.path(),
            &scope,
            "example",
            &source,
            &advisory,
            &[consumer],
        )
        .unwrap();
        let configuration: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&broker.configuration).unwrap()).unwrap();
        assert_eq!(configuration["http"], "127.0.0.1:8222");
        assert_eq!(configuration["tls"]["verify_and_map"], true);
        let users = configuration["authorization"]["users"].as_array().unwrap();
        // R4: no user has a password. The development user has every permission.
        assert!(users.iter().all(|user| user.get("password").is_none()));
        let mut names = BTreeSet::new();
        for user in users {
            assert!(names.insert(user["user"].as_str().unwrap()));
            if user["user"] == crate::nats_tls::DEVELOPMENT_USER {
                assert!(user.get("permissions").is_none());
                continue;
            }
            let provisioning = user["user"] == broker.provisioning.user.as_str();
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
                assert!(subject.contains(&source.name) || subject == "evt.example.fixture.dev.>");
            }
            assert_eq!(manages, provisioning);
            let subscriptions = user["permissions"]["subscribe"]["allow"]
                .as_array()
                .unwrap();
            assert_eq!(
                subscriptions[0],
                format!("_INBOX_{}.>", user["user"].as_str().unwrap())
            );
            assert_eq!(subscriptions.len(), 1);
        }
        assert_eq!(names.len(), 6);
    }

    #[test]
    fn invalid_scope_and_repeated_durables_leave_no_broker_files() {
        let root = directory();
        let (scope, mut source, advisory, consumer) = declarations();
        source.subjects = vec!["evt.example.*.dev.>".into()];
        assert!(
            prepare(
                root.path(),
                &scope,
                "example",
                &source,
                &advisory,
                std::slice::from_ref(&consumer)
            )
            .is_err()
        );
        source.subjects = vec!["evt.example.fixture.dev.>".into()];
        assert!(
            prepare(
                root.path(),
                &scope,
                "*",
                &source,
                &advisory,
                std::slice::from_ref(&consumer)
            )
            .is_err()
        );
        assert!(
            prepare(
                root.path(),
                &scope,
                "example",
                &source,
                &advisory,
                &[consumer.clone(), consumer]
            )
            .is_err()
        );
        assert!(!root.path().join("event-nats").exists());
    }
}

//! Event broker users and credential Secrets for the declared environments.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_control_provision::events::{advisory_stream_config, source_stream_config};

use crate::environment::document::EnvironmentDocument;
use crate::{kubectl, release_chart::Target};

use super::install::apply_object;

/// The Secret that holds the authorization file of the one NATS.
pub(super) const AUTHORIZATION: &str = "wamn-nats-authorization";

#[derive(Serialize, Deserialize)]
pub(super) struct Authorization {
    users: Vec<Account>,
}

/// A NATS user. A user with no password is a certificate user: the server
/// maps a client certificate to it by the certificate's DNS name.
#[derive(Serialize, Deserialize)]
struct Account {
    user: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<String>,
    permissions: Permissions,
}

#[derive(Serialize, Deserialize)]
struct Permissions {
    publish: Subjects,
    subscribe: Subjects,
}

#[derive(Serialize, Deserialize)]
struct Subjects {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    deny: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Secret {
    api_version: &'static str,
    kind: &'static str,
    metadata: Metadata,
    #[serde(rename = "type")]
    content_type: &'static str,
    data: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct Metadata {
    name: String,
    namespace: String,
}

fn secret(namespace: &str, name: &str, data: BTreeMap<String, String>) -> Secret {
    Secret {
        api_version: "v1",
        kind: "Secret",
        metadata: Metadata {
            name: name.into(),
            namespace: namespace.into(),
        },
        content_type: "Opaque",
        data: data
            .into_iter()
            .map(|(key, value)| (key, STANDARD.encode(value)))
            .collect(),
    }
}

pub(super) fn documents(
    directory: &Path,
) -> anyhow::Result<Vec<(std::path::PathBuf, EnvironmentDocument)>> {
    let mut paths = std::fs::read_dir(directory)
        .with_context(|| format!("read environments {}", directory.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    paths
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "k"))
        .map(|path| Ok((path.clone(), EnvironmentDocument::compile(&path)?)))
        .collect()
}

/// The certificate users of the platform clients (docs/plan/alignment.md R4).
///
/// The operator and the host scheduler connection reach only the wasmCloud
/// control subjects. The data user of the chart grants nothing, because each
/// environment's host connects with its own certificate.
fn platform_users() -> Vec<Account> {
    let subjects = |allow: &[&str]| Subjects {
        allow: allow.iter().map(|subject| (*subject).into()).collect(),
        deny: if allow.is_empty() {
            vec![">".into()]
        } else {
            Vec::new()
        },
    };
    let user = |name: &str, publish: &[&str], subscribe: &[&str]| Account {
        user: name.into(),
        password: None,
        permissions: Permissions {
            publish: subjects(publish),
            subscribe: subjects(subscribe),
        },
    };
    let control = ["runtime.host.>", "runtime.operator.>", "_INBOX.>"];
    // Identity owns both memory buckets (docs/plan/alignment.md R7). Making a
    // bucket reads the account limits first ($JS.API.INFO).
    let identity = [
        "$JS.API.INFO",
        "$KV.wamn-auth.>",
        "$KV.wamn-connection.>",
        "$JS.API.STREAM.INFO.KV_wamn-auth",
        "$JS.API.STREAM.INFO.KV_wamn-connection",
        "$JS.API.STREAM.CREATE.KV_wamn-auth",
        "$JS.API.STREAM.CREATE.KV_wamn-connection",
        "$JS.API.STREAM.UPDATE.KV_wamn-auth",
        "$JS.API.STREAM.UPDATE.KV_wamn-connection",
    ];
    // wamn-ctl and the provisioning Jobs run the same verbs: they manage the
    // event streams and consumers, read the advisories, and delete memory keys.
    let manage = [
        "$KV.wamn-auth.>",
        "$KV.wamn-connection.>",
        "$JS.API.STREAM.INFO.>",
        "$JS.API.STREAM.MSG.GET.>",
        "$JS.API.STREAM.CREATE.*",
        "$JS.API.STREAM.UPDATE.*",
        "$JS.API.STREAM.DELETE.*",
        "$JS.API.CONSUMER.INFO.>",
        "$JS.API.CONSUMER.CREATE.>",
        "$JS.API.CONSUMER.DURABLE.CREATE.>",
        "$JS.API.CONSUMER.DELETE.>",
    ];
    // A stream name is one subject token, so the grant above names every
    // stream. The memory buckets stay identity's.
    let manager = |name: &str| {
        let mut account = user(name, &manage, &["_INBOX.>"]);
        for operation in ["CREATE", "UPDATE", "DELETE"] {
            for bucket in ["wamn-auth", "wamn-connection"] {
                account
                    .permissions
                    .publish
                    .deny
                    .push(format!("$JS.API.STREAM.{operation}.KV_{bucket}"));
            }
        }
        account
    };
    vec![
        user("wasmcloud-operator", &control, &control),
        user("wasmcloud-runtime", &control, &control),
        user("wasmcloud-data", &[], &[]),
        user("wamn-identity", &identity, &["_INBOX.>"]),
        manager("wamn-ctl"),
        user(
            "wamn-cdc-reader",
            &["evt.>", "$JS.API.STREAM.INFO.>"],
            &["_INBOX.>"],
        ),
        manager("wamn-provisioning"),
    ]
}

/// The first authorization file: the platform users and no environment.
pub(super) fn initial_authorization() -> String {
    let authorization = Authorization {
        users: platform_users(),
    };
    format!("authorization: {}\n", json!(authorization))
}

/// Put each platform user in `authorization`, in place of its old entry, and
/// drop every password user of an older install (docs/plan/alignment.md R4).
fn keep_platform_users(authorization: &mut Authorization) {
    authorization.users.retain(|user| user.password.is_none());
    for user in platform_users() {
        keep_user(authorization, user);
    }
}

pub(super) async fn apply(target: &Target, environments: &Path, org: &str) -> anyhow::Result<bool> {
    let live: Value = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        AUTHORIZATION,
    )
    .await?
    .context("missing Secret platform/wamn-nats-authorization")?;
    let documents = documents(environments)?;
    for (path, document) in &documents {
        ensure!(
            document.org == org,
            "environment {} names another org",
            path.display()
        );
    }
    let documents = documents
        .into_iter()
        .map(|(_, document)| document)
        .collect::<Vec<_>>();
    let mut changed = false;
    for object in objects(&live, org, &documents)? {
        changed |= apply_object(target, &object).await?;
    }
    Ok(changed)
}

pub(super) fn objects(
    live: &Value,
    org: &str,
    environments: &[EnvironmentDocument],
) -> anyhow::Result<Vec<Value>> {
    let encoded = live["data"]["authorization.conf"]
        .as_str()
        .context("the broker Secret has no authorization.conf")?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!("decode the broker authorization"))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| anyhow::anyhow!("decode the broker authorization"))?;
    let mut authorization: Authorization = serde_json::from_str(
        text.strip_prefix("authorization: ")
            .context("the broker authorization has no prefix")?,
    )
    .map_err(|_| anyhow::anyhow!("decode the broker authorization"))?;
    keep_platform_users(&mut authorization);
    let (control, certificate) = control_user(org);
    keep_user(&mut authorization, control);
    let mut objects = vec![certificate];
    for document in environments {
        ensure!(document.org == org, "environment names another org");
        objects.extend(render(&document.triple(), &mut authorization));
    }
    // Preserve the original bytes when the user set did not change. Formatting
    // alone must not rewrite an existing broker Secret.
    let desired = serde_json::to_value(&authorization)?;
    let original: Value = text
        .strip_prefix("authorization: ")
        .context("the broker authorization does not start with `authorization: `")
        .and_then(|body| {
            serde_json::from_str(body)
                .map_err(|_| anyhow::anyhow!("decode the broker authorization"))
        })?;
    if desired != original {
        let object = secret(
            "platform",
            AUTHORIZATION,
            [(
                "authorization.conf".into(),
                format!("authorization: {desired}\n"),
            )]
            .into(),
        );
        objects.push(serde_json::to_value(object)?);
    }
    Ok(objects)
}

/// The NATS objects of a cluster that runs the platform in one `namespace`, as
/// the kind tests do: the authorization Secret with the platform users, the
/// control host user of `org` and one user per environment of `environments`,
/// and each user's Certificate and materializer binding.
///
/// # Errors
///
/// Returns an error when an object does not render.
pub fn single_namespace_nats(
    namespace: &str,
    org: &str,
    environments: &[wamn_control_registry::Triple],
) -> anyhow::Result<Vec<Value>> {
    let mut authorization = Authorization {
        users: platform_users(),
    };
    let (control, certificate) = control_user(org);
    keep_user(&mut authorization, control);
    let mut objects = vec![certificate];
    for scope in environments {
        objects.extend(render(scope, &mut authorization));
    }
    objects.push(serde_json::to_value(secret(
        "platform",
        AUTHORIZATION,
        [(
            "authorization.conf".into(),
            format!("authorization: {}\n", json!(authorization)),
        )]
        .into(),
    ))?);
    objects
        .into_iter()
        .map(|object| super::manifests::in_namespace(object, namespace))
        .collect()
}

/// The one NATS, its certificates and its users for a cluster that runs the
/// platform in one `namespace`: [`super::manifests::one_nats`] and
/// [`single_namespace_nats`] together.
///
/// # Errors
///
/// Returns an error when a manifest is unreadable or an object does not render.
pub fn kind_nats(
    repository: &Path,
    namespace: &str,
    org: &str,
    environments: &[wamn_control_registry::Triple],
) -> anyhow::Result<Vec<Value>> {
    let mut objects = super::manifests::one_nats(repository, namespace)?;
    objects.extend(single_namespace_nats(namespace, org, environments)?);
    Ok(objects)
}

/// Render the environment's NATS objects without a cluster or a live broker.
pub fn render_event_secrets(document: &EnvironmentDocument) -> anyhow::Result<Vec<Value>> {
    render(&document.triple(), &mut Authorization { users: Vec::new() })
        .into_iter()
        .map(|object| serde_json::to_value(object).context("encode event object"))
        .collect()
}

fn render(scope: &wamn_control_registry::Triple, authorization: &mut Authorization) -> Vec<Value> {
    let env_key =
        wamn_control_registry::project_env_key(&scope.org, &scope.project, scope.env.as_str());
    let source = source_stream_config(scope, 1, Duration::from_secs(120));
    let advisory = advisory_stream_config(scope, 1);
    let user = wamn_control_registry::environment_nats_user(&env_key);
    // The environment's events, and what its host and its materializer read
    // on that environment's own stream: every consumer of the stream, by
    // wildcard. Stream and consumer management stays with the provisioning
    // client (docs/plan/alignment.md R5, docs/architecture/capabilities.md).
    let mut publish = vec![source.subjects[0].clone()];
    for name in [&source.name, &advisory.name] {
        publish.push(format!("$JS.API.STREAM.INFO.{name}"));
    }
    publish.extend([
        format!("$JS.API.CONSUMER.INFO.{}.*", source.name),
        format!("$JS.API.CONSUMER.MSG.NEXT.{}.*", source.name),
        format!("$JS.ACK.{}.>", source.name),
    ]);
    for bucket in ["wamn-auth", "wamn-connection"] {
        publish.extend(memory_subjects(bucket, &env_key));
    }
    publish.push(format!(
        "$JS.API.CONSUMER.CREATE.KV_wamn-auth.*.$KV.wamn-auth.{}.>",
        wamn_platform_identity::auth_changes::IDENTITY_TOKEN
    ));
    keep_user(
        authorization,
        Account {
            user: user.clone(),
            password: None,
            permissions: Permissions {
                publish: Subjects {
                    allow: publish,
                    deny: Vec::new(),
                },
                subscribe: Subjects {
                    allow: vec![format!("_INBOX_{user}.>")],
                    deny: Vec::new(),
                },
            },
        },
    );
    // The release chart declares the materializer's binding, which inherits
    // the host's data connection (docs/plan/alignment.md R6).
    vec![environment_certificate(
        &format!("wamn-nats-{env_key}"),
        &user,
    )]
}

/// What a host may do on its own keys of the memory `bucket`: put and delete
/// them, read them by key, watch them, and read the bucket's metadata
/// (docs/plan/alignment.md R5, A7).
fn memory_subjects(bucket: &str, prefix: &str) -> [String; 4] {
    [
        format!("$KV.{bucket}.{prefix}.>"),
        format!("$JS.API.STREAM.INFO.KV_{bucket}"),
        format!("$JS.API.DIRECT.GET.KV_{bucket}.$KV.{bucket}.{prefix}.>"),
        format!("$JS.API.CONSUMER.CREATE.KV_{bucket}.*.$KV.{bucket}.{prefix}.>"),
    ]
}

/// Put `user` in `authorization`, in place of the entry of the same name.
fn keep_user(authorization: &mut Authorization, user: Account) {
    if let Some(existing) = authorization
        .users
        .iter_mut()
        .find(|old| old.user == user.user)
    {
        *existing = user;
    } else {
        authorization.users.push(user);
    }
}

/// The client certificate of a NATS user in the hosts namespace. Its DNS name
/// is the user (`verify_and_map`).
fn environment_certificate(name: &str, user: &str) -> Value {
    json!({
        "apiVersion": "cert-manager.io/v1",
        "kind": "Certificate",
        "metadata": {"name": name, "namespace": "hosts"},
        "spec": {
            "secretName": name,
            "commonName": user,
            "dnsNames": [user],
            "usages": ["client auth", "digital signature", "key encipherment"],
            "issuerRef": {"kind": "ClusterIssuer", "name": "wasmcloud-ca", "group": "cert-manager.io"}
        }
    })
}

/// The NATS user of the control host of `org` and its Certificate. It covers
/// every environment of the org (docs/plan/alignment.md R7).
fn control_user(org: &str) -> (Account, Value) {
    let user = wamn_control_registry::control_nats_user(org);
    let mut publish = Vec::new();
    for bucket in ["wamn-auth", "wamn-connection"] {
        publish.extend(memory_subjects(bucket, "*"));
        publish.push(format!("$KV.{bucket}.>"));
        // The control host watches the whole bucket, with the filter `>`.
        publish.push(format!(
            "$JS.API.CONSUMER.CREATE.KV_{bucket}.*.$KV.{bucket}.>"
        ));
    }
    publish.sort();
    publish.dedup();
    let account = Account {
        user: user.clone(),
        password: None,
        permissions: Permissions {
            publish: Subjects {
                allow: publish,
                deny: Vec::new(),
            },
            subscribe: Subjects {
                allow: vec![format!("_INBOX_{user}.>")],
                deny: Vec::new(),
            },
        },
    };
    (
        account,
        environment_certificate(&format!("wamn-nats-control-{org}"), &user),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(project: &str) -> EnvironmentDocument {
        let mut document = EnvironmentDocument::from_json(
            br#"{
            "org":"test","project":"one","env":"dev","release":"none",
            "target": "cloud", "route_host":"one.example.com","policy":"dev","applications":{}
        }"#,
        )
        .unwrap();
        document.project = project.into();
        document
    }

    #[test]
    fn repeated_render_keeps_one_certificate_user_per_environment() {
        let mut authorization = Authorization {
            users: platform_users(),
        };
        let platform = authorization.users.len();
        let first = render(&document("one").triple(), &mut authorization);
        let users = serde_json::to_value(&authorization).unwrap();
        let repeated = render(&document("one").triple(), &mut authorization);
        assert_eq!(first, repeated);
        assert_eq!(users, serde_json::to_value(&authorization).unwrap());
        assert_eq!(authorization.users.len(), platform + 1);
        render(&document("two").triple(), &mut authorization);
        assert_eq!(authorization.users.len(), platform + 2);
        // R4: no user has a password.
        assert!(
            !serde_json::to_string(&authorization)
                .unwrap()
                .contains("password")
        );
        let certificate = &first[0];
        assert_eq!(certificate["kind"], "Certificate");
        assert_eq!(certificate["metadata"]["name"], "wamn-nats-test--one--dev");
        assert_eq!(
            certificate["spec"]["dnsNames"],
            json!(["wamn-env-test--one--dev"])
        );
        assert_eq!(certificate["spec"]["issuerRef"]["name"], "wasmcloud-ca");
    }

    #[test]
    fn an_environment_user_reaches_no_subject_of_another() {
        let mut authorization = Authorization { users: Vec::new() };
        render(&document("one").triple(), &mut authorization);
        render(&document("two").triple(), &mut authorization);
        let subjects = |user: &Account| {
            let mut all = user.permissions.publish.allow.clone();
            all.extend(user.permissions.subscribe.allow.clone());
            all
        };
        let (one, two) = (&authorization.users[0], &authorization.users[1]);
        assert_eq!(one.user, "wamn-env-test--one--dev");
        for subject in subjects(one) {
            // R5: the subjects of one environment name only its own keys,
            // streams, consumers and inbox. The bucket metadata and the
            // identity watch are the shared, read-only exceptions.
            let shared = subject.starts_with("$JS.API.STREAM.INFO.KV_")
                || subject.contains("$KV.wamn-auth.=identity.");
            assert!(
                shared || subject.contains("one"),
                "{subject} is not scoped to environment one"
            );
            assert!(
                !subject.contains("two"),
                "{subject} reaches environment two"
            );
            assert!(!subjects(two).contains(&subject) || shared, "{subject}");
        }
        for bucket in ["wamn-auth", "wamn-connection"] {
            assert!(
                one.permissions
                    .publish
                    .allow
                    .contains(&format!("$KV.{bucket}.test--one--dev.>"))
            );
        }
        // Environment one is refused consumer and stream management on its own
        // event stream; only the filtered KV watches are its own consumers.
        let stream =
            source_stream_config(&document("one").triple(), 1, Duration::from_secs(120)).name;
        for subject in [
            format!("$JS.API.CONSUMER.CREATE.{stream}.mat"),
            format!("$JS.API.CONSUMER.CREATE.{stream}.mat.evt.test.one.dev.>"),
            format!("$JS.API.CONSUMER.DURABLE.CREATE.{stream}.mat"),
            format!("$JS.API.CONSUMER.DELETE.{stream}.mat"),
            format!("$JS.API.STREAM.CREATE.{stream}"),
            format!("$JS.API.STREAM.UPDATE.{stream}"),
            format!("$JS.API.STREAM.DELETE.{stream}"),
        ] {
            assert!(
                !one.permissions
                    .publish
                    .allow
                    .iter()
                    .any(|pattern| subject_matches(pattern, &subject)),
                "{subject} is granted"
            );
        }
        assert!(
            one.permissions
                .publish
                .allow
                .iter()
                .any(|pattern| subject_matches(
                    pattern,
                    &format!("$JS.API.CONSUMER.MSG.NEXT.{stream}.mat")
                ))
        );
    }

    /// The control host and an environment host open their auth memory
    /// watches as their own users on a real server (docs/plan/alignment.md
    /// R5, R7, A7). Each user gets a password here in place of its
    /// certificate, and the permissions are the rendered ones.
    #[tokio::test]
    #[ignore = "requires: WAMN_NATIVE_C_NATS_BIN"]
    async fn the_control_and_environment_users_open_their_memory_watches() {
        let binary = std::env::var_os("WAMN_NATIVE_C_NATS_BIN")
            .expect("set WAMN_NATIVE_C_NATS_BIN to a nats-server executable");
        let directory = std::env::temp_dir().join(format!("wamn-broker-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut authorization = Authorization { users: Vec::new() };
        render(&document("one").triple(), &mut authorization);
        authorization.users.push(control_user("test").0);
        authorization.users.push(Account {
            user: "admin".into(),
            password: None,
            permissions: Permissions {
                publish: Subjects {
                    allow: vec![">".into()],
                    deny: Vec::new(),
                },
                subscribe: Subjects {
                    allow: vec![">".into()],
                    deny: Vec::new(),
                },
            },
        });
        for user in &mut authorization.users {
            user.password = Some("p".into());
        }
        let configuration = directory.join("nats.conf");
        std::fs::write(
            &configuration,
            format!(
                "listen: \"127.0.0.1:{port}\"\njetstream {{ store_dir: {:?} }}\nauthorization: {}\n",
                directory.join("store"),
                json!(authorization)
            ),
        )
        .unwrap();
        let mut server = std::process::Command::new(binary)
            .arg("-c")
            .arg(&configuration)
            .spawn()
            .unwrap();
        let url = format!("nats://127.0.0.1:{port}");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let admin = loop {
            match async_nats::ConnectOptions::with_user_and_password("admin".into(), "p".into())
                .connect(url.clone())
                .await
            {
                Ok(client) => break client,
                Err(error) => assert!(std::time::Instant::now() < deadline, "{error}"),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        wamn_platform_identity::kv_memory::create_buckets(&admin)
            .await
            .unwrap();
        let env_key = wamn_control_registry::project_env_key("test", "one", "dev");
        for (user, scope) in [
            (
                wamn_control_registry::environment_nats_user(&env_key),
                Some(env_key.as_str()),
            ),
            (wamn_control_registry::control_nats_user("test"), None),
        ] {
            eprintln!("{user} opens its auth memory watch");
            let client =
                async_nats::ConnectOptions::with_user_and_password(user.clone(), "p".into())
                    .custom_inbox_prefix(format!("_INBOX_{user}"))
                    .connect(url.clone())
                    .await
                    .unwrap();
            let cache = std::sync::Arc::new(wamn_runtime::auth_cache::AuthCache::default());
            wamn_runtime::auth_cache::test_util::watched(&client, scope, cache).await;
        }
        let _ = server.kill();
        let _ = server.wait();
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Whether the NATS subject `pattern` matches `subject`.
    fn subject_matches(pattern: &str, subject: &str) -> bool {
        let mut subject = subject.split('.');
        for token in pattern.split('.') {
            match (token, subject.next()) {
                (">", Some(_)) => return true,
                ("*", Some(_)) => {}
                (token, Some(value)) if token == value => {}
                _ => return false,
            }
        }
        subject.next().is_none()
    }

    #[test]
    fn an_older_install_loses_its_password_users() {
        let mut authorization: Authorization = serde_json::from_value(json!({"users":[{
            "user":"platform-seal","password":"old",
            "permissions":{"publish":{"deny":[">"]},"subscribe":{"deny":[">"]}}
        }]}))
        .unwrap();
        keep_platform_users(&mut authorization);
        assert!(
            authorization
                .users
                .iter()
                .all(|user| user.password.is_none())
        );
        assert_eq!(authorization.users.len(), platform_users().len());
    }

    /// Kind run 7: the invite Job, as wamn-ctl, found no wamn-auth bucket.
    /// Identity makes the buckets as its own user, and wamn-ctl then deletes
    /// a memory key, on a real server with the rendered platform users.
    #[tokio::test]
    #[ignore = "requires: WAMN_NATIVE_C_NATS_BIN"]
    async fn identity_makes_the_buckets_that_wamn_ctl_writes() {
        let binary = std::env::var_os("WAMN_NATIVE_C_NATS_BIN")
            .expect("set WAMN_NATIVE_C_NATS_BIN to a nats-server executable");
        let directory = std::env::temp_dir().join(format!("wamn-broker-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut authorization = Authorization {
            users: platform_users(),
        };
        for user in &mut authorization.users {
            user.password = Some("p".into());
        }
        let configuration = directory.join("nats.conf");
        std::fs::write(
            &configuration,
            format!(
                "listen: \"127.0.0.1:{port}\"\njetstream {{ store_dir: {:?} }}\nauthorization: {}\n",
                directory.join("store"),
                json!(authorization)
            ),
        )
        .unwrap();
        let mut server = std::process::Command::new(binary)
            .arg("-c")
            .arg(&configuration)
            .spawn()
            .unwrap();
        let url = format!("nats://127.0.0.1:{port}");
        let connect = |user: &'static str| {
            let url = url.clone();
            async move {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                loop {
                    match async_nats::ConnectOptions::with_user_and_password(
                        user.into(),
                        "p".into(),
                    )
                    .connect(url.clone())
                    .await
                    {
                        Ok(client) => break client,
                        Err(error) => assert!(std::time::Instant::now() < deadline, "{error}"),
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        };
        let identity = connect("wamn-identity").await;
        let made = wamn_platform_identity::kv_memory::create_buckets(&identity).await;
        let ctl = connect("wamn-ctl").await;
        let opened = wamn_platform_identity::auth_changes::auth_bucket(&ctl).await;
        let deleted = match &opened {
            Ok(bucket) => bucket
                .delete("identity.someone")
                .await
                .map_err(anyhow::Error::from),
            Err(_) => Ok(()),
        };
        let _ = server.kill();
        let _ = server.wait();
        let _ = std::fs::remove_dir_all(&directory);
        made.expect("identity makes the memory buckets");
        opened.expect("wamn-ctl opens wamn-auth");
        deleted.expect("wamn-ctl deletes a memory key");
    }
}

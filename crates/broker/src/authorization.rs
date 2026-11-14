//! Event broker users and credential Secrets for the declared environments.

use std::time::Duration;

use crate::events::{advisory_stream_config, source_stream_config};
use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use wamn_registry::Triple;

/// The Secret that holds the authorization file of the one NATS.
pub const AUTHORIZATION: &str = "wamn-nats-authorization";

#[derive(Serialize, Deserialize)]
struct Authorization {
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
    // Identity connects through the broker, which creates both memory buckets.
    // Creating a bucket reads the account limits first ($JS.API.INFO).
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
        "$JS.API.STREAM.PURGE.*",
        "$JS.API.CONSUMER.INFO.>",
        "$JS.API.CONSUMER.CREATE.>",
        "$JS.API.CONSUMER.DURABLE.CREATE.>",
        "$JS.API.CONSUMER.DELETE.>",
    ];
    // A stream name is one subject token, so the grant above names every
    // stream. Only the identity connection may change memory bucket settings.
    let manager = |name: &str| {
        let mut account = user(name, &manage, &["_INBOX.>"]);
        for operation in ["CREATE", "UPDATE", "DELETE", "PURGE"] {
            for bucket in ["wamn-auth", "wamn-connection"] {
                account
                    .permissions
                    .publish
                    .deny
                    .push(wamn_registry::name::nats_subject(&[
                        "$JS",
                        "API",
                        "STREAM",
                        operation,
                        &wamn_registry::name::nats_token("KV_", bucket, ""),
                    ]));
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

/// Put each platform user in `authorization`, in place of its old entry, and
/// drop every password user of an older install (docs/plan/alignment.md R4).
fn keep_platform_users(authorization: &mut Authorization) {
    authorization.users.retain(|user| user.password.is_none());
    for user in platform_users() {
        keep_user(authorization, user);
    }
}

fn read_authorization(live: Option<&str>) -> anyhow::Result<Authorization> {
    Ok(match live {
        Some(encoded) => {
            let bytes = STANDARD
                .decode(encoded)
                .map_err(|_| anyhow::anyhow!("decode the broker authorization"))?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| anyhow::anyhow!("decode the broker authorization"))?;
            serde_json::from_str::<Authorization>(
                text.strip_prefix("authorization: ")
                    .context("the broker authorization has no prefix")?,
            )
            .map_err(|_| anyhow::anyhow!("decode the broker authorization"))?
        }
        None => Authorization { users: Vec::new() },
    })
}

fn render(target: &wamn_registry::Triple, authorization: &mut Authorization) -> Vec<Value> {
    let project_env_key =
        wamn_registry::project_env_key(&target.org, &target.project, target.env.as_str());
    let source = source_stream_config(target, 1, Duration::from_secs(120));
    let advisory = advisory_stream_config(target, 1);
    let user = wamn_registry::environment_nats_user(&project_env_key);
    // The environment host reads its application consumers.
    // Provisioning declares the streams and consumers before attachment.
    let mut publish = vec![source.subjects[0].clone()];
    for name in [&source.name, &advisory.name] {
        publish.push(wamn_registry::name::nats_subject(&[
            "$JS",
            "API",
            "STREAM",
            "INFO",
            (name).as_ref(),
        ]));
    }
    publish.extend([
        wamn_registry::name::nats_subject(&[
            "$JS",
            "API",
            "CONSUMER",
            "INFO",
            (source.name).as_ref(),
            "*",
        ]),
        wamn_registry::name::nats_subject(&[
            "$JS",
            "API",
            "CONSUMER",
            "MSG",
            "NEXT",
            (source.name).as_ref(),
            "*",
        ]),
        wamn_registry::name::nats_subject(&["$JS", "ACK", (source.name).as_ref(), ">"]),
    ]);
    for bucket in ["wamn-auth", "wamn-connection"] {
        publish.extend(memory_subjects(bucket, &project_env_key));
    }
    publish.push(wamn_registry::name::nats_subject(&[
        "$JS",
        "API",
        "CONSUMER",
        "CREATE",
        "KV_wamn-auth",
        "*",
        "$KV",
        "wamn-auth",
        crate::auth_changes::IDENTITY_TOKEN,
        ">",
    ]));
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
                    allow: vec![wamn_registry::name::nats_subject(&[
                        &wamn_registry::name::nats_token("_INBOX_", (user).as_ref(), ""),
                        ">",
                    ])],
                    deny: Vec::new(),
                },
            },
        },
    );
    // The environment host mounts this certificate only for this project's event client.
    vec![environment_certificate(
        &wamn_registry::name::nats_certificate(&project_env_key),
        &user,
    )]
}

/// What a host may do on its own keys of the memory `bucket`: put and delete
/// them, read them by key, watch them, and read the bucket's metadata
/// (docs/plan/alignment.md R5, A7).
fn memory_subjects(bucket: &str, prefix: &str) -> [String; 4] {
    [
        wamn_registry::name::nats_subject(&["$KV", bucket, prefix, ">"]),
        wamn_registry::name::nats_subject(&[
            "$JS",
            "API",
            "STREAM",
            "INFO",
            &wamn_registry::name::nats_token("KV_", bucket, ""),
        ]),
        wamn_registry::name::nats_subject(&[
            "$JS",
            "API",
            "DIRECT",
            "GET",
            &wamn_registry::name::nats_token("KV_", bucket, ""),
            "$KV",
            bucket,
            prefix,
            ">",
        ]),
        wamn_registry::name::nats_subject(&[
            "$JS",
            "API",
            "CONSUMER",
            "CREATE",
            &wamn_registry::name::nats_token("KV_", bucket, ""),
            "*",
            "$KV",
            bucket,
            prefix,
            ">",
        ]),
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
    let user = wamn_registry::control_nats_user(org);
    let mut publish = Vec::new();
    for bucket in ["wamn-auth", "wamn-connection"] {
        publish.extend(memory_subjects(bucket, "*"));
        publish.push(wamn_registry::name::nats_subject(&["$KV", bucket, ">"]));
        // The control host watches the whole bucket, with the filter `>`.
        publish.push(wamn_registry::name::nats_subject(&[
            "$JS",
            "API",
            "CONSUMER",
            "CREATE",
            &wamn_registry::name::nats_token("KV_", bucket, ""),
            "*",
            "$KV",
            bucket,
            ">",
        ]));
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
                allow: vec![wamn_registry::name::nats_subject(&[
                    &wamn_registry::name::nats_token("_INBOX_", (user).as_ref(), ""),
                    ">",
                ])],
                deny: Vec::new(),
            },
        },
    };
    (
        account,
        environment_certificate(&wamn_registry::name::control_nats_certificate(org), &user),
    )
}

/// Merge platform or environment users through the single authorization Secret writer.
pub async fn update_authorization(
    target: &wamn_kubernetes::Target,
    org: &str,
    coordinate: Option<&Triple>,
) -> anyhow::Result<Vec<Value>> {
    let mut certificates = Vec::new();
    wamn_kubernetes::secret::update(
        &target.kubeconfig,
        &target.context,
        "platform",
        AUTHORIZATION,
        |secret| {
            let live = if secret.data.is_empty() {
                None
            } else {
                Some(
                    secret
                        .data
                        .get("authorization.conf")
                        .context("the broker Secret has no authorization.conf")?
                        .as_str(),
                )
            };
            anyhow::ensure!(
                coordinate.is_none() || secret.metadata.resource_version.is_some(),
                "missing event broker authorization Secret"
            );
            let mut authorization = read_authorization(live)?;
            if let Some(target) = coordinate {
                certificates = render(target, &mut authorization);
            } else {
                keep_platform_users(&mut authorization);
                let (user, certificate) = control_user(org);
                keep_user(&mut authorization, user);
                certificates = vec![certificate];
            }
            secret.data.insert(
                "authorization.conf".into(),
                STANDARD.encode(format!("authorization: {}\n", json!(authorization))),
            );
            Ok(())
        },
    )
    .await?;
    Ok(certificates)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(project: &str) -> Triple {
        Triple::new("test", "dev", project)
    }

    #[test]
    fn platform_certificate_users_keep_scheduler_permissions_targeted() {
        let users = platform_users();
        assert!(users.iter().all(|user| user.password.is_none()));
        for name in ["wasmcloud-operator", "wasmcloud-runtime"] {
            let user = users.iter().find(|user| user.user == name).unwrap();
            for subjects in [&user.permissions.publish, &user.permissions.subscribe] {
                assert_eq!(
                    subjects.allow,
                    ["runtime.host.>", "runtime.operator.>", "_INBOX.>"]
                );
            }
        }
    }

    #[test]
    fn an_environment_user_reaches_no_subject_of_another() {
        let mut authorization = Authorization { users: Vec::new() };
        render(&document("one"), &mut authorization);
        render(&document("two"), &mut authorization);
        let subjects = |user: &Account| {
            let mut all = user.permissions.publish.allow.clone();
            all.extend(user.permissions.subscribe.allow.clone());
            all
        };
        let (one, two) = (&authorization.users[0], &authorization.users[1]);
        for subject in subjects(one) {
            // R5: the subjects of one environment name only its own keys,
            // streams, consumers and inbox. The bucket metadata and the
            // identity watch are the shared, read-only exceptions.
            let shared = subject.starts_with("$JS.API.STREAM.INFO.KV_")
                || subject.contains("$KV.wamn-auth.=identity.");
            assert!(
                shared || subject.contains("one"),
                "{subject} is not targeted to environment one"
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
                    .contains(&wamn_registry::name::nats_subject(&[
                        "$KV",
                        (bucket).as_ref(),
                        &wamn_registry::project_env_key("test", "one", "dev"),
                        ">"
                    ]))
            );
        }
        // Native hosts attach to provisioned consumers. Broker mutations stay refused.
        let stream = source_stream_config(&document("one"), 1, Duration::from_secs(120)).name;
        for subject in [
            wamn_registry::name::nats_subject(&[
                "$JS",
                "API",
                "CONSUMER",
                "CREATE",
                (stream).as_ref(),
                "mat",
            ]),
            wamn_registry::name::nats_subject(&[
                "$JS",
                "API",
                "CONSUMER",
                "CREATE",
                (stream).as_ref(),
                "mat",
                "evt",
                "test",
                "dev",
                "two",
                ">",
            ]),
            wamn_registry::name::nats_subject(&[
                "$JS",
                "API",
                "CONSUMER",
                "DURABLE",
                "CREATE",
                (stream).as_ref(),
                "mat",
            ]),
            wamn_registry::name::nats_subject(&[
                "$JS",
                "API",
                "CONSUMER",
                "DELETE",
                (stream).as_ref(),
                "mat",
            ]),
            wamn_registry::name::nats_subject(&[
                "$JS",
                "API",
                "STREAM",
                "CREATE",
                (stream).as_ref(),
            ]),
            wamn_registry::name::nats_subject(&[
                "$JS",
                "API",
                "STREAM",
                "UPDATE",
                (stream).as_ref(),
            ]),
            wamn_registry::name::nats_subject(&[
                "$JS",
                "API",
                "STREAM",
                "DELETE",
                (stream).as_ref(),
            ]),
            wamn_registry::name::nats_subject(&[
                "$JS",
                "API",
                "CONSUMER",
                "CREATE",
                (stream).as_ref(),
                "mat",
                "evt",
                "test",
                "dev",
                "one",
                "order",
                ">",
            ]),
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
        for subject in [wamn_registry::name::nats_subject(&[
            "$JS",
            "API",
            "CONSUMER",
            "MSG",
            "NEXT",
            (stream).as_ref(),
            "mat",
        ])] {
            assert!(
                one.permissions
                    .publish
                    .allow
                    .iter()
                    .any(|pattern| subject_matches(pattern, &subject))
            );
        }
    }

    /// The control host and an environment host open their auth memory
    /// watches as their own users on a real server (docs/plan/alignment.md
    /// R5, R7, A7). Each user gets a password here in place of its
    /// certificate, and the permissions are the rendered ones.
    #[tokio::test]
    #[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
    async fn the_control_and_environment_users_open_their_memory_watches() {
        wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");
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
        render(&document("one"), &mut authorization);
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
        crate::kv_memory::create_buckets(&admin).await.unwrap();
        let project_env_key = wamn_registry::project_env_key("test", "one", "dev");
        for (user, target) in [
            (
                wamn_registry::environment_nats_user(&project_env_key),
                Some(project_env_key.as_str()),
            ),
            (wamn_registry::control_nats_user("test"), None),
        ] {
            eprintln!("{user} opens its auth memory watch");
            let client =
                async_nats::ConnectOptions::with_user_and_password(user.clone(), "p".into())
                    .custom_inbox_prefix(wamn_registry::name::nats_subject(&[
                        &wamn_registry::name::nats_token("_INBOX_", (user).as_ref(), ""),
                    ]))
                    .connect(url.clone())
                    .await
                    .unwrap();
            let cache = std::sync::Arc::new(wamn_runtime::auth_cache::AuthCache::default());
            wamn_runtime::auth_cache::test_util::watched(&client, target, cache).await;
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
    #[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
    async fn identity_makes_the_buckets_that_wamn_ctl_writes() {
        wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");
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
        let made = crate::kv_memory::create_buckets(&identity).await;
        let ctl = connect("wamn-ctl").await;
        let opened = crate::auth_changes::auth_bucket(&ctl).await;
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

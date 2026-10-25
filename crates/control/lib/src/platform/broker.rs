//! Event broker users and credential Secrets for the declared environments.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::rand::SecureRandom as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_control_provision::events::{advisory_stream_config, source_stream_config};

use crate::delivery::environment::EnvironmentFile;
use crate::environment::document::EnvironmentDocument;
use crate::{kubectl, release_chart::Target};

use super::install::apply_object;

const SERVER: &str = "nats://evt-nats.platform.svc.cluster.local:4222";

#[derive(Serialize, Deserialize)]
struct Authorization {
    users: Vec<Account>,
}

#[derive(Serialize, Deserialize)]
struct Account {
    user: String,
    password: String,
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

pub(super) async fn apply(
    target: &Target,
    repository: &Path,
    environments: &Path,
    org: &str,
) -> anyhow::Result<bool> {
    let live: Value = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        "evt-nats-authorization",
    )
    .await?
    .context("missing Secret platform/evt-nats-authorization")?;
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
    let mut changed = false;
    for (path, document) in documents(environments)? {
        ensure!(
            document.org == org,
            "environment {} names another org",
            path.display()
        );
        let file: EnvironmentFile =
            serde_json::from_slice(&std::fs::read(path.with_extension("json"))?)?;
        let manifests = file
            .packages
            .iter()
            .map(|package| {
                let path = wamn_schema_generator::package_manifest_path(&repository.join(package));
                Ok(wamn_schema_generator::PackageManifest::from_slice(
                    &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
                )?)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        for object in render(&document, &manifests, &mut authorization)? {
            changed |= apply_object(target, &serde_json::to_value(object)?).await?;
        }
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
            "evt-nats-authorization",
            [(
                "authorization.conf".into(),
                format!("authorization: {desired}\n"),
            )]
            .into(),
        );
        changed |= apply_object(target, &serde_json::to_value(object)?).await?;
    }
    Ok(changed)
}

fn render(
    document: &EnvironmentDocument,
    manifests: &[wamn_schema_generator::PackageManifest],
    authorization: &mut Authorization,
) -> anyhow::Result<Vec<Secret>> {
    let scope = document.triple();
    let tenant = format!("{}--{}--{}", document.org, document.project, document.env);
    let source = source_stream_config(&scope, 1, Duration::from_secs(120));
    let advisory = advisory_stream_config(&scope, 1);
    let consumers = wamn_control_provision::events::entity_consumers(
        &scope,
        manifests.iter().filter(|manifest| {
            document
                .applications
                .values()
                .any(|application| application.packages.contains(&manifest.package.id))
        }),
    );
    let subject = source.subjects[0].clone();
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
    for consumer in consumers {
        let name = consumer
            .durable_name
            .context("materializer has no durable name")?;
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
    let mut secrets = Vec::new();
    for (role, publish) in [
        ("provisioning", provisioning),
        ("publisher", publisher),
        ("observer", observer),
        ("runtime", runtime),
        ("materializer", materializer),
    ] {
        let username = format!("{role}_{}", source.name);
        let password = if let Some(user) = authorization
            .users
            .iter()
            .find(|user| user.user == username)
        {
            user.password.clone()
        } else {
            let mut bytes = [0; 32];
            ring::rand::SystemRandom::new()
                .fill(&mut bytes)
                .map_err(|_| anyhow::anyhow!("generate event broker password"))?;
            hex::encode(bytes)
        };
        let subscribe = vec![format!("_INBOX_{username}.>")];
        let user = Account {
            user: username.clone(),
            password: password.clone(),
            permissions: Permissions {
                publish: Subjects {
                    allow: publish,
                    deny: Vec::new(),
                },
                subscribe: Subjects {
                    allow: subscribe,
                    deny: Vec::new(),
                },
            },
        };
        if let Some(existing) = authorization
            .users
            .iter_mut()
            .find(|old| old.user == username)
        {
            *existing = user;
        } else {
            authorization.users.push(user);
        }
        let mut data: BTreeMap<String, String> = [
            ("username".into(), username.clone()),
            ("password".into(), password.clone()),
        ]
        .into();
        if role == "runtime" {
            data.extend([
                ("org".into(), document.org.clone()),
                ("project".into(), document.project.clone()),
                ("environment".into(), document.env.clone()),
                ("stream_replicas".into(), "1".into()),
                ("dup_window_secs".into(), "120".into()),
            ]);
        }
        if role == "materializer" {
            data = [(
                "binding.json".into(),
                json!({
                    "servers": SERVER, "username": username, "password": password,
                    "inbox-prefix": format!("_INBOX_{username}"),
                    "stream-allow": source.name, "subject-allow": source.subjects.join(","),
                    "subscription-capacity-bytes": "4194304", "subscription-capacity": "64",
                    "max-in-flight": "64"
                })
                .to_string(),
            )]
            .into();
        }
        let name = format!("evt-nats-{tenant}-{role}");
        let namespace = if matches!(role, "runtime" | "materializer") {
            "hosts"
        } else {
            "platform"
        };
        secrets.push(secret(namespace, &name, data.clone()));
        if role == "publisher" {
            secrets.push(secret("hosts", &name, data));
        }
    }
    Ok(secrets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_render_keeps_credentials_and_other_users() {
        let document = EnvironmentDocument::from_json(
            br#"{
            "org":"test","project":"one","env":"dev","release":"none",
            "route_host":"one.example.com","policy":"dev","applications":{}
        }"#,
        )
        .unwrap();
        let mut authorization: Authorization = serde_json::from_value(json!({"users":[{
            "user":"platform-seal","password":"keep",
            "permissions":{"publish":{"deny":[">"]},"subscribe":{"deny":[">"]}}
        }]}))
        .unwrap();
        let seal = serde_json::to_value(&authorization.users[0]).unwrap();
        let first =
            serde_json::to_value(render(&document, &[], &mut authorization).unwrap()).unwrap();
        let users = serde_json::to_value(&authorization).unwrap();
        let repeated =
            serde_json::to_value(render(&document, &[], &mut authorization).unwrap()).unwrap();
        assert_eq!(first, repeated);
        assert_eq!(users, serde_json::to_value(&authorization).unwrap());
        assert_eq!(serde_json::to_value(&authorization.users[0]).unwrap(), seal);
        assert_eq!(authorization.users.len(), 6);
        let mut other = document.clone();
        other.project = "two".into();
        render(&other, &[], &mut authorization).unwrap();
        assert_eq!(authorization.users.len(), 11);
        assert_eq!(serde_json::to_value(&authorization.users[0]).unwrap(), seal);
        for user in &authorization.users[1..] {
            assert!(
                user.permissions
                    .publish
                    .allow
                    .iter()
                    .chain(&user.permissions.subscribe.allow)
                    .all(|subject| !subject.starts_with("tap."))
            );
            let manages = user
                .permissions
                .publish
                .allow
                .iter()
                .any(|subject| subject.contains(".CREATE."));
            assert_eq!(manages, user.user.starts_with("provisioning_"));
        }
    }
}

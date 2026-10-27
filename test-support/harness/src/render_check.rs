//! Render-only checks for the release fixtures. No host or cluster starts here.

use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::Value;
use wamn_control::release_chart::{self, ImageSet};
use wamn_test_infrastructure::rendering::kubernetes_documents;
use wamn_test_infrastructure::scratch::ScratchRoot;

/// Render the source chart with fixture image references, without pulling images.
pub fn template(values: &serde_yaml::Value, namespace: &str) -> anyhow::Result<Vec<Value>> {
    let work = ScratchRoot::create()?;
    let image = format!(
        "registry.invalid/wamn/fixture:render@sha256:{}",
        "1".repeat(64)
    );
    let chart = release_chart::stamp(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/platform/release"),
        &ImageSet {
            host: image.clone(),
            http: image.clone(),
            materializer: image,
        },
        None,
        &work.path().join("chart"),
    )?;
    let file = work.path().join("values.yaml");
    std::fs::write(&file, serde_yaml::to_string(values)?)?;
    let coordinate = values["environment"]
        .as_str()
        .context("rendered environment")?
        .split('/')
        .collect::<Vec<_>>();
    ensure!(coordinate.len() == 3, "environment coordinate");
    let name = release_chart::release_name(coordinate[0], coordinate[1], coordinate[2])?;
    let output = std::process::Command::new("helm")
        .args(["template", &name, "--namespace", namespace])
        .arg(&chart)
        .arg("--values")
        .arg(&file)
        .output()
        .context("start helm template")?;
    ensure!(
        output.status.success(),
        "helm template: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    kubernetes_documents(&output.stdout)
}

/// Check named Secret keys and the bindings of every selected host group.
///
/// `secrets` contains emitted Secrets and explicit prerequisite declarations.
/// A prerequisite declaration uses the same metadata and key shape as a Secret.
pub fn assert_complete(documents: &[Value], secrets: &[Value]) -> anyhow::Result<()> {
    let all = documents.iter().chain(secrets).collect::<Vec<_>>();
    for document in documents {
        let namespace = document["metadata"]["namespace"]
            .as_str()
            .unwrap_or(crate::case_release::NAMESPACE);
        secret_references(document, namespace, &all)?;
        if document["kind"] == "Deployment" {
            let pod = &document["spec"]["template"]["spec"];
            for container in pod["containers"]
                .as_array()
                .context("deployment containers")?
            {
                for variable in container["env"].as_array().into_iter().flatten() {
                    let Some(path) = variable["value"].as_str() else {
                        continue;
                    };
                    for mount in container["volumeMounts"].as_array().into_iter().flatten() {
                        let prefix =
                            format!("{}/", mount["mountPath"].as_str().context("mount path")?);
                        if path.starts_with(&prefix)
                            && pod["volumes"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .any(|volume| {
                                    volume["name"] == mount["name"] && volume["secret"].is_object()
                                })
                        {
                            mounted_secret_key(pod, container, path, &all, namespace)?;
                        }
                    }
                }
            }
        }
    }
    for workload in documents
        .iter()
        .filter(|doc| doc["kind"] == "WorkloadDeployment")
    {
        let spec = &workload["spec"]["template"]["spec"];
        let selector = spec["hostSelector"]
            .as_object()
            .context("workload host selector")?;
        let hosts = documents
            .iter()
            .filter(|doc| {
                doc["kind"] == "Deployment"
                    && selector.iter().all(|(key, value)| {
                        doc["spec"]["template"]["metadata"]["labels"][if key == "hostgroup" {
                            "wasmcloud.com/hostgroup"
                        } else {
                            key
                        }] == *value
                    })
            })
            .collect::<Vec<_>>();
        ensure!(
            !hosts.is_empty(),
            "no host group matches {}",
            workload["metadata"]["name"]
        );
        for host in hosts {
            let pod = &host["spec"]["template"]["spec"];
            let container = pod["containers"]
                .as_array()
                .context("host containers")?
                .iter()
                .find(|container| {
                    container["env"]
                        .as_array()
                        .is_some_and(|env| env.iter().any(|var| var["name"] == "WAMN_ORG"))
                })
                .context("host container")?;
            let env = container["env"].as_array().context("host environment")?;
            let require = |name: &str| -> anyhow::Result<&Value> {
                env.iter()
                    .find(|var| var["name"] == name)
                    .with_context(|| format!("host {} lacks {name}", host["metadata"]["name"]))
            };
            for interface in spec["hostInterfaces"]
                .as_array()
                .context("workload host interfaces")?
            {
                match (
                    interface["namespace"].as_str(),
                    interface["package"].as_str(),
                ) {
                    (Some("wasmcloud"), Some("nats")) if interface["name"] == "events" => {
                        let binding = require("WAMN_MAT_NATS_BINDING_FILE")?["value"]
                            .as_str()
                            .context("events binding path")?;
                        mounted_secret_key(
                            pod,
                            container,
                            binding,
                            &all,
                            host["metadata"]["namespace"]
                                .as_str()
                                .context("host namespace")?,
                        )?;
                    }
                    (Some("wamn"), Some("postgres")) => {
                        require("WAMN_PG_URL")?;
                        if spec["service"]["localResources"]["config"]["wamn.postgres.authority"]
                            == "event-materializer"
                        {
                            require("WAMN_EVENT_MATERIALIZER_PG_URL")?;
                        }
                    }
                    (Some("wamn"), Some("operation" | "router-delivery")) => {
                        require("WAMN_EXECUTOR_PLATFORM_PG_URL")?;
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn secret_key(
    namespace: &str,
    name: &str,
    key: Option<&str>,
    secrets: &[&Value],
) -> anyhow::Result<()> {
    let secret = secrets
        .iter()
        .find(|secret| {
            secret["kind"] == "Secret"
                && secret["metadata"]["name"] == name
                && secret["metadata"]["namespace"] == namespace
        })
        .with_context(|| format!("missing Secret {namespace}/{name}"))?;
    if let Some(key) = key {
        ensure!(
            secret["data"].get(key).is_some() || secret["stringData"].get(key).is_some(),
            "Secret {namespace}/{name} lacks key {key}"
        );
    }
    Ok(())
}

fn secret_references(value: &Value, namespace: &str, secrets: &[&Value]) -> anyhow::Result<()> {
    match value {
        Value::Object(map) => {
            if let Some(reference) = map.get("secretRef") {
                secret_key(
                    namespace,
                    reference["name"].as_str().context("envFrom Secret name")?,
                    None,
                    secrets,
                )?;
            }
            if let Some(reference) = map.get("secretKeyRef") {
                secret_key(
                    namespace,
                    reference["name"]
                        .as_str()
                        .context("Secret reference name")?,
                    Some(reference["key"].as_str().context("Secret reference key")?),
                    secrets,
                )?;
            }
            if let Some(reference) = map.get("secret")
                && let Some(name) = reference["secretName"]
                    .as_str()
                    .or_else(|| reference["name"].as_str())
            {
                secret_key(namespace, name, None, secrets)?;
                if let Some(items) = reference["items"].as_array() {
                    for item in items {
                        secret_key(
                            namespace,
                            name,
                            Some(item["key"].as_str().context("Secret volume key")?),
                            secrets,
                        )?;
                    }
                }
            }
            if let Some(references) = map.get("imagePullSecrets").and_then(Value::as_array) {
                for reference in references {
                    secret_key(
                        namespace,
                        reference["name"].as_str().context("image pull Secret")?,
                        Some(".dockerconfigjson"),
                        secrets,
                    )?;
                }
            }
            for nested in map.values() {
                secret_references(nested, namespace, secrets)?;
            }
        }
        Value::Array(values) => {
            for nested in values {
                secret_references(nested, namespace, secrets)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn mounted_secret_key(
    pod: &Value,
    container: &Value,
    path: &str,
    secrets: &[&Value],
    namespace: &str,
) -> anyhow::Result<()> {
    for mount in container["volumeMounts"]
        .as_array()
        .context("host volume mounts")?
    {
        let prefix = format!("{}/", mount["mountPath"].as_str().context("mount path")?);
        if let Some(relative) = path.strip_prefix(&prefix) {
            let volume = pod["volumes"]
                .as_array()
                .context("host volumes")?
                .iter()
                .find(|volume| volume["name"] == mount["name"])
                .context("binding volume")?;
            let secret = &volume["secret"];
            let key = match secret["items"].as_array() {
                Some(items) => items
                    .iter()
                    .find(|item| item["path"] == relative)
                    .and_then(|item| item["key"].as_str())
                    .context("binding Secret item")?,
                None => relative,
            };
            return secret_key(
                namespace,
                secret["secretName"]
                    .as_str()
                    .context("binding Secret name")?,
                Some(key),
                secrets,
            );
        }
    }
    anyhow::bail!("events binding {path} has no Secret mount")
}

/// Project fixture roles without the Wasm artifact that kind composes.
/// The entry reference is only chart input. Helm never pulls it.
pub fn fixture_roles(
    manifest: &wamn_catalog::ServingManifest,
    org: &str,
    project: &str,
    env: &str,
) -> Vec<release_chart::Role> {
    let mut roles = release_chart::roles(manifest, org, project, env);
    for role in &mut roles {
        if role.name == release_chart::RoleName::Http && role.entry.is_none() {
            role.entry = Some(format!("entries@sha256:{}", "1".repeat(64)));
        }
    }
    roles
}

/// Render the apply path with its real credential references and emission list.
pub fn environment(
    document: &wamn_control::environment::document::EnvironmentDocument,
    manifest: &wamn_catalog::ServingManifest,
    host_group: &Path,
    drain_bound_seconds: u32,
) -> anyhow::Result<serde_yaml::Value> {
    let mut applications = document.applications.clone();
    for application in applications.values_mut() {
        application.connections.clear();
    }
    wamn_control::environment::write::render(
        &release_chart::ValuesInput {
            org: document.org.clone(),
            project: document.project.clone(),
            env: document.env.clone(),
            manifest_digest: manifest.digest().as_str().to_owned(),
            artifact_base: "registry.invalid/wamn/releases".into(),
            route_host: document.route_host.clone(),
            applications,
            roles: fixture_roles(manifest, &document.org, &document.project, &document.env),
            drain_bound_seconds,
            actor: "prekind render".into(),
            host_group: serde_yaml::Mapping::new(),
        },
        Some(host_group),
    )
}

/// Render the credential Secrets from apply's producer list, with disposable URLs.
pub fn credential_secrets(triple: &wamn_control_registry::Triple, namespace: &str) -> Vec<Value> {
    use wamn_control_provision::{
        WorkloadRoleFamily, WorkloadSecretBody, render_workload_secret_manifest,
    };
    wamn_control_provision::host_credentials::HOST_CREDENTIALS
        .iter()
        .map(|credential| credential.family)
        .map(|family| {
            let body = if family == WorkloadRoleFamily::App {
                WorkloadSecretBody::TenantUrl {
                    tenant: "render",
                    tenant_key: "render",
                    url: "postgresql://render:render@database:5432/render",
                }
            } else {
                WorkloadSecretBody::Url("postgresql://render:render@database:5432/render")
            };
            render_workload_secret_manifest(family, triple, namespace, body)
        })
        .collect()
}

/// Operator prerequisites from `test-support/infrastructure/platform.rs`:
/// its CA and its runtime client certificate Secrets exist before a case release.
pub fn operator_prerequisites(namespace: &str) -> Vec<Value> {
    [
        (
            "wasmcloud-runtime-tls",
            &["ca.crt", "tls.crt", "tls.key"][..],
        ),
        ("wasmcloud-data-tls", &["ca.crt", "tls.crt", "tls.key"][..]),
    ]
    .into_iter()
    .map(|(name, keys)| prerequisite(name, keys, namespace))
    .collect()
}

/// Declare named prerequisite keys without storing credentials in the test.
pub fn prerequisite(name: &str, keys: &[&str], namespace: &str) -> Value {
    serde_json::json!({"apiVersion":"v1", "kind":"Secret", "metadata":{"name":name,"namespace":namespace},
        "stringData":keys.iter().map(|key| ((*key).to_owned(), Value::String("render".into()))).collect::<serde_json::Map<_, _>>()})
}

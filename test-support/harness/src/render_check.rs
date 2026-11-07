//! Render-only checks for the release fixtures. No host or cluster starts here.

use std::path::Path;

pub use wamn_environment_control::platform::resources::runtime_namespaces;

use anyhow::{Context as _, ensure};
use serde_json::Value;
use wamn_release_control::release_chart::{self, ImageSet};
use wamn_test_infrastructure::rendering::kubernetes_documents;
use wamn_test_infrastructure::scratch::ScratchRoot;

/// Render a case's host values through apply, with the case's platform inputs.
pub fn host_values(
    input: &wamn_test_infrastructure::rendering::HostValuesInput,
    schema: &str,
) -> anyhow::Result<wamn_test_infrastructure::rendering::RenderedHostValues> {
    use serde_json::json;
    use wamn_test_infrastructure::rendering::{RenderedHostValues, host_defaults};

    let mut values = wamn_environment_control::environment::write::render(
        &release_chart::ValuesInput {
            org: input.event.org.clone(),
            project: input.event.project.clone(),
            env: input.event.environment.clone(),
            manifest_digest: input.manifest_digest.clone(),
            artifact_base: input.release_artifact_base.clone(),
            route_host: "fixture.invalid".into(),
            applications: std::collections::BTreeMap::new(),
            roles: Vec::new(),
            drain_bound_seconds: 70,
            actor: "prekind render".into(),
            host_group: serde_yaml::Mapping::new(),
        },
        None,
    )?;
    let group = &mut values["runtime-operator"]["runtime"]["hostGroups"][0];
    group["namespace"] = input.namespace.clone().into();
    group["replicas"] = input.replicas.into();
    group["http"] = serde_yaml::to_value(json!({"enabled": true, "port": 80}))?;
    group["resources"] = serde_yaml::to_value(json!({
        "requests": {"memory": "256Mi", "cpu": "2"},
        "limits": {"memory": "4Gi", "cpu": "6"},
        "defaultHeapMemory": "256MiB", "coreInstances": 512,
        "guestMemoryMode": "count", "maxConcurrentStarts": 4,
    }))?;
    let variables = group["env"]
        .as_sequence_mut()
        .context("apply host variables")?;
    for credential in wamn_control_provision::host_credentials::HOST_CREDENTIALS {
        let Some(variable) = credential.host_variable else {
            continue;
        };
        let selected = if credential.family == wamn_control_provision::WorkloadRoleFamily::App {
            Some(input.guest_secret_name.as_str())
        } else {
            input
                .role_secrets
                .iter()
                .find(|secret| secret.family == credential.family)
                .map(|secret| secret.name.as_str())
        };
        if let Some(name) = selected {
            let entry = variables
                .iter_mut()
                .find(|entry| entry["name"] == variable)
                .context("apply credential variable")?;
            entry["valueFrom"]["secretKeyRef"]["name"] = name.into();
        }
    }
    for (name, value) in [
        ("WAMN_SCHEMA", schema.to_owned()),
        (
            "WAMN_COMPONENT_ARTIFACT_BASE",
            input.component_artifact_base.clone(),
        ),
        ("WAMN_EVT_ORG", input.event.org.clone()),
        ("WAMN_EVT_PROJECT", input.event.project.clone()),
        ("WAMN_EVT_ENV", input.event.environment.clone()),
        (
            "WAMN_EVT_STREAM_REPLICAS",
            input.stream_replicas.to_string(),
        ),
        (
            "WAMN_EVT_DUP_WINDOW_SECS",
            input.dup_window_secs.to_string(),
        ),
        ("WAMN_WASMTIME_CACHE_DIR", "/tmp/wamn-wasmtime-cache".into()),
        ("OTEL_BSP_SCHEDULE_DELAY", "1".into()),
        ("OTEL_BSP_MAX_EXPORT_BATCH_SIZE", "1".into()),
        ("DOCKER_CONFIG", "/etc/wamn/registry".into()),
        (
            "WAMN_REGISTRY_AUTH_FILE",
            "/etc/wamn/registry/config.json".into(),
        ),
    ] {
        variables.push(serde_yaml::to_value(json!({"name": name, "value": value}))?);
    }
    group["volumes"]
        .as_sequence_mut()
        .context("apply host volumes")?
        .push(serde_yaml::to_value(
            json!({"name": "registry-pull", "secret": {
                "secretName": "wamn-registry-pull", "optional": false,
                "items": [{"key": ".dockerconfigjson", "path": "config.json"}],
            }}),
        )?);
    group["volumeMounts"]
        .as_sequence_mut()
        .context("apply host mounts")?
        .push(serde_yaml::to_value(json!(
            {"name": "registry-pull", "mountPath": "/etc/wamn/registry", "readOnly": true}
        ))?);
    group["ociCaPaths"] = serde_yaml::to_value(["/runtime-cert/ca.crt"])?;
    group["extraArgs"]
        .as_sequence_mut()
        .context("release arguments")?
        .push("--allow-insecure-registries".into());
    let mut base: serde_yaml::Value = serde_yaml::from_str(&host_defaults()?)?;
    base["runtime"]["terminationGracePeriodSeconds"] =
        values["runtime-operator"]["runtime"]["terminationGracePeriodSeconds"].clone();
    base["runtime"]["image"]["repository"] = "wamn-host".into();
    base["runtime"]["image"]["tag"] = input.host_tag.clone().into();
    let mut overlay = values["runtime-operator"].clone();
    overlay["global"] = values["global"].clone();
    let mut overlay = serde_yaml::to_string(&overlay)?;
    if let Some(secret) = &input.object_store_secret_name {
        overlay = wamn_test_infrastructure::rendering::object_store_overlay(&overlay, secret)?;
    }
    Ok(RenderedHostValues {
        base: serde_yaml::to_string(&base)?,
        overlay,
    })
}

/// Extract the platform inputs before the release chart derives its fields again.
pub fn host_group(overlay: &str) -> anyhow::Result<serde_yaml::Mapping> {
    let mut group = crate::case_release::host_group(overlay)?;
    if let Some(ports) = group
        .get_mut("ports")
        .and_then(serde_yaml::Value::as_sequence_mut)
    {
        ports.retain(|port| port["name"] != "files");
    }
    group
        .get_mut("env")
        .and_then(serde_yaml::Value::as_sequence_mut)
        .context("apply host variables")?
        .retain(|variable| variable["name"] != "WAMN_MATERIALIZER_DRAIN_BOUND_SECONDS");
    Ok(group)
}

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
                // The chart appends a group's env to the shared defaults
                // without merging by name; the API server refuses a repeat.
                let mut names = std::collections::BTreeSet::new();
                for variable in container["env"].as_array().into_iter().flatten() {
                    let name = variable["name"].as_str().context("env name")?;
                    ensure!(
                        names.insert(name),
                        "{} container {} sets {name} twice",
                        document["metadata"]["name"],
                        container["name"]
                    );
                }
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
                    // The events binding is the host group's `plugins` entry,
                    // which the chart renders into the host configuration file
                    // (docs/plan/alignment.md R6).
                    (Some("wasmcloud"), Some("nats")) if interface["name"] == "events" => {
                        ensure!(
                            container["args"]
                                .as_array()
                                .is_some_and(|args| args.iter().any(|arg| arg
                                    .as_str()
                                    .is_some_and(|arg| arg.starts_with("--user-config=")))),
                            "the host lacks the plugins entry of the events binding"
                        );
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
    document: &wamn_environment_control::environment::document::EnvironmentDocument,
    manifest: &wamn_catalog::ServingManifest,
    host_group: &Path,
    drain_bound_seconds: u32,
) -> anyhow::Result<serde_yaml::Value> {
    let mut applications = document.membership();
    for application in applications.values_mut() {
        application.connections.clear();
    }
    wamn_environment_control::environment::write::render(
        &release_chart::ValuesInput {
            org: document.org.clone(),
            project: document.project.clone(),
            env: document.env.clone(),
            manifest_digest: manifest.digest().as_str().to_owned(),
            artifact_base: "registry.invalid/wamn/releases".into(),
            route_host: document.legacy_hostname().to_owned(),
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
                WorkloadSecretBody::EnvKeyUrl {
                    env_key: "render",
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

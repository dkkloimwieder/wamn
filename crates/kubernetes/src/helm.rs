//! The shared Helm command runner.
use anyhow::{Context as _, ensure};
use serde_json::Value;
use std::{ffi::OsStr, process::Stdio};

pub async fn run(arguments: &[&OsStr]) -> anyhow::Result<Vec<u8>> {
    let output = output(arguments).await?;
    ensure!(
        output.status.success(),
        "helm failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

/// Run Helm and retain its exit status for an inspection caller.
pub async fn output(arguments: &[&OsStr]) -> anyhow::Result<std::process::Output> {
    tokio::process::Command::new("helm")
        .args(arguments)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("start helm")
}

pub fn template(entry: &serde_json::Value) -> anyhow::Result<Vec<serde_json::Value>> {
    use serde::Deserialize as _;
    use std::io::Write as _;
    use std::process::Command;
    let chart = entry
        .get("render_chart")
        .unwrap_or(&entry["chart"])
        .as_str()
        .context("local chart reference")?;
    anyhow::ensure!(
        std::path::Path::new(chart).exists(),
        "local chart {chart} is missing; rendering cannot fetch a chart"
    );
    let shown = Command::new("helm")
        .args(["show", "chart", chart])
        .output()
        .context("read local Helm chart")?;
    anyhow::ensure!(shown.status.success(), "read local chart {chart}");
    let metadata: Value = serde_yaml::from_slice(&shown.stdout)?;
    anyhow::ensure!(
        metadata["version"] == entry["version"],
        "chart {chart} version {} does not match pinned {}",
        metadata["version"],
        entry["version"]
    );
    let name = entry["name"].as_str().context("chart release name")?;
    let namespace = entry["namespace"].as_str().context("chart namespace")?;
    let mut child = Command::new("helm")
        .args(["template", name, chart, "--namespace", namespace, "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start local helm template")?;
    child
        .stdin
        .take()
        .context("helm stdin")?
        .write_all(&serde_json::to_vec(&entry["values"])?)?;
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "helm template {chart}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_yaml::Deserializer::from_slice(&output.stdout)
        .map(serde_json::Value::deserialize)
        .filter_map(|value| match value {
            Ok(value) if value.is_null() => None,
            value => Some(value.map_err(Into::into)),
        })
        .collect()
}

pub async fn require_references(
    namespace: &str,
    objects: &[Value],
    mut get: impl AsyncFnMut(&str, &str, &str) -> anyhow::Result<Option<Value>>,
) -> anyhow::Result<()> {
    use base64::Engine as _;

    let mut references = std::collections::BTreeMap::new();
    for object in objects {
        let pod = match object["kind"].as_str() {
            Some("Deployment" | "StatefulSet" | "DaemonSet" | "Job") => {
                &object["spec"]["template"]["spec"]
            }
            Some("CronJob") => &object["spec"]["jobTemplate"]["spec"]["template"]["spec"],
            _ => continue,
        };
        let namespace = object["metadata"]["namespace"]
            .as_str()
            .unwrap_or(namespace);
        collect_references(
            pod,
            references
                .entry(namespace)
                .or_insert_with(std::collections::BTreeMap::new),
        )?;
    }
    for (reference_namespace, references) in references {
        for ((type_, name), keys) in references {
            let external;
            let object = if let Some(object) = objects.iter().find(|object| {
                object["kind"] == type_
                    && object["metadata"]["name"] == name
                    && object["metadata"]["namespace"]
                        .as_str()
                        .unwrap_or(namespace)
                        == reference_namespace
            }) {
                object
            } else {
                external = get(reference_namespace, &type_, &name)
                    .await?
                    .with_context(|| {
                        format!(
                            "missing required {} {reference_namespace}/{name}. No resources changed.",
                            type_.to_ascii_lowercase()
                        )
                    })?;
                &external
            };
            for key in keys {
                let present = if type_ == "Secret" {
                    object["stringData"][&key]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                        || object["data"][&key].as_str().is_some_and(|value| {
                            base64::engine::general_purpose::STANDARD
                                .decode(value)
                                .is_ok_and(|value| !value.is_empty())
                        })
                } else {
                    object["data"][&key]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                        || object["binaryData"][&key]
                            .as_str()
                            .is_some_and(|value| !value.is_empty())
                };
                ensure!(
                    present,
                    "required {} {reference_namespace}/{name} lacks valid {key}. No resources changed.",
                    type_.to_ascii_lowercase()
                );
            }
        }
    }
    Ok(())
}

fn collect_references(
    value: &Value,
    references: &mut std::collections::BTreeMap<
        (String, String),
        std::collections::BTreeSet<String>,
    >,
) -> anyhow::Result<()> {
    match value {
        Value::Object(fields) => {
            for (field, child) in fields {
                let reference = match field.as_str() {
                    "secretKeyRef" | "secretRef" => Some(("Secret", "name")),
                    "secret" if child.get("secretName").is_some() => Some(("Secret", "secretName")),
                    "configMapKeyRef" | "configMapRef" | "configMap" => Some(("ConfigMap", "name")),
                    _ => None,
                };
                if let Some((type_, name_field)) = reference {
                    if child["optional"] == true {
                        continue;
                    }
                    let name = child[name_field]
                        .as_str()
                        .context("runtime reference name")?;
                    let keys = references.entry((type_.into(), name.into())).or_default();
                    if let Some(key) = child["key"].as_str() {
                        keys.insert(key.to_owned());
                    }
                    for item in child["items"].as_array().into_iter().flatten() {
                        keys.insert(
                            item["key"]
                                .as_str()
                                .context("runtime reference key")?
                                .to_owned(),
                        );
                    }
                } else {
                    collect_references(child, references)?;
                }
            }
        }
        Value::Array(entries) => {
            for entry in entries {
                collect_references(entry, references)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Install or upgrade one Helm owner and leave a failed revision available for inspection.
pub async fn upgrade(
    target: &crate::Target,
    chart: &std::path::Path,
    name: &str,
    values: &std::path::Path,
    timeout: std::time::Duration,
) -> anyhow::Result<()> {
    let timeout = format!("{}s", timeout.as_secs());
    run(&[
        "--kubeconfig".as_ref(),
        target.kubeconfig.as_os_str(),
        "--kube-context".as_ref(),
        target.context.as_ref(),
        "upgrade".as_ref(),
        "--install".as_ref(),
        "--create-namespace".as_ref(),
        "--namespace".as_ref(),
        target.namespace.as_ref(),
        name.as_ref(),
        chart.as_os_str(),
        "-f".as_ref(),
        values.as_os_str(),
        "--wait".as_ref(),
        "--timeout".as_ref(),
        timeout.as_ref(),
        "--history-max".as_ref(),
        "50".as_ref(),
    ])
    .await
    .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn references_use_rendered_config_and_require_external_secret_keys() -> anyhow::Result<()>
    {
        let env = json!({"env":[{"name":"TOKEN", "valueFrom":{"secretKeyRef":{"name":"credential","key":"token"}}}]});
        let env_from = json!({"envFrom":[{"secretRef":{"name":"credential"}}]});
        for (type_, container, internal_key, external_exists, external_key, succeeds) in [
            (
                "Deployment",
                &env,
                true,
                true,
                Some("Y3JlZGVudGlhbA=="),
                true,
            ),
            ("Deployment", &env, true, false, None, false),
            ("Deployment", &env, true, true, None, false),
            (
                "Deployment",
                &env,
                false,
                true,
                Some("Y3JlZGVudGlhbA=="),
                false,
            ),
            ("StatefulSet", &env_from, true, false, None, false),
            ("Deployment", &env, true, true, Some("!"), false),
        ] {
            let objects = [
                json!({"kind":type_, "metadata":{"namespace":"hosts"}, "spec":{"template":{"spec":{
                    "containers":[container],
                    "volumes":[{"configMap":{"name":"host-config", "items":[{"key":"config","path":"config"}]}}]
                }}}}),
                json!({"kind":"ConfigMap", "metadata":{"name":"host-config"},
                    "data": if internal_key { json!({"config":"rendered"}) } else { json!({}) }}),
            ];
            let result = require_references("hosts", &objects, async |namespace, type_, name| {
                assert_eq!((namespace, type_, name), ("hosts", "Secret", "credential"));
                Ok(external_exists.then(|| {
                    json!({"data":
                        if let Some(value) = external_key { json!({"token":value}) } else { json!({}) }
                    })
                }))
            })
            .await;
            assert_eq!(result.is_ok(), succeeds);
        }
        Ok(())
    }
}

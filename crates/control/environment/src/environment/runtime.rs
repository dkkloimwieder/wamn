//! Read application runtime inputs from the installed platform.

use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_release_control::release_chart::Target;

use super::{Platform, document::EnvironmentDocument};
use crate::platform::{artifact_plan, document::ClusterDocument, provider, resources};

/// Resolve the selected chart and shared host references without applying resources.
/// The caller keeps `directory` until the application operation ends.
///
/// # Errors
/// Returns an error for an absent cluster, selected chart, or required host input.
pub async fn resolve(
    platform: &mut Platform,
    document: &ClusterDocument,
    environment: &EnvironmentDocument,
    directory: &Path,
) -> anyhow::Result<()> {
    let kubeconfig = directory.join("application-kubeconfig");
    let context = provider::existing_kubeconfig(document, &kubeconfig).await?;
    let target = Target {
        kubeconfig,
        context,
        namespace: "hosts".into(),
    };
    platform.target = target;
    platform.release_artifact_base = format!("{}/wamn/releases", document.registry);
    platform.package_artifact_base = Some(format!("{}/wamn/packages", document.registry));
    platform.component_artifact_base = Some(format!("{}/wamn/components", document.registry));
    resolve_target(platform, environment, directory).await
}

pub(super) async fn resolve_target(
    platform: &mut Platform,
    environment: &EnvironmentDocument,
    directory: &Path,
) -> anyhow::Result<()> {
    let target = &platform.target;
    let org = &environment.org;
    let installed = resources::helm(
        target,
        &[
            "get".as_ref(),
            "values".as_ref(),
            "wamn".as_ref(),
            "-n".as_ref(),
            "platform".as_ref(),
            "-o".as_ref(),
            "json".as_ref(),
        ],
    )
    .await?;
    let installed: Value = serde_json::from_slice(&installed)?;
    let chart = artifact_plan::select(Path::new(
        installed["platform"]["chart"]
            .as_str()
            .context("installed platform has no selected chart")?,
    ))?;
    let control = resources::helm(
        target,
        &[
            "get".as_ref(),
            "values".as_ref(),
            format!("c-{org}").as_ref(),
            "-n".as_ref(),
            "hosts".as_ref(),
            "-o".as_ref(),
            "json".as_ref(),
        ],
    )
    .await?;
    let control: Value = serde_json::from_slice(&control)?;
    let local = control["runtime-operator"]["runtime"]["hostGroups"][0]["extraArgs"]
        .as_array()
        .is_some_and(|args| args.iter().any(|arg| arg == "--allow-insecure-registries"));
    let component_base = if local {
        let (_, repository) = platform
            .component_artifact_base
            .as_deref()
            .context("missing component registry")?
            .split_once('/')
            .context("component repository")?;
        format!("{}/{repository}", resources::POD_REGISTRY)
    } else {
        platform
            .component_artifact_base
            .as_ref()
            .context("missing component registry")?
            .to_owned()
    };
    let name = super::ensure::environment_host_name(
        &environment.org,
        &environment.project,
        &environment.env,
    )?;
    let shared = resources::helm(
        target,
        &[
            "get".as_ref(),
            "values".as_ref(),
            name.as_ref(),
            "-n".as_ref(),
            "hosts".as_ref(),
            "-o".as_ref(),
            "json".as_ref(),
        ],
    )
    .await?;
    let shared: Value = serde_json::from_slice(&shared)?;
    let group = host_group(&control, &shared, &component_base)?;
    let path = directory.join("application-host.yaml");
    std::fs::write(&path, serde_yaml::to_string(&group)?)?;
    platform.target.namespace = "hosts".into();
    chart.directory().clone_into(&mut platform.chart);
    platform.database_namespace = "platform".into();
    platform.host_group = Some(path);
    Ok(())
}

/// Keep the installed platform's shared runtime references without its control authority.
///
/// # Errors
/// Returns an error when identity or the installed control host is absent.
pub fn host_group(
    control: &Value,
    shared: &Value,
    component_base: &str,
) -> anyhow::Result<serde_yaml::Mapping> {
    let groups = control["runtime-operator"]["runtime"]["hostGroups"]
        .as_array()
        .context("installed platform has no control host")?;
    let [installed] = groups.as_slice() else {
        anyhow::bail!("installed platform requires one control host");
    };
    let variables = installed["env"]
        .as_array()
        .context("control host environment")?;
    let mut variables: Vec<_> = variables
        .iter()
        .filter(|variable| {
            matches!(
                variable["name"].as_str(),
                Some(
                    "DOCKER_CONFIG"
                        | "WAMN_REGISTRY_AUTH_FILE"
                        | "WAMN_REGISTRY_TOKEN_METADATA"
                        | "WAMN_SESSION_ISSUER"
                        | "WAMN_SESSION_JWKS_CA"
                        | "WAMN_OAUTH_ENCRYPTION"
                        | "WAMN_OAUTH_KEY_FILE"
                )
            )
        })
        .cloned()
        .collect();
    for name in [
        "WAMN_SESSION_ISSUER",
        "WAMN_SESSION_JWKS_CA",
        "WAMN_OAUTH_ENCRYPTION",
    ] {
        ensure!(
            variables.iter().any(|entry| entry["name"] == name
                && entry["value"]
                    .as_str()
                    .is_some_and(|value| !value.is_empty())),
            "installed platform lacks {name}"
        );
    }
    let shared = shared["runtime-operator"]["runtime"]["hostGroups"][0]["env"]
        .as_array()
        .context("provisioned environment host variables")?;
    for name in [
        "WAMN_EVT_ORG",
        "WAMN_EVT_PROJECT",
        "WAMN_EVT_ENV",
        "WAMN_EVT_STREAM_REPLICAS",
        "WAMN_EVT_DUP_WINDOW_SECS",
    ] {
        variables.push(
            shared
                .iter()
                .find(|entry| entry["name"] == name)
                .with_context(|| format!("provisioned environment lacks {name}"))?
                .clone(),
        );
    }
    variables.push(json!({"name":"WAMN_COMPONENT_ARTIFACT_BASE", "value":component_base}));
    let mut group = json!({"replicas":1, "http":{"enabled":true,"port":80}, "env":variables});
    for field in ["schedulerNatsUrl", "dataNatsUrl"] {
        ensure!(
            installed[field]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "installed platform lacks {field}"
        );
        group[field] = installed[field].clone();
    }
    for field in ["volumes", "volumeMounts"] {
        group[field] = installed[field]
            .as_array()
            .context("control host mounts")?
            .iter()
            .filter(|entry| {
                matches!(
                    entry["name"].as_str(),
                    Some("registry-pull" | "identity-ca" | "oauth-key")
                )
            })
            .cloned()
            .collect::<Vec<_>>()
            .into();
    }
    serde_yaml::from_value(serde_yaml::to_value(group)?).context("application host inputs")
}

/// Refuse missing required Secrets and ConfigMaps before Helm can change a workload.
pub(super) async fn require_references(target: &Target, objects: &[Value]) -> anyhow::Result<()> {
    let mut references = std::collections::BTreeMap::new();
    for object in objects {
        if object["kind"] != "Deployment" {
            continue;
        }
        collect_references(&object["spec"]["template"]["spec"], &mut references)?;
    }
    for ((kind, name), keys) in references {
        let object: Value = crate::kubectl::get(
            &target.kubeconfig,
            &target.context,
            &target.namespace,
            &kind,
            &name,
        )
        .await?
        .with_context(|| format!("missing required {kind} {}/{name}", target.namespace))?;
        for key in keys {
            ensure!(
                object["data"][&key]
                    .as_str()
                    .is_some_and(|value| !value.is_empty())
                    || object["binaryData"][&key]
                        .as_str()
                        .is_some_and(|value| !value.is_empty()),
                "required {kind} {}/{name} lacks {key}",
                target.namespace
            );
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
                if let Some((kind, name_field)) = reference {
                    if child["optional"] == true {
                        continue;
                    }
                    let name = child[name_field]
                        .as_str()
                        .context("runtime reference name")?;
                    let keys = references.entry((kind.into(), name.into())).or_default();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::write;
    use wamn_release_control::release_chart::{self, ImageSet};

    #[test]
    fn application_render_consumes_platform_references_without_control_credentials()
    -> anyhow::Result<()> {
        let private = crate::environment::ensure::PrivateDirectory::create()?;
        let mut control = crate::platform::control::host_values("example");
        let installed = &mut control["runtime-operator"]["runtime"]["hostGroups"][0];
        for entry in installed["env"].as_array_mut().unwrap() {
            if entry["name"] == "WAMN_SESSION_ISSUER" {
                entry["value"] = "https://selected-identity.identity.svc".into();
            }
        }
        let component_base = format!("{}/wamn/components", resources::POD_REGISTRY);
        let digest = format!("sha256:{}", "a".repeat(64));
        let document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
            "org":"example", "project":"orders", "env":"dev", "target":"cloud", "policy":"dev",
            "applications":{"orders":{"packages":["orders"],"current":digest,"hostname":"orders.example"}}
        }))?)?;
        let shared =
            crate::environment::ensure::materializer_host_values(&document, 70, 1, 120, None)?;
        let group = host_group(&control, &shared, &component_base)?;
        let path = private.path("host.yaml");
        std::fs::write(&path, serde_yaml::to_string(&group)?)?;
        let manifest = serde_json::from_value(json!({
            "format-version":wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
            "release":{"packages":[]}, "components":[], "routes":[], "attachments":{}
        }))?;
        let (values, _) = write::release_values(
            &document,
            "orders",
            &manifest,
            &digest,
            "localhost:5001/wamn/releases",
            70,
            "test",
            Some(&path),
            &std::collections::BTreeMap::new(),
        )?;
        let image = format!("registry.example/host:render@{digest}");
        let chart = release_chart::stamp(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/platform/release"),
            &ImageSet {
                host: image.clone(),
                http: image.clone(),
                materializer: image,
            },
            None,
            &private.path("chart"),
        )?;
        let metadata: Value = serde_yaml::from_slice(&std::fs::read(chart.join("Chart.yaml"))?)?;
        let objects = resources::helm_template(&json!({
            "chart":chart, "version":metadata["version"], "name":"orders",
            "namespace":"hosts", "values":values,
        }))?;
        let deployment = objects
            .iter()
            .find(|object| object["kind"] == "Deployment")
            .context("host deployment")?;
        assert_eq!(deployment["metadata"]["namespace"], "hosts");
        let pod = &deployment["spec"]["template"]["spec"];
        let host = &pod["containers"][0];
        let variables = host["env"].as_array().context("host environment")?;
        for variable in group["env"].as_sequence().unwrap() {
            let variable: Value = serde_json::to_value(variable)?;
            assert!(variables.contains(&variable), "{}", variable["name"]);
        }
        assert!(
            !variables
                .iter()
                .any(|variable| variable["name"] == "WAMN_CONTROL_URL")
        );
        let arguments = host["args"].as_array().context("host arguments")?;
        assert!(arguments.contains(&json!("--allow-insecure-registries")));
        assert!(arguments.contains(&json!(format!(
            "--release-artifact-base={}/wamn/releases",
            resources::POD_REGISTRY
        ))));
        let mut references = std::collections::BTreeMap::new();
        collect_references(pod, &mut references)?;
        for credential in wamn_control_provision::host_credentials::HOST_CREDENTIALS {
            if credential.host_variable.is_some() {
                assert!(references.contains_key(&(
                    "Secret".into(),
                    wamn_control_provision::workload_secret_name(
                        credential.family,
                        "example",
                        "orders",
                        "dev"
                    )
                )));
            }
        }
        assert!(
            references.contains_key(&("Secret".into(), "wamn-nats-example--orders--dev".into()))
        );
        assert!(!references.contains_key(&("Secret".into(), "operator-example".into())));
        assert!(
            objects
                .iter()
                .all(|object| !matches!(object["kind"].as_str(), Some("Secret" | "Certificate")))
        );
        Ok(())
    }
}

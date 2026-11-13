//! Read host runtime inputs from the installed platform.

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
    let control = resolve_platform(platform, &environment.org).await?;
    resolve_application(platform, environment, directory, &control).await
}

/// Read platform inputs before an environment or application exists.
pub async fn resolve_platform(platform: &mut Platform, org: &str) -> anyhow::Result<Value> {
    let target = &platform.target;
    crate::platform::edge::require_running(target).await?;
    let chart = if platform.chart.as_os_str().is_empty() {
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
        artifact_plan::select(Path::new(
            installed["platform"]["chart"]
                .as_str()
                .context("installed platform has no selected chart")?,
        ))?
    } else {
        artifact_plan::select(&platform.chart)?
    };
    let control = resolve_registry(platform, org).await?;
    platform.target.namespace = "hosts".into();
    chart.directory().clone_into(&mut platform.chart);
    platform.database_namespace = "platform".into();
    Ok(control)
}

/// Read registry transport from the installed control host without resolving runtime inputs.
///
/// # Errors
/// Returns an error when Helm values or the control host arguments are unavailable.
pub(super) async fn resolve_registry(platform: &mut Platform, org: &str) -> anyhow::Result<Value> {
    let control = resources::helm(
        &platform.target,
        &[
            "get".as_ref(),
            "values".as_ref(),
            wamn_registry::name::control_resource(org).as_ref(),
            "-n".as_ref(),
            "hosts".as_ref(),
            "-o".as_ref(),
            "json".as_ref(),
        ],
    )
    .await
    .context("read installed host registry configuration")?;
    let control: Value =
        serde_json::from_slice(&control).context("decode installed host registry configuration")?;
    let groups = control["runtime-operator"]["runtime"]["hostGroups"]
        .as_array()
        .context("installed host registry configuration has no host groups")?;
    let [group] = groups.as_slice() else {
        anyhow::bail!("installed host registry configuration requires one control host");
    };
    ensure!(group.is_object(), "installed control host is not an object");
    if let Some(args) = group.get("extraArgs") {
        ensure!(
            args.as_array()
                .is_some_and(|args| args.iter().all(Value::is_string)),
            "installed control host extraArgs must be an array of strings"
        );
    }
    platform.insecure_registry = insecure_registry(group);
    Ok(control)
}

/// The installed host arguments own the registry transport choice.
pub fn insecure_registry(group: &Value) -> bool {
    group["extraArgs"]
        .as_array()
        .is_some_and(|args| args.iter().any(|arg| arg == "--allow-insecure-registries"))
}

/// Borrow only the explicit image override of the named installed host group.
pub(crate) fn host_image<'a>(installed: Option<&'a Value>, name: &str) -> Option<&'a Value> {
    installed?
        .pointer("/runtime-operator/runtime/hostGroups")?
        .as_array()?
        .iter()
        .find(|group| group["name"] == name)?
        .get("image")
}

pub(super) fn pod_artifact_base(
    insecure_registry: bool,
    base: &str,
) -> anyhow::Result<std::borrow::Cow<'_, str>> {
    let (registry, repository) = base.split_once('/').context("artifact repository")?;
    let pod_registry = pod_registry(insecure_registry, registry);
    if pod_registry == registry {
        Ok(base.into())
    } else {
        Ok(format!("{pod_registry}/{repository}").into())
    }
}

pub(super) fn pod_registry(insecure_registry: bool, registry: &str) -> &str {
    if insecure_registry && registry == resources::LOCAL_REGISTRY {
        resources::POD_REGISTRY
    } else {
        registry
    }
}

pub(super) async fn resolve_application(
    platform: &mut Platform,
    environment: &EnvironmentDocument,
    directory: &Path,
    control: &Value,
) -> anyhow::Result<()> {
    let target = &platform.target;
    let component_base = pod_artifact_base(
        platform.insecure_registry,
        platform
            .component_artifact_base
            .as_deref()
            .context("missing component registry")?,
    )?;
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
    if let Some(events) = &mut platform.events {
        let variables = shared["runtime-operator"]["runtime"]["hostGroups"][0]["env"]
            .as_array()
            .context("provisioned environment host variables")?;
        let value = |name| {
            variables
                .iter()
                .find(|variable| variable["name"] == name)
                .and_then(|variable| variable["value"].as_str())
                .with_context(|| format!("provisioned environment lacks {name}"))
        };
        events.stream_replicas = value("WAMN_EVT_STREAM_REPLICAS")?.parse()?;
        events.dup_window_secs = value("WAMN_EVT_DUP_WINDOW_SECS")?.parse()?;
    }
    let group = host_group(control, Some(&shared), &component_base)?;
    let path = directory.join("application-host.yaml");
    std::fs::write(&path, serde_yaml::to_string(&group)?)?;
    platform.host_group = Some(path);
    Ok(())
}

/// Keep the installed platform's shared runtime references without its control authority.
/// Omit shared event settings before provisioning derives them from the environment.
///
/// # Errors
/// Returns an error when identity or the installed control host is absent.
pub fn host_group(
    control: &Value,
    shared: Option<&Value>,
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
    if let Some(shared) = shared {
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

/// Validate the same rendered inputs that Helm will install or upgrade.
pub(super) async fn require_chart_references(
    target: &Target,
    chart: &Path,
    name: &str,
    values: &impl serde::Serialize,
) -> anyhow::Result<()> {
    let metadata: Value = serde_yaml::from_slice(&std::fs::read(chart.join("Chart.yaml"))?)?;
    let objects = wamn_kubernetes::helm::template(&json!({
        "chart":chart, "version":metadata["version"], "name":name,
        "namespace":target.namespace, "values":values,
    }))?;
    require_references(
        &target.namespace,
        &objects,
        async |namespace, type_, name| {
            wamn_kubernetes::kubectl::get(
                &target.kubeconfig,
                &target.context,
                namespace,
                type_,
                name,
            )
            .await
        },
    )
    .await
}

/// Rendered objects satisfy internal references. Only external objects need a cluster read.
pub(crate) async fn require_references(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::write;
    use wamn_release_control::release_chart::{self, ImageSet};

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

    #[test]
    fn only_the_known_local_registry_is_rewritten_for_pods() {
        for (registry, expected) in [
            (resources::LOCAL_REGISTRY, resources::POD_REGISTRY),
            (resources::POD_REGISTRY, resources::POD_REGISTRY),
            ("registry.example:5001", "registry.example:5001"),
            ("localhost:5002", "localhost:5002"),
            ("127.0.0.1:5001", "127.0.0.1:5001"),
        ] {
            let base = format!("{registry}/wamn/components");
            assert_eq!(pod_artifact_base(false, &base).unwrap(), base);
            assert_eq!(
                pod_artifact_base(true, &base).unwrap(),
                format!("{expected}/wamn/components")
            );
        }
    }

    #[test]
    fn registry_transport_follows_the_installed_host_arguments() {
        assert!(!insecure_registry(&json!({})));
        assert!(!insecure_registry(
            &json!({"extraArgs":["--allow-insecure-registries=false"]})
        ));
        assert!(insecure_registry(
            &json!({"extraArgs":["--allow-insecure-registries"]})
        ));
    }

    #[tokio::test]
    async fn application_render_consumes_platform_references_without_control_credentials()
    -> anyhow::Result<()> {
        let private = crate::environment::ensure::PrivateDirectory::create()?;
        let cluster: ClusterDocument = serde_json::from_value(json!({
            "provider":"kind", "project_id":"", "zone":"", "cluster_name":"local",
            "org":"example", "owner_email":"owner@example.com", "registry":resources::LOCAL_REGISTRY,
            "buckets":{"web":"", "otel":"", "state":""},
            "route_hosts":{"control":"control.wamn.localhost"}, "resend_from":"owner@example.com"
        }))?;
        let auth_file = private.path("registry.json");
        std::fs::write(
            &auth_file,
            serde_json::to_vec(&json!({"auths":{
                resources::LOCAL_REGISTRY:{"username":"platform", "password":"credential"},
                "remote.example":{"username":"remote", "password":"remote-credential"}
            }}))?,
        )?;
        let registry_secret = crate::platform::control::registry_pull_secret(&cluster, &auth_file)?;
        let projected = private.path("projected-registry.json");
        std::fs::write(
            &projected,
            registry_secret["stringData"][".dockerconfigjson"]
                .as_str()
                .context("registry projection")?,
        )?;
        let credentials = wamn_package_artifact::registry_credentials::read_registry_credentials(
            &projected,
            resources::POD_REGISTRY,
        )?;
        assert_eq!(credentials.username(), "platform");
        assert_eq!(credentials.password(), "credential");
        assert!(
            wamn_package_artifact::registry_credentials::read_registry_credentials(
                &projected,
                "remote.example"
            )
            .is_ok()
        );
        let mut control = crate::platform::control::release_values(&cluster, &json!({}))?;
        let installed = &mut control["runtime-operator"]["runtime"]["hostGroups"][0];
        for entry in installed["env"].as_array_mut().unwrap() {
            if entry["name"] == "WAMN_SESSION_ISSUER" {
                entry["value"] = "https://selected-identity.identity.svc".into();
            }
        }
        let component_base = format!("{}/wamn/components", resources::POD_REGISTRY);
        let digest = format!("sha256:{}", "a".repeat(64));
        let document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
            "org":"example", "project":"orders", "env":"dev", "target":"cloud", "policy":"dev","revocation_seconds":60,
            "applications":{"orders":{"packages":["orders"],"current":digest,"revocation_seconds":5,"hostname":"orders.example"}}
        }))?)?;
        let shared = crate::environment::ensure::materializer_host_values(
            &document, 70, 1, 120, None, false, None,
        )?;
        let group = host_group(&control, Some(&shared), &component_base)?;
        let path = private.path("host.yaml");
        std::fs::write(&path, serde_yaml::to_string(&group)?)?;
        let manifest = serde_json::from_value(json!({
            "format-version":wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
            "release":{"packages":[]}, "components":[], "routes":[], "attachments":{}
        }))?;
        let mut cloud = cluster;
        cloud.provider = crate::platform::document::Provider::Gcp;
        cloud.project_id = "project-one".into();
        cloud.zone = "us-east1-b".into();
        let cloud_control = crate::platform::control::release_values(&cloud, &json!({}))?;
        let cloud_group = host_group(&cloud_control, Some(&shared), "remote.example/components")?;
        let cloud_path = private.path("cloud-host.yaml");
        std::fs::write(&cloud_path, serde_yaml::to_string(&cloud_group)?)?;
        let (cloud_values, _) = write::release_values(
            &document,
            "orders",
            &manifest,
            &digest,
            "remote.example/releases",
            70,
            "test",
            Some(&cloud_path),
            None,
        )?;
        let cloud_args = cloud_values["runtime-operator"]["runtime"]["hostGroups"][0]["extraArgs"]
            .as_sequence()
            .unwrap();
        assert!(cloud_args.contains(&serde_yaml::Value::String(
            "--registry-token-metadata".into()
        )));
        assert!(!cloud_args.iter().any(|arg| {
            arg.as_str()
                .is_some_and(|arg| arg.starts_with("--registry-auth-file="))
        }));
        // Local components do not redirect an independently published release.
        let remote = "registry.example/custom/releases";
        let (remote_values, _) = write::release_values(
            &document,
            "orders",
            &manifest,
            &digest,
            remote,
            70,
            "test",
            Some(&path),
            None,
        )?;
        assert!(
            remote_values["runtime-operator"]["runtime"]["hostGroups"][0]["extraArgs"]
                .as_sequence()
                .unwrap()
                .contains(&serde_yaml::Value::String(format!(
                    "--release-artifact-base={remote}"
                )))
        );
        let (values, _) = write::release_values(
            &document,
            "orders",
            &manifest,
            &digest,
            "localhost:5001/wamn/releases",
            70,
            "test",
            Some(&path),
            None,
        )?;
        let image = format!("registry.example/host:render@{digest}");
        let chart = release_chart::stamp(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/platform/release"),
            &ImageSet {
                host: image.clone(),
                http: image.clone(),
                materializer: image,
            },
            &private.path("chart"),
        )?;
        let metadata: Value = serde_yaml::from_slice(&std::fs::read(chart.join("Chart.yaml"))?)?;
        let objects = wamn_kubernetes::helm::template(&json!({
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
        let auth = variables
            .iter()
            .find(|entry| entry["name"] == "WAMN_REGISTRY_AUTH_FILE")
            .context("application registry credential")?["value"]
            .as_str()
            .unwrap();
        assert!(arguments.contains(&json!(format!("--registry-auth-file={auth}"))));
        assert!(!arguments.contains(&json!("--registry-token-metadata")));
        let mount = host["volumeMounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|mount| auth.starts_with(mount["mountPath"].as_str().unwrap()))
            .context("registry mount")?;
        assert_eq!(mount["readOnly"], true);
        let volume = pod["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|volume| volume["name"] == mount["name"])
            .context("registry volume")?;
        assert_eq!(
            volume["secret"]["secretName"],
            registry_secret["metadata"]["name"]
        );
        assert!(
            volume["secret"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["key"] == ".dockerconfigjson"
                    && auth.ends_with(item["path"].as_str().unwrap()))
        );
        assert!(arguments.contains(&json!("--allow-insecure-registries")));
        assert!(arguments.contains(&json!(format!(
            "--release-artifact-base={}/wamn/releases",
            resources::POD_REGISTRY
        ))));
        let mut references = std::collections::BTreeMap::new();
        collect_references(pod, &mut references)?;
        assert!(references.contains_key(&(
            "ConfigMap".into(),
            wamn_control_provision::project_env_secret_name("example", "orders", "dev")
        )));
        let host = pod["containers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|container| container["name"] == "host")
            .unwrap();
        let postgres = host["env"]
            .as_array()
            .unwrap()
            .iter()
            .find(|variable| variable["name"] == "WAMN_PG_URL")
            .unwrap();
        assert_eq!(postgres["value"], "/etc/wamn/postgres/app.json");
        assert!(!references.contains_key(&("Secret".into(), "operator-example".into())));
        assert!(
            objects
                .iter()
                .all(|object| !matches!(object["kind"].as_str(), Some("Secret" | "Certificate")))
        );
        // Only references absent from the real chart need a cluster read.
        let internal = objects
            .iter()
            .filter_map(|object| {
                let type_ = object["kind"].as_str()?;
                let name = object["metadata"]["name"].as_str()?;
                references
                    .contains_key(&(type_.to_owned(), name.to_owned()))
                    .then_some((type_, name))
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert!(internal.iter().any(|(type_, _)| *type_ == "ConfigMap"));
        require_references("hosts", &objects, async |namespace, type_, name| {
            assert_eq!(namespace, "hosts");
            assert!(!internal.contains(&(type_, name)), "{namespace}/{name}");
            if type_ == "Secret" && registry_secret["metadata"]["name"] == name {
                return Ok(Some(registry_secret.clone()));
            }
            let data: serde_json::Map<_, _> = references[&(type_.to_owned(), name.to_owned())]
                .iter()
                .map(|key| (key.to_owned(), json!("Y3JlZGVudGlhbA==")))
                .collect();
            Ok(Some(json!({"data":data})))
        })
        .await?;
        Ok(())
    }
}

//! Read host runtime inputs from the installed platform.

use std::path::Path;

use anyhow::Context as _;
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
        namespace: environment.org.clone(),
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
    crate::platform::edge::require_running(&platform.target).await?;
    let installed = crate::platform::installed::InstalledPlatform::read(&platform.target)
        .await?
        .context("platform is not installed")?;
    let reference = installed.values["platform"]["chart"]
        .as_str()
        .context("installed platform has no selected chart")?;
    let chart = if platform.chart.as_os_str().is_empty() {
        artifact_plan::select(Path::new(reference))?
    } else {
        artifact_plan::select(&platform.chart)?
    };
    chart.directory().clone_into(&mut platform.chart);
    if chart.is_downloaded() {
        platform.chart_download = Some(std::sync::Arc::new(chart));
    }
    platform.target.namespace = org.into();
    platform.database_namespace = org.into();
    apply_registry_configuration(platform, &installed.values["configuration"])?;
    Ok(installed.values)
}

pub(super) async fn resolve_registry(platform: &mut Platform, org: &str) -> anyhow::Result<Value> {
    let installed = crate::platform::installed::InstalledPlatform::read(&platform.target)
        .await?
        .context("platform is not installed")?;
    platform.target.namespace = org.into();
    platform.database_namespace = org.into();
    apply_registry_configuration(platform, &installed.values["configuration"])?;
    Ok(installed.values)
}

fn apply_registry_configuration(
    platform: &mut Platform,
    configuration: &Value,
) -> anyhow::Result<()> {
    platform.insecure_registry = configuration["insecure_registry"]
        .as_bool()
        .context("registry transport is absent")?;
    platform.release_artifact_base = configuration["release_base"]
        .as_str()
        .context("release registry is absent")?
        .into();
    platform.package_artifact_base = Some(
        configuration["package_base"]
            .as_str()
            .context("package registry is absent")?
            .into(),
    );
    platform.component_artifact_base = Some(
        configuration["component_base"]
            .as_str()
            .context("component registry is absent")?
            .into(),
    );
    platform.release_artifact_base =
        pod_artifact_base(platform.insecure_registry, &platform.release_artifact_base)?
            .into_owned();
    if let Some(base) = &platform.package_artifact_base {
        platform.package_artifact_base =
            Some(pod_artifact_base(platform.insecure_registry, base)?.into_owned());
    }
    if let Some(base) = &platform.component_artifact_base {
        platform.component_artifact_base =
            Some(pod_artifact_base(platform.insecure_registry, base)?.into_owned());
    }
    Ok(())
}

/// The installed host arguments own the registry transport choice.
pub fn insecure_registry(group: &Value) -> bool {
    group["extraArgs"]
        .as_array()
        .is_some_and(|args| args.iter().any(|arg| arg == "--allow-insecure-registries"))
}

/// Borrow only the explicit image override of the named installed host group.
pub(crate) use crate::platform::installed::host_image;

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
    let name = super::ensure::environment_host_name(&environment.org, &environment.env)?;
    let shared = resources::helm(
        target,
        &[
            "get".as_ref(),
            "values".as_ref(),
            name.as_ref(),
            "-n".as_ref(),
            target.namespace.as_ref(),
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
    let _ = shared;
    let mut group = control["hostConfiguration"].clone();
    group["env"] = json!([{"name":"WAMN_COMPONENT_ARTIFACT_BASE","value":component_base}]);
    serde_yaml::from_value(serde_yaml::to_value(group)?)
        .context("installed shared host configuration")
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
pub(crate) use wamn_kubernetes::helm::require_references;

#[cfg(test)]
mod tests {
    use super::*;

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
}

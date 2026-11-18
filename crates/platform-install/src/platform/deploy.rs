//! Deploy platform resources from their selected, prepared charts.

use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use wamn_kubernetes::{Target, helm, kubectl};

use super::artifact_plan::{self, SelectedChart};
use super::document::{ClusterDocument, UpgradeTarget};
use super::{control, identity, installation, installed::InstalledPlatform, resources};
use serde_json::Value;
use wamn_catalog::lifecycle_lock::LifecycleLock;

/// Prepare an update of one existing owner without entering installation.
///
/// # Errors
/// Returns an error for a missing owner, host group, chart, or required input.
pub async fn update(
    document: &ClusterDocument,
    chart: &SelectedChart,
    target: &Target,
    selection: &UpgradeTarget,
    directory: &Path,
    registry: &str,
) -> anyhow::Result<()> {
    if let UpgradeTarget::Host { group, .. } = selection {
        let (name, namespace) = selection.owner(document);
        let release_namespace = helm::org_release_namespace(namespace);
        let installed = installed_values(target, &release_namespace, &name).await?;
        let inspected = chart.selection_values(document, selection, installed)?;
        let coordinate = host_target(&inspected, group)?;
        let operator = super::installed::release_values(target, "platform", "wamn")
            .await?
            .context("host update requires existing release platform/wamn")?;
        let owner_chart = artifact_plan::platform_chart(Some(&operator), None)?;
        anyhow::ensure!(coordinate.0 == document.org, "host belongs to another org");
        let lock = LifecycleLock::acquire_environment(
            wamn_postgres::connect(registry).await?,
            &coordinate.0,
            &coordinate.1,
        )
        .await?;
        let result = lock.protect(async {
            helm::recover_pending(target, &release_namespace, &name, helm::Wait::Watcher).await?;
            let installed = installed_values(target, &release_namespace, &name).await?;
            anyhow::ensure!(
                host_target(&installed, group)? == coordinate,
                "recovered release changed its environment owner"
            );
            let values = chart.selection_values(document, selection, installed)?;
            let entry = serde_json::json!({"chart":owner_chart.directory().join("platform/environment"), "version":owner_chart.version,
                "name":name, "namespace":release_namespace, "values":values});
            resources::upgrade_release(target, &entry, directory, helm::Wait::Watcher, helm::Install::SkipCrds).await
        }).await;
        lock.release().await;
        return result;
    }
    let installed = InstalledPlatform::read(target)
        .await?
        .context("platform update requires an installed platform")?;
    let mut values = installed.values;
    let (name, namespace) = selection.owner(document);
    let (key, mut component) = match selection {
        UpgradeTarget::Control => (
            "control",
            control::resource_entry(document, chart, Some(&values["control"]))?["values"].take(),
        ),
        UpgradeTarget::Identity => {
            identity::require_keys(registry, &document.org, Duration::from_secs(10)).await?;
            ("identity", identity::resource_entry(document, chart)?["values"].take())
        }
        UpgradeTarget::Host { .. } => unreachable!(),
    };
    component["enabled"] = true.into();
    component["name"] = name.as_ref().into();
    component["namespace"] = namespace.into();
    values[key] = component;
    let entry = installation::platform_entry(chart, values)?;
    resources::apply_release(target, &entry, directory, helm::Wait::None).await?;
    let deployment = Target {
        namespace: namespace.into(),
        ..target.clone()
    };
    kubectl::wait_deployment_ready(&deployment, &name, Duration::from_secs(600)).await
}

fn host_target(
    values: &Value,
    name: &str,
) -> anyhow::Result<(wamn_registry::OrgId, wamn_registry::EnvName)> {
    let group = values["runtime-operator"]["runtime"]["hostGroups"]
        .as_array()
        .context("installed release has no host groups")?
        .iter()
        .find(|entry| entry["name"] == name)
        .context("selected host group is absent")?;
    let org = group["env"]
        .as_array()
        .context("host has no environment variables")?
        .iter()
        .find(|variable| variable["name"] == "WAMN_ORG")
        .and_then(|variable| variable["value"].as_str())
        .context("host has no org")?
        .parse::<wamn_registry::OrgId>()?;
    let env = group["extraArgs"]
        .as_array()
        .context("host has no arguments")?
        .iter()
        .filter_map(Value::as_str)
        .find_map(|argument| argument.strip_prefix("--environment="))
        .context("host has no environment")?
        .parse::<wamn_registry::EnvName>()?;
    anyhow::ensure!(
        wamn_registry::name::environment_host_name(&org, &env) == name,
        "host group differs from its environment owner"
    );
    Ok((org, env))
}

async fn installed_values(target: &Target, namespace: &str, name: &str) -> anyhow::Result<Value> {
    super::installed::release_values(target, namespace, name)
        .await?
        .context("installed host owner is absent")
}

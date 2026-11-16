//! Deploy platform resources from their selected, prepared charts.

use std::path::Path;

use anyhow::Context as _;
use wamn_kubernetes::Target;

use super::artifact_plan::{self, SelectedChart};
use super::document::{ClusterDocument, UpgradeTarget};
use super::{control, identity, installation, installed::InstalledPlatform, resources};
use serde_json::Value;
use wamn_catalog::lifecycle_lock::LifecycleLock;
use wamn_kubernetes::helm;

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
        let owner = Target {
            namespace: namespace.into(),
            ..target.clone()
        };
        let installed = super::installed::release_values(&owner, &owner.namespace, &name)
            .await?
            .context("host owner is absent")?;
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
            helm::recover_pending(&owner, &name).await?;
            let installed = installed_values(&owner, &name).await?;
            anyhow::ensure!(
                host_target(&installed, group)? == coordinate,
                "recovered release changed its environment owner"
            );
            let values = chart.selection_values(document, selection, installed)?;
            let entry = serde_json::json!({"chart":owner_chart.directory().join("platform/environment"), "version":owner_chart.version,
                "name":name, "namespace":namespace, "values":values});
            resources::require_inputs(target, &entry).await?;
            resources::upgrade_release(&owner, &entry, directory).await
        }).await;
        lock.release().await;
        return result;
    }
    let installed = InstalledPlatform::read(target)
        .await?
        .context("platform update requires an installed platform")?;
    let mut values = installed.values;
    match selection {
        UpgradeTarget::Control => {
            values["control"] = control::resource_entry(document, chart, Some(&values["control"]))?
                ["values"]
                .take();
            values["control"]["enabled"] = true.into();
            values["control"]["name"] = wamn_registry::name::control_resource(&document.org).into();
            values["control"]["namespace"] = document.org.as_str().into();
        }
        UpgradeTarget::Identity => {
            identity::require_keys(registry, &document.org, std::time::Duration::from_secs(10))
                .await?;
            values["identity"] = identity::resource_entry(document, chart)?["values"].take();
            values["identity"]["enabled"] = true.into();
            values["identity"]["name"] = "identity".into();
            values["identity"]["namespace"] = document.org.as_str().into();
        }
        UpgradeTarget::Host { .. } => unreachable!(),
    }
    let entry = installation::platform_entry(chart, values)?;
    resources::require_inputs(target, &entry).await?;
    resources::apply_release(target, &entry, directory).await
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

async fn installed_values(target: &Target, name: &str) -> anyhow::Result<Value> {
    super::installed::release_values(target, &target.namespace, name)
        .await?
        .context("installed host owner is absent")
}

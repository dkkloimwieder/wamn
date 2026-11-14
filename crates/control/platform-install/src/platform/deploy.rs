//! Deploy platform resources from their selected, prepared charts.

use std::io::Write;
use std::path::Path;

use anyhow::Context as _;
use wamn_release_control::release_chart::Target;

use super::artifact_plan::{self, SelectedChart};
use super::document::{ClusterDocument, UpgradeTarget};
use super::{
    bootstrap, control, identity, installation, installed::InstalledPlatform, provider, resources,
};
use crate::lifecycle_lock::LifecycleLock;
use serde_json::Value;
use wamn_release_control::release_chart;

/// Apply platform resources and initialize an installation without a control release.
///
/// # Errors
/// Returns resource, bootstrap, service, or endpoint failures.
pub async fn deploy(
    document: &ClusterDocument,
    chart: Option<&Path>,
    directory: &Path,
    output: &mut impl Write,
) -> anyhow::Result<()> {
    let kubeconfig = directory.join("kubeconfig");
    let context = provider::available_kubeconfig(document, &kubeconfig).await?;
    let existing = context.is_some();
    let mut target = Target {
        kubeconfig,
        context: context.unwrap_or_default(),
        namespace: document.org.clone(),
    };
    let installed = if existing {
        InstalledPlatform::read(&target).await?
    } else {
        None
    };
    let operator_values = installed.as_ref().map(|installed| &installed.values);
    let platform_chart = artifact_plan::platform_chart(operator_values, chart)?;
    let control_values = installed
        .as_ref()
        .map(InstalledPlatform::control)
        .filter(|value| value["enabled"] == true);
    anyhow::ensure!(
        control_values.is_none(),
        "existing platforms update through authenticated control"
    );
    let identity_values = installed
        .as_ref()
        .map(InstalledPlatform::identity)
        .filter(|value| value["enabled"] == true);
    let control_chart = artifact_plan::resolve_chart(
        document,
        &UpgradeTarget::Control,
        operator_values,
        control_values,
        identity_values,
        platform_chart.directory(),
    )?;
    let identity_chart = artifact_plan::resolve_chart(
        document,
        &UpgradeTarget::Identity,
        operator_values,
        control_values,
        identity_values,
        platform_chart.directory(),
    )?;
    writeln!(
        output,
        "platform default chart: {}",
        platform_chart.directory().display()
    )?;
    writeln!(
        output,
        "control chart: {}",
        control_chart.directory().display()
    )?;
    writeln!(
        output,
        "identity chart: {}",
        identity_chart.directory().display()
    )?;
    output.flush()?;
    target.context = document
        .provider
        .kubeconfig(document, &target.kubeconfig)
        .await?;
    let target = &target;
    let chart = &platform_chart;
    anyhow::ensure!(
        chart.reference().starts_with("oci://"),
        "platform installation requires a published chart digest"
    );
    installation::infrastructure(target, document, chart, directory).await?;
    // Broker credentials are explicit and never enter Helm values.
    let users =
        wamn_broker::authorization::update_authorization(target, &document.org, None).await?;
    let mut values = installation::values(document, chart).await?;
    resources::merge(&mut values, crate::environment::broker_certificates(users));
    if let Some(installed) = &installed {
        for component in ["control", "identity", "edge"] {
            values[component] = installed.values[component].clone();
        }
    }
    installation::apply(target, chart, &mut values, directory).await?;
    if document.provider == super::document::Provider::Kind && control_values.is_none() {
        let auth_file = std::env::var_os("WAMN_REGISTRY_AUTH_FILE")
            .context("set WAMN_REGISTRY_AUTH_FILE for the platform registry credential")?;
        resources::apply_object(
            target,
            &control::registry_pull_secret(document, Path::new(&auth_file))?,
        )
        .await?;
    }
    let org_name = wamn_registry::name::resource_suffix("org", &document.org);
    let org_values = resources::release_values(target, &document.org, &org_name).await?;
    let mut entry = installation::organization_entry(document, chart, org_values)?;
    let declarations = crate::roles::platform(&document.org, "wamn-pg", "wamn_system")?;
    crate::credentials::prepare(target, &declarations).await?;
    entry["values"]["managedRoles"] = crate::roles::values(&declarations)?;
    resources::apply_release(target, &entry, directory).await?;
    wamn_kubernetes::kubectl::wait(
        &target.kubeconfig,
        &target.context,
        &document.org,
        "cluster.postgresql.cnpg.io",
        "wamn-pg",
        "condition=Ready",
        "600s",
    )
    .await?;
    if control_values.is_none() {
        initialize(document, chart, target, directory, output).await?;
    }
    identity::mail(target, document).await?;
    values["identity"] = identity::resource_entry(document, &identity_chart)?["values"].take();
    values["identity"]["enabled"] = true.into();
    values["identity"]["name"] = "identity".into();
    values["identity"]["namespace"] = document.org.as_str().into();
    installation::apply(target, chart, &mut values, directory).await?;
    if control_values.is_none() {
        identity::initialize(target, document, &identity_chart, directory).await?;
        control::initialize(target, document).await?;
    }
    values["control"] =
        control::resource_entry(document, &control_chart, control_values)?["values"].take();
    values["control"]["enabled"] = true.into();
    values["control"]["name"] = wamn_registry::name::control_resource(&document.org).into();
    values["control"]["namespace"] = document.org.as_str().into();
    values["edge"] = super::edge::chart_input(
        &chart.directory().join("platform/edge"),
        chart.image("edge")?,
        document,
    )?["values"]
        .take();
    values["edge"]["enabled"] = true.into();
    values["edge"]["name"] = "wamn-edge".into();
    values["edge"]["namespace"] = "hosts".into();
    resources::apply_release(
        target,
        &installation::platform_entry(chart, values)?,
        directory,
    )
    .await?;
    control::probe(target, document).await
}

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
        let lock = LifecycleLock::acquire_environment(registry, coordinate.0, coordinate.1).await?;
        let result = lock.protect(async {
            release_chart::recover_pending(&owner, &name).await?;
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

async fn initialize(
    document: &ClusterDocument,
    chart: &SelectedChart,
    target: &Target,
    directory: &Path,
    output: &mut impl Write,
) -> anyhow::Result<()> {
    let initialization = bootstrap::initialize(target, document, chart, directory).await;
    let public_result = directory.join("jobs/platform-system.log");
    let printed = (|| -> anyhow::Result<()> {
        if public_result.exists() {
            writeln!(output, "{}", std::fs::read_to_string(public_result)?)?;
        }
        Ok(())
    })();
    initialization?;
    printed?;
    Ok(())
}

fn host_target<'a>(values: &'a Value, name: &str) -> anyhow::Result<(&'a str, &'a str)> {
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
        .context("host has no org")?;
    let env = group["extraArgs"]
        .as_array()
        .context("host has no arguments")?
        .iter()
        .filter_map(Value::as_str)
        .find_map(|argument| argument.strip_prefix("--environment="))
        .context("host has no environment")?;
    anyhow::ensure!(
        wamn_registry::name::environment_host_name(org, env)? == name,
        "host group differs from its environment owner"
    );
    Ok((org, env))
}

async fn installed_values(target: &Target, name: &str) -> anyhow::Result<Value> {
    super::installed::release_values(target, &target.namespace, name)
        .await?
        .context("installed host owner is absent")
}

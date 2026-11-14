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
    let declarations = if control_values.is_some() {
        let forward =
            super::database::forward(target, &document.org, "wamn-pg", "wamn_system").await?;
        crate::roles::registered(&forward.input, &document.org).await?
    } else {
        crate::roles::platform(&document.org, "wamn-pg", "wamn_system")?
    };
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
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !matches!(selection, UpgradeTarget::Host { .. }),
        "host updates run through the authenticated control service"
    );
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
            identity::require_keys(target, std::time::Duration::from_secs(10)).await?;
            values["identity"] = identity::resource_entry(document, chart)?["values"].take();
            values["identity"]["enabled"] = true.into();
            values["identity"]["name"] = "identity".into();
            values["identity"]["namespace"] = document.org.as_str().into();
        }
        UpgradeTarget::Host { .. } => unreachable!("host updates use control"),
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

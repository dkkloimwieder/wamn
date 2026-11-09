//! Deploy platform resources from one selected, prepared chart.

use std::io::Write;
use std::path::Path;

use anyhow::Context as _;
use serde::Deserialize as _;
use serde_json::Value;
use wamn_release_control::release_chart::Target;

use super::artifact_plan::{self, SelectedChart};
use super::document::{ClusterDocument, Provider, UpgradeTarget};
use super::{bootstrap, broker, control, edge, identity, manifests, provider, resources};
use crate::kubectl;

/// Apply platform resources, initialize the system, and start identity and control.
///
/// # Errors
/// Returns resource, bootstrap, service, or endpoint failures.
pub async fn deploy(
    document: &ClusterDocument,
    chart: &SelectedChart,
    target: &Target,
    directory: &Path,
    output: &mut impl Write,
) -> anyhow::Result<()> {
    // Resolve both recorded selections before any resource mutation.
    let operator_values = resources::release_values(target, "platform", "wamn").await?;
    let control_values =
        resources::release_values(target, "hosts", &format!("c-{}", document.org)).await?;
    let identity_values = resources::release_values(target, "identity", "identity").await?;
    let control_chart = artifact_plan::resolve_chart(
        document,
        &UpgradeTarget::Control,
        operator_values.as_ref(),
        control_values.as_ref(),
        identity_values.as_ref(),
        chart.directory(),
    )?;
    let identity_chart = artifact_plan::resolve_chart(
        document,
        &UpgradeTarget::Identity,
        operator_values.as_ref(),
        control_values.as_ref(),
        identity_values.as_ref(),
        chart.directory(),
    )?;
    infrastructure(document, chart, target, directory).await?;
    initialize(document, chart, target, directory, output).await?;
    identity::apply(target, document, &identity_chart, directory).await?;
    control::credentials(target, document).await?;
    control::release(
        target,
        document,
        &control_chart,
        control_values.as_ref(),
        directory,
    )
    .await?;
    let payload = chart.directory().join("platform/resources");
    let certificates = edge::certificates(&payload, document)?;
    apply(target, &certificates).await?;
    certificates_ready(target, &certificates).await?;
    edge::apply(target, document, chart, directory).await?;
    control::probe(target, document).await
}

/// Prepare an update of one existing owner without entering installation.
///
/// # Errors
/// Returns an error for an absent owner or an update that needs DEP4-03 or DEP4-04.
pub async fn update(
    document: &ClusterDocument,
    chart: &SelectedChart,
    target: &Target,
    selection: &UpgradeTarget,
) -> anyhow::Result<()> {
    let (name, namespace) = selection.owner(document);
    let installed = resources::release_values(target, namespace, &name)
        .await?
        .with_context(|| {
            format!(
                "targeted update requires existing release {namespace}/{name}; no resources changed"
            )
        })?;
    if matches!(selection, UpgradeTarget::Host { .. }) {
        chart.selection_values(document, selection, installed)?;
    } else {
        let operator = resources::release_values(target, "platform", "wamn").await?;
        artifact_plan::resolve_chart(
            document,
            selection,
            operator.as_ref(),
            (selection == &UpgradeTarget::Control).then_some(&installed),
            (selection == &UpgradeTarget::Identity).then_some(&installed),
            chart.directory(),
        )?;
    }
    match selection {
        UpgradeTarget::Control => anyhow::bail!(
            "control update requires DEP4-03 (wamn-275sr.3) to separate resources from credential initialization; no resources changed"
        ),
        UpgradeTarget::Identity => anyhow::bail!(
            "identity update requires DEP4-03 (wamn-275sr.3) to separate resources from SQL and signing-key initialization; no resources changed"
        ),
        UpgradeTarget::Host { .. } => anyhow::bail!(
            "host update requires DEP4-04 (wamn-275sr.4) to preserve the owning release and host selections; no resources changed"
        ),
    }
}

async fn infrastructure(
    document: &ClusterDocument,
    chart: &SelectedChart,
    target: &Target,
    directory: &Path,
) -> anyhow::Result<()> {
    let payload = chart.directory().join("platform/resources");
    for object in resources::runtime_namespaces() {
        resources::apply_object(target, &object).await?;
    }
    if document.provider == Provider::Kind {
        let registry = provider::ensure_registry(document).await?;
        for object in resources::registry_objects(&payload, &registry)? {
            resources::apply_object(target, &object).await?;
        }
    }
    for manifest in ["cert-manager.yaml", "cnpg-operator.yaml"] {
        let objects = manifests::prerequisite_objects(&payload, manifest, document.provider)?;
        apply(target, &objects).await?;
        // Derive the served APIs and controllers from the selected payload.
        for object in &objects {
            let name = object["metadata"]["name"]
                .as_str()
                .context("resource name")?;
            let namespace = object["metadata"]["namespace"]
                .as_str()
                .unwrap_or("platform");
            match object["kind"].as_str() {
                Some("CustomResourceDefinition") => {
                    kubectl::wait(
                        &target.kubeconfig,
                        &target.context,
                        namespace,
                        "crd",
                        name,
                        "condition=Established",
                        "120s",
                    )
                    .await?;
                }
                Some("Deployment") => {
                    rollout(target, namespace, &format!("deployment/{name}")).await?;
                }
                _ => {}
            }
        }
    }
    for manifest in [
        "nats-certificates.yaml",
        "identity-certificate.yaml",
        "identity-operator-ca.yaml",
    ] {
        let objects = manifests::objects(&payload, manifest, document)?;
        apply(target, &objects).await?;
        certificates_ready(target, &objects).await?;
    }
    let live: Option<Value> = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        broker::AUTHORIZATION,
    )
    .await?;
    let users = broker::platform_objects(live.as_ref(), &document.org)?;
    apply(target, &users).await?;
    certificates_ready(target, &users).await?;
    apply(
        target,
        &manifests::objects(&payload, "nats-jetstream.yaml", document)?,
    )
    .await?;
    rollout(target, "platform", "statefulset/wamn-nats").await?;

    let mut values = resources::operator_payload_values(&payload, document)?;
    // Helm owns the selected chart reference as an ordinary release value.
    values["platform"] = serde_json::json!({"chart": chart.directory()});
    let values_path = directory.join("values-operator.yaml");
    std::fs::write(&values_path, serde_yaml::to_string(&values)?)
        .context("write operator values")?;
    resources::helm(
        target,
        &[
            "upgrade".as_ref(),
            "--install".as_ref(),
            "wamn".as_ref(),
            "--namespace".as_ref(),
            "platform".as_ref(),
            chart.operator_archive.as_os_str(),
            "-f".as_ref(),
            values_path.as_os_str(),
            "--wait".as_ref(),
            "--timeout".as_ref(),
            "5m".as_ref(),
        ],
    )
    .await?;
    // Helm's wait covers its Deployment; establish its CRDs before future host releases.
    let crds = resources::helm(
        target,
        &[
            "show".as_ref(),
            "crds".as_ref(),
            chart.operator_archive.as_os_str(),
        ],
    )
    .await?;
    for yaml in serde_yaml::Deserializer::from_slice(&crds) {
        let object = Value::deserialize(yaml).context("decode operator CRD")?;
        if object["kind"] == "CustomResourceDefinition" {
            let name = object["metadata"]["name"]
                .as_str()
                .context("operator CRD name")?;
            kubectl::wait(
                &target.kubeconfig,
                &target.context,
                "platform",
                "crd",
                name,
                "condition=Established",
                "120s",
            )
            .await?;
        }
    }
    for manifest in [
        "runtime-operator-events-rbac.example.yaml",
        "wamn-host-serviceaccount.yaml",
        "control-status-rbac.yaml",
        "registry-helper.yaml",
    ] {
        apply(target, &manifests::objects(&payload, manifest, document)?).await?;
    }
    apply(
        target,
        &manifests::objects(&payload, "cnpg-cluster.yaml", document)?,
    )
    .await?;
    kubectl::wait(
        &target.kubeconfig,
        &target.context,
        "platform",
        "cluster.postgresql.cnpg.io",
        "wamn-pg",
        "condition=Ready",
        "600s",
    )
    .await?;
    Ok(())
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

async fn apply(target: &Target, objects: &[Value]) -> anyhow::Result<()> {
    for object in objects {
        resources::apply_object(target, object).await?;
    }
    Ok(())
}

async fn certificates_ready(target: &Target, objects: &[Value]) -> anyhow::Result<()> {
    for object in objects {
        if !matches!(
            object["kind"].as_str(),
            Some("Certificate" | "Issuer" | "ClusterIssuer")
        ) {
            continue;
        }
        kubectl::wait(
            &target.kubeconfig,
            &target.context,
            object["metadata"]["namespace"]
                .as_str()
                .unwrap_or("platform"),
            object["kind"].as_str().context("certificate kind")?,
            object["metadata"]["name"]
                .as_str()
                .context("certificate name")?,
            "condition=Ready",
            "120s",
        )
        .await?;
    }
    Ok(())
}

async fn rollout(target: &Target, namespace: &str, resource: &str) -> anyhow::Result<()> {
    kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        namespace,
        &["rollout", "status", resource, "--timeout=300s"],
        None,
    )
    .await?;
    Ok(())
}

//! Deploy platform resources from one selected, prepared chart.

use std::io::Write;
use std::path::Path;

use anyhow::Context as _;
use serde::Deserialize as _;
use serde_json::Value;
use wamn_release_control::release_chart::Target;

use super::artifact_plan::SelectedChart;
use super::document::{ClusterDocument, Provider};
use super::{bootstrap, broker, manifests, provider, resources};
use crate::kubectl;

/// Apply platform prerequisites through their Kubernetes and Helm owners.
///
/// # Errors
/// Returns resource or bootstrap failures, or an incomplete result until services exist.
pub async fn deploy(
    document: &ClusterDocument,
    chart: &SelectedChart,
    target: &Target,
    directory: &Path,
    output: &mut impl Write,
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
    let initialization = bootstrap::initialize(target, document, chart, directory).await;
    let public_result = directory.join("jobs/platform-system.log");
    if public_result.exists() {
        writeln!(output, "{}", std::fs::read_to_string(public_result)?)?;
    }
    initialization?;
    writeln!(
        output,
        "incomplete: platform resources and system initialization are ready. Services are not implemented"
    )?;
    anyhow::bail!("incomplete platform deployment: services are not implemented")
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

//! Platform edge installation and application route prerequisites.

use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_release_control::release_chart::Target;

use super::{
    artifact_plan::SelectedChart,
    document::{ClusterDocument, Provider},
    manifests, resources,
};
use wamn_kubernetes::kubectl;

/// Render the platform's existing edge certificate in the hosts namespace.
pub fn certificates(payload: &Path, document: &ClusterDocument) -> anyhow::Result<Vec<Value>> {
    manifests::objects(payload, "letsencrypt.yaml", document)
}

/// Select one platform edge, with no application route list.
pub fn chart_input(chart: &Path, image: &str, document: &ClusterDocument) -> anyhow::Result<Value> {
    let bucket = &document.buckets.web;
    Ok(json!({
        "chart":chart,
        "version":wamn_release_control::release_chart::read_platform_version(&chart.join("Chart.yaml"))?,
        "name":"wamn-edge", "namespace":"hosts",
        "values":{
            "neg":document.provider == Provider::Gcp,
            "tlsSecret":"wamn-edge-tls", "identityCaConfigMap":"identity-ca",
            "identity":&super::identity::issuer(&document.org),
            "web":{"image":image, "bucket":if bucket.is_empty() { String::new() } else { format!("gs://{bucket}") }},
        }
    }))
}

pub(super) async fn apply(
    target: &Target,
    document: &ClusterDocument,
    chart: &SelectedChart,
    directory: &Path,
) -> anyhow::Result<()> {
    let entry = chart_input(
        &chart.directory().join("platform/edge"),
        chart.image("edge")?,
        document,
    )?;
    let values = directory.join("values-edge.yaml");
    std::fs::write(&values, serde_yaml::to_string(&entry["values"])?)?;
    resources::helm(
        target,
        &[
            "upgrade".as_ref(),
            "--install".as_ref(),
            "wamn-edge".as_ref(),
            entry["chart"].as_str().context("edge chart")?.as_ref(),
            "--namespace".as_ref(),
            "hosts".as_ref(),
            "-f".as_ref(),
            values.as_os_str(),
            "--wait".as_ref(),
            "--timeout".as_ref(),
            "5m".as_ref(),
        ],
    )
    .await?;
    require_running(target).await
}

/// Refuse application deployment when the platform edge has no ready endpoint.
pub async fn require_running(target: &Target) -> anyhow::Result<()> {
    let slices: Value = serde_json::from_str(
        &kubectl::kubectl(
            &target.kubeconfig,
            &target.context,
            "hosts",
            &[
                "get",
                "endpointslices",
                "--selector=kubernetes.io/service-name=wamn-edge",
                "-o",
                "json",
            ],
            None,
        )
        .await?,
    )?;
    ensure!(
        slices["items"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|slice| {
                slice["endpoints"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|endpoint| {
                        endpoint["conditions"]["ready"] == true
                            && endpoint["addresses"]
                                .as_array()
                                .is_some_and(|addresses| !addresses.is_empty())
                    })
            }),
        "platform edge has no ready endpoint; run wamn platform deploy"
    );
    Ok(())
}

/// Hostname conflicts are warnings. The edge selects the lowest route ConfigMap name.
pub(crate) async fn warn_conflicts(
    target: &Target,
    host: &str,
    release: &str,
) -> anyhow::Result<()> {
    let objects: Value = serde_json::from_str(
        &kubectl::kubectl(
            &target.kubeconfig,
            &target.context,
            "hosts",
            &[
                "get",
                "configmaps",
                "--selector=wamn.route=true",
                "-o",
                "json",
            ],
            None,
        )
        .await?,
    )?;
    for object in objects["items"].as_array().into_iter().flatten() {
        let route: Value = serde_json::from_str(
            object["data"]["route.json"]
                .as_str()
                .context("route ConfigMap lacks route.json")?,
        )?;
        if route["host"] == host && route["release"] != release {
            tracing::warn!(%host, %release, other = %route["release"], "hostname conflict; the edge selects the lowest route ConfigMap name");
        }
    }
    Ok(())
}

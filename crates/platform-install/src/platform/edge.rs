//! Platform edge installation and application route prerequisites.

use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_release_control::release_chart::Target;

use super::document::{ClusterDocument, Provider};
use wamn_kubernetes::kubectl;

/// Select one platform edge, with no application route list.
pub fn chart_input(chart: &Path, image: &str, document: &ClusterDocument) -> anyhow::Result<Value> {
    let bucket = &document.buckets.web;
    Ok(json!({
        "chart":chart,
        "version":wamn_release_control::release_chart::read_platform_version(&chart.join("Chart.yaml"))?,
        "name":"wamn-edge", "namespace":"hosts",
        "values":{
            "neg":document.provider == Provider::Gcp,
            "trustedProxyHops":if document.provider == Provider::Gcp { 2 } else { 1 },
            "tlsSecret":"wamn-edge-tls", "identityCaConfigMap":"identity-ca",
            "identity":&super::identity::issuer(&document.org),
            "web":{"image":image, "bucket":if bucket.is_empty() { String::new() } else { format!("gs://{bucket}") }},
        }
    }))
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
pub async fn warn_conflicts(target: &Target, host: &str, release: &str) -> anyhow::Result<()> {
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

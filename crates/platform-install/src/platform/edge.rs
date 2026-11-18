//! Platform edge installation and application route prerequisites.

use std::path::Path;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_kubernetes::Target;

use super::document::ClusterDocument;
use wamn_kubernetes::kubectl;

/// Select one platform edge, with no application route list.
pub fn chart_input(chart: &Path, image: &str, document: &ClusterDocument) -> anyhow::Result<Value> {
    let bucket = &document.buckets.web;
    Ok(json!({
        "chart":chart,
        "version":crate::release_chart::read_platform_version(&chart.join("Chart.yaml"))?,
        "name":"wamn-edge", "namespace":"hosts",
        "values":{
            "tlsSecret":"wamn-edge-tls", "identityCaConfigMap":"identity-ca",
            "identity":&super::identity::issuer(document.org.as_str()),
            "web":{"image":image, "bucket":if bucket.is_empty() { String::new() } else { format!("gs://{bucket}") }},
        }
    }))
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

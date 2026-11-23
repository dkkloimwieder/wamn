//! Render the collector for interface tests.

use serde_json::{Value, json};
use std::path::Path;

/// Render the platform collector chart with GCP configuration and test environment values.
pub fn deployed_collector(repository: &Path) -> anyhow::Result<Vec<Value>> {
    let chart = repository.join("deploy/charts/platform");
    let values: Value = serde_yaml::from_slice(&std::fs::read(chart.join("values-gcp.yaml"))?)?;
    let metadata: Value = serde_yaml::from_slice(&std::fs::read(chart.join("Chart.yaml"))?)?;
    wamn_kubernetes::helm::template(
        &json!({"chart":chart,"version":metadata["version"],"name":"wamn","namespace":"platform","values":values}),
    )
}

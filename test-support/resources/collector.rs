//! Render the collector for interface tests.

use serde_json::{Value, json};
use std::path::Path;

/// Render the platform collector chart with GCP configuration and test environment values.
pub fn deployed_collector(repository: &Path) -> anyhow::Result<Vec<Value>> {
    let chart = repository.join("deploy/charts/installation");
    let mut values: Value = serde_yaml::from_slice(&std::fs::read(chart.join("values-gcp.yaml"))?)?;
    values["org"] = "fixture".into();
    values["registry"] = "registry.invalid".into();
    values["opentelemetry-collector"]["extraEnvs"] = json!([
        {"name":"CLUSTER","value":"fixture"},{"name":"PROJECT","value":"fixture"},
        {"name":"REGION","value":"us-central1"},{"name":"BUCKET","value":"fixture"}
    ]);
    let metadata: Value = serde_yaml::from_slice(&std::fs::read(chart.join("Chart.yaml"))?)?;
    wamn_kubernetes::helm::template(
        &json!({"chart":chart,"version":metadata["version"],"name":"wamn","namespace":"platform","values":values}),
    )
}

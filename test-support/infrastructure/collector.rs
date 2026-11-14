//! Render the collector for interface tests.

use anyhow::Context as _;
use serde_json::{Value, json};
use std::path::Path;

/// Render the platform collector chart with GCP configuration and test environment values.
pub fn deployed_collector(repository: &Path) -> anyhow::Result<Vec<Value>> {
    let chart = repository.join("deploy/platform/installation");
    let mut values: Value = serde_yaml::from_slice(&std::fs::read(chart.join("values-gcp.yaml"))?)?;
    values["org"] = "fixture".into();
    values["registry"] = "registry.invalid".into();
    values["opentelemetry-collector"]["extraEnvs"] = json!([
        {"name":"CLUSTER","value":"fixture"},{"name":"PROJECT","value":"fixture"},
        {"name":"REGION","value":"us-central1"},{"name":"BUCKET","value":"fixture"}
    ]);
    wamn_kubernetes::helm::template(
        &json!({"chart":chart,"name":"wamn","namespace":"platform","values":values}),
    )
}

/// Render the cluster test collector from the deployed receiver and processors.
/// Only file exporters and the retained Prometheus metric pipeline differ.
pub fn render_collector(repository: &Path, work: &Path) -> anyhow::Result<std::path::PathBuf> {
    use serde::Deserialize as _;
    let documents = deployed_collector(repository)?;
    let source = documents
        .iter()
        .find(|document| {
            document["kind"] == "ConfigMap" && document["metadata"]["name"] == "otel-collector"
        })
        .context("deployed collector has a ConfigMap")?;
    let mut configuration: Value = serde_yaml::from_str(
        source["data"]["relay"]
            .as_str()
            .context("deployed collector has configuration")?,
    )?;
    let image = documents
        .iter()
        .find(|document| {
            document["kind"] == "Deployment" && document["metadata"]["name"] == "otel-collector"
        })
        .context("deployed collector has a Deployment")?["spec"]["template"]["spec"]["containers"]
        [0]["image"]
        .as_str()
        .context("deployed collector has an image")?;
    let template = std::fs::read_to_string(repository.join("deploy/infra/otel-collector.yaml"))?;
    let mut documents = serde_yaml::Deserializer::from_str(&template)
        .map(Value::deserialize)
        .collect::<Result<Vec<_>, _>>()?;
    for document in &mut documents {
        match document["kind"].as_str() {
            Some("ConfigMap") => {
                let metrics: Value = serde_yaml::from_str(
                    document["data"]["config.yaml"]
                        .as_str()
                        .context("metric collector has configuration")?,
                )?;
                configuration["exporters"] = json!({
                    "file/traces":{"path":"/var/lib/otelcol/traces.jsonl","format":"json"},
                    "file/logs":{"path":"/var/lib/otelcol/logs.jsonl","format":"json"},
                    "prometheus":metrics["exporters"]["prometheus"]
                });
                for signal in ["traces", "logs"] {
                    configuration["service"]["pipelines"][signal]["exporters"] =
                        json!([format!("file/{signal}")]);
                }
                configuration["processors"]["batch/metrics"] =
                    metrics["processors"]["batch/metrics"].clone();
                configuration["service"]["pipelines"]["metrics"] =
                    metrics["service"]["pipelines"]["metrics"].clone();
                configuration["service"]["telemetry"] = metrics["service"]["telemetry"].clone();
                document["data"]["config.yaml"] = json!(serde_yaml::to_string(&configuration)?);
            }
            Some("Deployment") => {
                let containers = document["spec"]["template"]["spec"]["containers"]
                    .as_array_mut()
                    .context("collector deployment has containers")?;
                containers[0]["image"] = json!(image);
                containers[1]["image"] = json!(
                    "curlimages/curl@sha256:88a9abad9d958340e48564f9bdcdaa29916a2984be59314da709f8bbc0eef6f7"
                );
            }
            _ => {}
        }
    }
    let path = work.join("otel-collector.yaml");
    std::fs::write(
        &path,
        documents
            .iter()
            .map(serde_yaml::to_string)
            .collect::<Result<Vec<_>, _>>()?
            .join("---\n"),
    )?;
    Ok(path)
}

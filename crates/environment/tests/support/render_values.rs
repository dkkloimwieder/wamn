//! Values shared by the chart interface tests.

use anyhow::{Context as _, ensure};
use serde::Deserialize as _;
use serde_yaml::Value;
pub use wamn_control_provision::environment_names::EventIdentity;

/// Parse rendered Kubernetes objects without accepting command output as a document.
pub fn kubernetes_documents(bytes: &[u8]) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut documents = Vec::new();
    for (index, document) in serde_yaml::Deserializer::from_slice(bytes).enumerate() {
        let document = serde_json::Value::deserialize(document)
            .context("parse rendered Kubernetes document")?;
        if document.is_null() {
            continue;
        }
        for field in ["apiVersion", "kind"] {
            ensure!(
                document[field]
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty()),
                "rendered Kubernetes document {} requires {field}",
                index + 1,
            );
        }
        documents.push(document);
    }
    ensure!(
        !documents.is_empty(),
        "rendered Kubernetes document stream is empty"
    );
    Ok(documents)
}

/// Inputs for the base host values and the selected application's overlay.
#[derive(Debug, Clone)]
pub struct HostValuesInput {
    pub namespace: String,
    pub host_tag: String,
    pub replicas: u32,
    pub component_artifact_base: String,
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
    pub event: EventIdentity,
    pub object_store_secret_name: Option<String>,
}

/// Host values in Helm's required application order.
#[derive(Debug)]
pub struct RenderedHostValues {
    pub base: String,
    pub overlay: String,
}

/// The host subchart defaults of the current release chart.
pub fn host_defaults() -> anyhow::Result<String> {
    let chart: Value = serde_yaml::from_str(include_str!(
        "../../../../../deploy/platform/environment/values.yaml"
    ))?;
    let mut host = chart["runtime-operator"].clone();
    host["global"] = chart["global"].clone();
    serde_yaml::to_string(&host).context("render the release chart host defaults")
}

/// Mount the object-store Secret that the application fixture supplies.
pub fn object_store_overlay(overlay: &str, secret_name: &str) -> anyhow::Result<String> {
    let mut overlay: Value = serde_yaml::from_str(overlay)?;
    let groups = overlay["runtime"]["hostGroups"]
        .as_sequence_mut()
        .context("host overlay hostGroups")?;
    ensure!(groups.len() == 1, "render one host group");
    let group = &mut groups[0];
    group["env"]
        .as_sequence_mut()
        .context("host environment")?
        .push(serde_yaml::to_value(serde_json::json!({
            "name": "WAMN_CREDENTIALS_FILE", "value": "/etc/wamn/credentials/credentials.json"
        }))?);
    group["volumes"]
        .as_sequence_mut()
        .context("host volumes")?
        .push(serde_yaml::to_value(serde_json::json!({
            "name": "connection-credentials", "secret": {
                "secretName": secret_name, "optional": false,
                "items": [{"key": "credentials.json", "path": "credentials.json"}]
            }
        }))?);
    group["volumeMounts"]
        .as_sequence_mut()
        .context("host mounts")?
        .push(serde_yaml::to_value(serde_json::json!({
            "name": "connection-credentials", "mountPath": "/etc/wamn/credentials", "readOnly": true
        }))?);
    serde_yaml::to_string(&overlay).context("render object-store host overlay")
}

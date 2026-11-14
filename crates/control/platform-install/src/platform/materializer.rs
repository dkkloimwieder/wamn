//! The materializer workload is rendered by the environment chart.
use serde_json::json;
use std::path::Path;

pub const FETCH_MS: u64 = 1000;
pub const SWEEP_MS: u64 = 5000;

pub use wamn_control_provision::environment_names::EventIdentity;

#[derive(Debug)]
pub struct MaterializerInput {
    pub workload: String,
    pub namespace: String,
    pub image: String,
    pub project_env_key: String,
    pub event: EventIdentity,
    pub event_stream: String,
    pub fetch_ms: u64,
    pub sweep_ms: u64,
}

pub fn values(input: &MaterializerInput) -> serde_json::Value {
    json!({"materializer":{
        "name":input.workload,"hostGroup":input.workload.strip_suffix("-materializer").unwrap_or(&input.workload),
        "image":input.image,"projectEnvKey":input.project_env_key,"org":input.event.org,
        "project":input.event.project,"environment":input.event.environment,"stream":input.event_stream,
        "fetchMs":input.fetch_ms,"sweepMs":input.sweep_ms,
    }})
}

pub fn render_materializer(chart: &Path, input: &MaterializerInput) -> anyhow::Result<String> {
    let entry = crate::environment::entry(chart, &input.namespace, &input.workload, values(input))?;
    let object = crate::environment::resource(
        wamn_kubernetes::helm::template(&entry)?,
        "WorkloadDeployment",
    )?;
    Ok(serde_yaml::to_string(&object)?)
}

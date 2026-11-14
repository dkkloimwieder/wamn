//! Helm owns environment resources; credential writes and SQL remain explicit.
use anyhow::Context as _;
use serde_json::{Value, json};
use std::path::Path;
use wamn_release_control::release_chart::Target;

pub async fn apply(
    target: &Target,
    chart: &Path,
    name: &str,
    overlay: Value,
) -> anyhow::Result<()> {
    let mut values = crate::platform::installed::release_values(target, &target.namespace, name)
        .await?
        .unwrap_or_else(|| json!({}));
    crate::platform::resources::merge(&mut values, overlay);
    let entry = entry(chart, &target.namespace, name, values)?;
    let directory = tempfile::tempdir()?;
    crate::platform::resources::apply_release(target, &entry, directory.path()).await
}

pub fn entry(chart: &Path, namespace: &str, name: &str, values: Value) -> anyhow::Result<Value> {
    let metadata: Value = serde_yaml::from_slice(&std::fs::read(chart.join("Chart.yaml"))?)?;
    let mut entry =
        json!({"chart":chart,"name":name,"namespace":namespace,"version":metadata["version"]});
    entry["values"] = values;
    Ok(entry)
}

pub fn resource(objects: Vec<Value>, type_: &str) -> anyhow::Result<Value> {
    objects
        .into_iter()
        .find(|object| object["kind"] == type_)
        .context("chart did not render the requested resource")
}

pub fn publication_values(
    triple: &wamn_registry::Triple,
    instance: &str,
    cluster: &str,
    name: &str,
    schemas: &[String],
) -> Value {
    json!({"publication":{"name":name,"cluster":cluster,"org":triple.org,"project":triple.project,"env":triple.env,
        "database":wamn_control_provision::project_env_database_name(&triple.org,&triple.project,triple.env.as_str(),instance),
        "schemas":schemas,"publish":wamn_control_provision::sql::CDC_PUBLISH}})
}

/// Keep broker-owned certificate parameters in their Helm owner's values.
pub fn broker_certificates(certificates: Vec<Value>) -> Value {
    json!({"brokerCertificates":certificates.into_iter().map(|mut certificate| {
        let mut entry = json!({});
        entry["name"] = certificate["metadata"]["name"].take();
        entry["spec"] = certificate["spec"].take();
        entry
    }).collect::<Vec<_>>()})
}

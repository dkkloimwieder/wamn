//! Environment worker accounts are owned by the environment chart.
use serde_json::{Value, json};
use std::path::Path;
use wamn_registry::Triple;
use wamn_release_control::release_chart::Target;

pub fn values(triple: &Triple, cluster: &str, database: &str) -> anyhow::Result<Value> {
    let name = wamn_control_provision::environment_names::environment_resource_name(
        &triple.org,
        &triple.project,
        triple.env.as_str(),
    )?;
    Ok(json!({"operations":{"name":name,"cluster":cluster,"database":database}}))
}

pub async fn provision(
    target: &Target,
    chart: &Path,
    triple: &Triple,
    cluster: &str,
    database: &str,
) -> anyhow::Result<()> {
    let values = values(triple, cluster, database)?;
    let name = wamn_control_provision::environment_names::environment_resource_name(
        &triple.org,
        &triple.project,
        triple.env.as_str(),
    )?;
    let target = Target {
        namespace: triple.org.clone(),
        ..target.clone()
    };
    crate::environment::apply(&target, &chart.join("platform/environment"), &name, values).await
}

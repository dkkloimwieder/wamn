//! Environment worker accounts are owned by the project chart.
use serde_json::{Value, json};
use wamn_registry::Triple;

pub fn values(triple: &Triple, cluster: &str, database: &str) -> anyhow::Result<Value> {
    let name = wamn_provision::environment_names::environment_resource_name(
        &triple.org,
        &triple.project,
        &triple.env,
    )?;
    let capture = wamn_provision::project_env_cdc_secret_name(
        &triple.org,
        &triple.project,
        &triple.env,
    );
    Ok(json!({"operations":{"name":name,"cluster":cluster,"database":database,"capture":capture}}))
}

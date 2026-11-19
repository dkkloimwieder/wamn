//! The infrastructure release precedes the platform release and its explicit Jobs.

use super::{artifact_plan::SelectedChart, document::ClusterDocument, resources};
use anyhow::Context as _;
use serde_json::{Value, json};
use std::path::Path;
use wamn_kubernetes::{Target, helm::Wait};

pub async fn infrastructure(
    target: &Target,
    chart: &SelectedChart,
    directory: &Path,
) -> anyhow::Result<()> {
    let entry = entry(
        &chart.directory().join("platform/infra"),
        "wamn-infrastructure",
        json!({}),
    )?;
    resources::apply_release(target, &entry, directory, Wait::Watcher).await
}

pub fn values(document: &ClusterDocument, chart: &SelectedChart) -> anyhow::Result<Value> {
    let (_, domain) = document
        .route_hosts
        .control
        .split_once('.')
        .context("control hostname requires a domain")?;
    Ok(json!({
        "org":document.org, "registry":document.registry,
        "edgeDnsNames":[format!("*.{domain}")],
        "platform":{"chart":chart.reference()},
        "runtime-operator":{"operator":{"watchNamespaces":[document.org],"hostNamespaces":[document.org]}},
        "configuration":{
            "insecure_registry":wamn_registry::registry_is_loopback(&document.registry),
            "package_base":format!("{}/wamn/packages",document.registry),
            "component_base":format!("{}/wamn/components",document.registry)
        }
    }))
}

pub(super) const SYSTEM_DATABASE: &str = "wamn_system";
pub(super) const SYSTEM_CLUSTER: &str = "wamn-pg";

pub(super) fn system_database_host() -> String {
    format!("{SYSTEM_CLUSTER}-rw.platform.svc.cluster.local")
}

pub fn system_database_values(org: &wamn_registry::OrgId) -> anyhow::Result<Value> {
    Ok(json!({
        "name": SYSTEM_DATABASE,
        "cluster": SYSTEM_CLUSTER,
        "host": system_database_host(),
        "roles": crate::roles::platform(org, SYSTEM_DATABASE)?,
    }))
}

pub fn platform_entry(chart: &SelectedChart, mut values: Value) -> anyhow::Result<Value> {
    let org: wamn_registry::OrgId = values["org"]
        .as_str()
        .context("installation org is absent")?
        .parse()?;
    values["systemDatabase"] = system_database_values(&org)?;
    if values["global"]["natsAccountOrgs"].is_null() {
        values["global"]["natsAccountOrgs"] = json!([org]);
    }
    entry(
        &chart.directory().join("platform/installation"),
        "wamn",
        values,
    )
}

fn entry(chart: &Path, name: &str, values: Value) -> anyhow::Result<Value> {
    let metadata: Value = serde_yaml::from_slice(&std::fs::read(chart.join("Chart.yaml"))?)?;
    let mut entry =
        json!({"chart":chart,"name":name,"namespace":"platform","version":metadata["version"]});
    entry["values"] = values;
    Ok(entry)
}

pub async fn apply(
    target: &Target,
    chart: &SelectedChart,
    values: &mut Value,
    directory: &Path,
) -> anyhow::Result<()> {
    let mut entry = platform_entry(chart, std::mem::take(values))?;
    let result = resources::apply_release(target, &entry, directory, Wait::None).await;
    *values = entry["values"].take();
    result
}

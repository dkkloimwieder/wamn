//! The infrastructure release precedes the platform release and its explicit Jobs.

use super::{artifact_plan::SelectedChart, document::ClusterDocument, resources};
use anyhow::Context as _;
use serde_json::{Value, json};
use std::path::Path;
use wamn_kubernetes::Target;

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
    resources::apply_release(target, &entry, directory).await
}

pub fn values(document: &ClusterDocument, chart: &SelectedChart) -> anyhow::Result<Value> {
    let (_, domain) = document
        .route_hosts
        .control
        .split_once('.')
        .context("control hostname requires a domain")?;
    let mut values = json!({
        "org":document.org, "registry":document.registry,
        "edgeDnsNames":[format!("*.{domain}")],
        "platform":{"chart":chart.reference()},
        "runtime-operator":{"operator":{"watchNamespaces":[document.org],"hostNamespaces":[document.org]}},
        "configuration":{
            "insecure_registry":wamn_registry::registry_is_loopback(&document.registry),
            "package_base":format!("{}/wamn/packages",document.registry),
            "component_base":format!("{}/wamn/components",document.registry)
        }
    });
    resources::merge(&mut values, host_settings(document));
    Ok(values)
}

pub fn platform_entry(chart: &SelectedChart, values: Value) -> anyhow::Result<Value> {
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

/// Public system cluster inputs shared by installation and local measurement.
pub fn organization_values(org: &wamn_registry::OrgId) -> Value {
    json!({"org":org,"clusters":[{
        "name":"wamn-pg","database":"wamn_system","owner":wamn_role::SYSTEM_ROLE,"enablePDB":false,
        "parameters":{"max_slot_wal_keep_size":"4GB","pg_stat_statements.max":"10000","pg_stat_statements.track":"top","auto_explain.log_min_duration":"-1"},
        "policy":{"name":"system","instances":1,"image":"ghcr.io/cloudnative-pg/postgresql:18","cpu":"200m","memory":"256Mi","storage":"10Gi","hibernation":"never","backup-cadence":"","wal-retention":""}
    }]})
}

pub fn organization_entry(
    document: &ClusterDocument,
    chart: &SelectedChart,
    installed: Option<Value>,
) -> anyhow::Result<Value> {
    let mut values = organization_values(&document.org);
    values["platform"] = json!({"chart":chart.reference()});
    if let Some(mut installed) = installed {
        for cluster in installed["clusters"]
            .take()
            .as_array_mut()
            .into_iter()
            .flatten()
        {
            if cluster["database"] != "wamn_system" {
                values["clusters"]
                    .as_array_mut()
                    .context("org clusters")?
                    .push(cluster.take());
            }
        }
    }
    let roles = crate::roles::platform(&document.org, "wamn-pg", "wamn_system")?;
    values["loginSecrets"] = roles.values().flatten()
        .map(|role| json!({"name":role.password_secret.name,"username":role.name}))
        .collect::<Vec<_>>().into();
    values["managedRoles"] = crate::roles::values(&roles)?;
    crate::environment::entry(
        &chart.directory().join("platform/org"),
        document.org.as_str(),
        &wamn_registry::name::resource_suffix("org", document.org.as_str()),
        values,
    )
}

pub async fn apply(
    target: &Target,
    chart: &SelectedChart,
    values: &mut Value,
    directory: &Path,
) -> anyhow::Result<()> {
    wamn_broker::authorization::bootstrap(target).await?;
    let mut entry = platform_entry(chart, std::mem::take(values))?;
    let result = resources::apply_release(target, &entry, directory).await;
    *values = entry["values"].take();
    result
}

pub fn host_settings(document: &ClusterDocument) -> Value {
    let mut values = json!({});
    let issuer = super::identity::issuer(document.org.as_str());
    values["hostEnvironment"] = json!({
        "DOCKER_CONFIG":"/etc/wamn/registry",
        "WAMN_SESSION_ISSUER":issuer,
        "WAMN_SESSION_JWKS_URL":format!("{issuer}/.well-known/jwks.json"),
        "WAMN_SESSION_AUDIENCE":wamn_registry::name::org_audience(&document.org),
        "WAMN_SESSION_ALGORITHMS":"EdDSA",
        "WAMN_SESSION_JWKS_CA":"/etc/identity-ca/ca.crt",
    });
    values
}

//! The infrastructure release precedes the platform release and its explicit Jobs.

use super::{artifact_plan::SelectedChart, document::ClusterDocument, resources};
use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
use std::path::Path;
use wamn_kubernetes::{Target, kubectl, secret::Secret};

const WEB_CSRF_SECRET: &str = "wamn-web-csrf";

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
pub fn organization_values(document: &ClusterDocument) -> Value {
    let values = json!({"org":document.org,"clusters":[{
        "name":"wamn-pg","database":"wamn_system","owner":wamn_role::SYSTEM_ROLE,"enablePDB":false,
        "parameters":{"max_slot_wal_keep_size":"4GB","pg_stat_statements.max":"10000","pg_stat_statements.track":"top","auto_explain.log_min_duration":"-1"},
        "policy":{"name":"system","instances":1,"image":"ghcr.io/cloudnative-pg/postgresql:18","cpu":"200m","memory":"256Mi","storage":"10Gi","hibernation":"never","backup-cadence":"","wal-retention":""}
    }]});
    values
}

pub fn organization_entry(
    document: &ClusterDocument,
    chart: &SelectedChart,
    installed: Option<Value>,
) -> anyhow::Result<Value> {
    let mut values = organization_values(document);
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
    values["managedRoles"] = crate::roles::values(&crate::roles::platform(
        &document.org,
        "wamn-pg",
        "wamn_system",
    )?)?;
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
    let namespace = match values["edge"]["namespace"].as_str() {
        Some("") => "platform",
        Some(namespace) => namespace,
        None => "hosts",
    };
    ensure_web_csrf_secret(target, namespace).await?;
    let mut entry = platform_entry(chart, std::mem::take(values))?;
    let result = resources::apply_release(target, &entry, directory).await;
    *values = entry["values"].take();
    result
}

async fn ensure_web_csrf_secret(target: &Target, namespace: &str) -> anyhow::Result<()> {
    if kubectl::get::<Secret>(
        &target.kubeconfig,
        &target.context,
        namespace,
        "secret",
        WEB_CSRF_SECRET,
    )
    .await?
    .is_some()
    {
        return Ok(());
    }
    let mut key = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| anyhow::anyhow!("generate web CSRF key"))?;
    let mut secret = Secret::new(namespace, WEB_CSRF_SECRET);
    secret.data.insert("key".into(), STANDARD.encode(key));
    let body = serde_json::to_vec(&secret).context("encode web CSRF Secret")?;
    match kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        namespace,
        &["create", "-f", "-"],
        Some(&body),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(error) if error.is::<kubectl::Conflict>() => Ok(()),
        Err(error) => Err(error).context("create web CSRF Secret"),
    }
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

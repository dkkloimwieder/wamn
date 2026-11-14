//! The infrastructure release precedes the platform release and its explicit Jobs.

use super::{
    artifact_plan::SelectedChart,
    document::{ClusterDocument, Provider},
    provider, resources,
};
use anyhow::Context as _;
use serde_json::{Value, json};
use std::path::Path;
use wamn_release_control::release_chart::Target;

pub async fn infrastructure(
    target: &Target,
    document: &ClusterDocument,
    chart: &SelectedChart,
    directory: &Path,
) -> anyhow::Result<()> {
    let entry = entry(
        &chart.directory().join("platform/infra"),
        "wamn-infrastructure",
        json!({
            "traefik":{"enabled": document.provider != Provider::Gcp}
        }),
    )?;
    resources::apply_release(target, &entry, directory).await
}

pub async fn values(document: &ClusterDocument, chart: &SelectedChart) -> anyhow::Result<Value> {
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
            "insecure_registry":document.provider == Provider::Kind,
            "package_base":format!("{}/wamn/packages",document.registry),
            "component_base":format!("{}/wamn/components",document.registry),
            "release_base":format!("{}/wamn/releases",document.registry)
        }
    });
    resources::merge(&mut values, host_settings(document));
    match document.provider {
        Provider::Kind => {
            let registry = provider::ensure_registry(document).await?;
            values["registryEndpoint"] = json!({"address":registry.address.to_string(),"port":registry.port,
                "addressType":if registry.address.is_ipv4() { "IPv4" } else { "IPv6" }});
        }
        Provider::Gcp => {
            let mut telemetry: Value = serde_yaml::from_slice(&std::fs::read(
                chart
                    .directory()
                    .join("platform/installation/values-gcp.yaml"),
            )?)?;
            let collector = &mut telemetry["opentelemetry-collector"];
            collector["extraEnvs"] = json!([
                {"name":"BUCKET","value":document.buckets.otel},
                {"name":"CLUSTER","value":document.cluster_name},
                {"name":"PROJECT","value":document.project_id},
                {"name":"REGION","value":document.zone.rsplit_once('-').context("GCP zone")?.0}
            ]);
            collector["serviceAccount"]["annotations"] = json!({"iam.gke.io/gcp-service-account":format!("wamn-otel@{}.iam.gserviceaccount.com",document.project_id)});
            resources::merge(&mut values, telemetry);
            values["acme"] = json!({"server":"https://acme-v02.api.letsencrypt.org/directory","email":document.owner_email,
                "privateKeySecretRef":{"name":"letsencrypt"},"solvers":[{"dns01":{"cloudDNS":{"project":document.project_id,"hostedZoneName":document.project_id}}}]});
        }
    }
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
    let mut values = json!({"org":document.org,"clusters":[{
        "name":"wamn-pg","database":"wamn_system","owner":"wamn_system","enablePDB":false,
        "parameters":{"max_slot_wal_keep_size":"4GB","pg_stat_statements.max":"10000","pg_stat_statements.track":"top","auto_explain.log_min_duration":"-1"},
        "storageClass":if document.provider == Provider::Kind { "standard" } else { "premium-rwo" },
        "policy":{"name":"system","instances":1,"image":"ghcr.io/cloudnative-pg/postgresql:18","cpu":"200m","memory":"256Mi","storage":"10Gi","hibernation":"never","backup-cadence":"","wal-retention":""}
    }]});
    if document.provider == Provider::Gcp {
        values["clusters"][0]["affinity"] = json!({"nodeSelector":{"role":"postgres"},"tolerations":[{"key":"role","operator":"Equal","value":"postgres","effect":"NoSchedule"}]});
    }
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
        &document.org,
        &wamn_registry::name::resource_suffix("org", &document.org),
        values,
    )
}

pub async fn apply(
    target: &Target,
    chart: &SelectedChart,
    values: &mut Value,
    directory: &Path,
) -> anyhow::Result<()> {
    let mut entry = platform_entry(chart, std::mem::take(values))?;
    let result = resources::apply_release(target, &entry, directory).await;
    *values = entry["values"].take();
    result
}

pub fn host_settings(document: &ClusterDocument) -> Value {
    let mut values = json!({});
    let registry = if document.provider == Provider::Kind {
        resources::POD_REGISTRY
    } else {
        &document.registry
    };
    values["hostEnvironment"] = json!({
        "DOCKER_CONFIG":"/etc/wamn/registry", "WAMN_SESSION_ISSUER":super::identity::issuer(&document.org),
        "WAMN_SESSION_JWKS_CA":"/etc/identity-ca/ca.crt",
        "WAMN_COMPONENT_ARTIFACT_BASE":format!("{registry}/wamn/components"),
        "WAMN_PACKAGE_ARTIFACT_BASE":format!("{registry}/wamn/packages"),
        "WAMN_RELEASE_ARTIFACT_BASE":format!("{registry}/wamn/releases"),
        "WAMN_OAUTH_ENCRYPTION":super::oauth::configuration(document).to_string(),
    });
    let mut shared = json!({"replicas":1,"http":{"enabled":true,"port":80},
        "schedulerNatsUrl":"nats://wamn-nats.platform.svc.cluster.local:4222",
        "dataNatsUrl":"nats://wamn-nats.platform.svc.cluster.local:4222",
        "envFrom":[{"configMapRef":{"name":"wamn-host-settings"}}],
        "volumes":[{"name":"identity-ca","configMap":{"name":"identity-ca"}},
            {"name":"registry-pull","configMap":{"name":"wamn-registry-helper"}}],
        "volumeMounts":[{"name":"identity-ca","mountPath":"/etc/identity-ca","readOnly":true},
            {"name":"registry-pull","mountPath":"/etc/wamn/registry","readOnly":true}]
    });
    if document.provider == Provider::Kind {
        values["hostEnvironment"]["WAMN_REGISTRY_AUTH_FILE"] =
            "/etc/wamn/registry/config.json".into();
        values["hostEnvironment"]["WAMN_OAUTH_KEY_FILE"] = "/etc/wamn/oauth/key".into();
        shared["volumes"][1] = json!({"name":"registry-pull","secret":{"secretName":"wamn-registry-pull","items":[{"key":".dockerconfigjson","path":"config.json"}]}});
        shared["volumes"].as_array_mut().unwrap().push(
            json!({"name":"oauth-key","secret":{"secretName":"wamn-oauth","defaultMode":384}}),
        );
        shared["volumeMounts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"oauth-key","mountPath":"/etc/wamn/oauth","readOnly":true}));
    } else {
        values["hostEnvironment"]["WAMN_REGISTRY_TOKEN_METADATA"] = "true".into();
    }
    values["hostConfiguration"] = shared;
    values
}

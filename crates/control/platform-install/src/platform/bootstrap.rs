//! System initialization Jobs and redacted Kubernetes credential decoding.

use std::path::Path;
use wamn_kubernetes::job;

use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity};
use wamn_release_control::release_chart::Target;

use super::artifact_plan::SelectedChart;
use super::document::ClusterDocument;
use wamn_kubernetes::kubectl;

pub(super) fn database_host(namespace: &str) -> String {
    format!("wamn-pg-rw.{namespace}.svc.cluster.local")
}

/// Encoded Secret data. Debug output omits every value.
#[derive(Deserialize)]
pub struct SecretData {
    #[serde(default)]
    data: std::collections::BTreeMap<String, String>,
}

impl std::fmt::Debug for SecretData {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretData")
            .field("data", &"[REDACTED]")
            .finish()
    }
}

pub(crate) fn secret_text(secret: &SecretData, key: &str) -> anyhow::Result<String> {
    let encoded = secret
        .data
        .get(key)
        .with_context(|| format!("the Secret has no {key}"))?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!("the Secret key {key} is not base64"))?;
    String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("the Secret key {key} is not UTF-8"))
}

/// Configure the system administrator from native CNPG Secrets in the org namespace.
pub(super) fn administrator(job: &mut Value, namespace: &str) -> anyhow::Result<()> {
    let config = crate::credentials::configuration(
        namespace,
        "wamn-pg",
        database_host(namespace),
        5432,
        "wamn_system".into(),
        "postgres".into(),
        "wamn-pg-superuser".into(),
    );
    let spec = &mut job["spec"]["template"]["spec"];
    spec["automountServiceAccountToken"] = true.into();
    let container = job::container(job);
    if !container["env"].is_array() {
        container["env"] = json!([]);
    }
    container["env"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"WAMN_SYSTEM_ADMIN_URL","value":config.to_json()?}));
    Ok(())
}

fn system_job(document: &ClusterDocument, image: &str) -> Value {
    let domain = document
        .route_hosts
        .control
        .split_once('.')
        .map_or(document.route_hosts.control.as_str(), |(_, domain)| domain);

    job::manifest(
        "platform-system",
        image,
        &["/usr/local/bin/wamn-ctl"],
        &["provision-system", "--platform-domain", domain],
    )
}

fn organization_job(document: &ClusterDocument, image: &str) -> Value {
    job::manifest(
        "platform-org",
        image,
        &["/usr/local/bin/wamn-ctl"],
        &[
            "provision-org",
            "--org",
            &document.org,
            "--template",
            "trials",
            "--pool",
            "wamn-pg",
        ],
    )
}

fn issuer_credential_job(document: &ClusterDocument, image: &str) -> Value {
    job::manifest(
        "platform-identity-issuer",
        image,
        &["/usr/local/bin/wamn-ctl"],
        &[
            "provision-identity-issuer",
            "--issuer",
            &super::identity::issuer(&document.org),
            "--namespace",
            &document.org,
        ],
    )
}

/// Initialize system SQL and the credentials of the initial platform services.
///
/// Public Job results remain in `directory/jobs`, including the system SQL result.
///
/// # Errors
/// Returns SQL, credential, or Kubernetes failures without resetting database state.
pub async fn initialize(
    target: &Target,
    document: &ClusterDocument,
    chart: &SelectedChart,
    directory: &Path,
) -> anyhow::Result<()> {
    let target = Target {
        namespace: document.org.clone(),
        ..target.clone()
    };
    let image = chart.image("ctl")?;
    let mut system = system_job(document, image);
    administrator(&mut system, &document.org)?;
    registry(&mut system, document);
    job::run(&target, directory, &mut system, &mut []).await?;
    let mut org = organization_job(document, image);
    administrator(&mut org, &document.org)?;
    registry(&mut org, document);
    job::run(&target, directory, &mut org, &mut []).await?;

    let mut issuer = issuer_credential_job(document, image);
    administrator(&mut issuer, &document.org)?;
    registry(&mut issuer, document);
    job::run(&target, directory, &mut issuer, &mut []).await?;
    let control = crate::credentials::configuration(
        &document.org,
        "wamn-pg",
        database_host(&document.org),
        5432,
        "wamn_system".into(),
        wamn_control_provision::workload_login(
            WorkloadRoleFamily::Control,
            WorkloadRoleIdentity::Org {
                org: &document.org,
                database: "wamn_system",
            },
        )?,
        wamn_registry::control_nats_user(&document.org),
    );
    let configuration = json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":wamn_registry::name::control_administration_secret_name(&document.org),"namespace":document.org},"data":{"control.json":control.to_json()?}});
    kubectl::apply(
        &target.kubeconfig,
        &target.context,
        &document.org,
        &configuration,
    )
    .await?;
    Ok(())
}

fn registry(job: &mut Value, document: &ClusterDocument) {
    let reference = if document.provider == super::document::Provider::Kind {
        json!({"secret":{"secretName":"wamn-registry-pull","items":[{"key":".dockerconfigjson","path":"config.json"}]}})
    } else {
        json!({"configMap":{"name":"wamn-registry-helper"}})
    };
    let spec = &mut job["spec"]["template"]["spec"];
    if !spec["volumes"].is_array() {
        spec["volumes"] = json!([]);
    }
    let mut volume = reference;
    volume["name"] = "registry".into();
    spec["volumes"].as_array_mut().unwrap().push(volume);
    let container = job::container(job);
    if !container["volumeMounts"].is_array() {
        container["volumeMounts"] = json!([]);
    }
    container["volumeMounts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"registry","mountPath":"/etc/wamn/registry","readOnly":true}));
    job::environment(
        job,
        &[("WAMN_REGISTRY_AUTH_FILE", "/etc/wamn/registry/config.json")],
    );
}

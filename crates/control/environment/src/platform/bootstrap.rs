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
    crate::provision_project_env::database_host(None, "wamn-pg", namespace).into_owned()
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
    let config = crate::provision_project_env::workload::configuration(
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
            "--admin",
            &document.owner_email,
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
    let access = json!({"apiVersion":"v1","kind":"List","items":[
        {"apiVersion":"rbac.authorization.k8s.io/v1","kind":"Role","metadata":{"name":"wamn-database-provisioning","namespace":document.org},"rules":[
            {"apiGroups":[""],"resources":["secrets","configmaps"],"verbs":["get","create","update","patch"]},
            {"apiGroups":["postgresql.cnpg.io"],"resources":["clusters"],"verbs":["get","patch"]}]},
        {"apiVersion":"rbac.authorization.k8s.io/v1","kind":"RoleBinding","metadata":{"name":"wamn-database-provisioning","namespace":document.org},"roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":"wamn-database-provisioning"},"subjects":[{"kind":"ServiceAccount","name":"default","namespace":document.org}]}]});
    kubectl::apply(&target.kubeconfig, &target.context, &document.org, &access).await?;
    let image = chart.image("ctl")?;
    let mut system = system_job(document, image);
    administrator(&mut system, &document.org)?;
    job::run(&target, directory, &mut system, &mut []).await?;
    let mut org = organization_job(document, image);
    administrator(&mut org, &document.org)?;
    job::run(&target, directory, &mut org, &mut []).await?;

    let mut issuer = issuer_credential_job(document, image);
    administrator(&mut issuer, &document.org)?;
    job::run(&target, directory, &mut issuer, &mut []).await?;
    let control = super::super::provision_project_env::workload::configuration(
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

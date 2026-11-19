//! Ownership tests and deletion of resources installed outside Helm.

use serde_json::Value;
use wamn_registry::{Triple, name};

use super::super::Platform;

async fn get(
    platform: &Platform,
    namespace: &str,
    type_: &str,
    name: &str,
) -> anyhow::Result<Option<Value>> {
    wamn_kubernetes::kubectl::get(
        &platform.target.kubeconfig,
        &platform.target.context,
        namespace,
        type_,
        name,
    )
    .await
}

/// Refuse foreign resources before the first destructive operation.
pub(super) async fn require_ownership(
    platform: &Platform,
    triple: &Triple,
    database: &str,
) -> anyhow::Result<()> {
    let namespace = &platform.target.namespace;
    let reader = wamn_provision::cdc_reader_name(&triple.org, &triple.project, &triple.env);
    if let Some(object) = get(platform, namespace, "deployment", &reader).await? {
        anyhow::ensure!(
            object["metadata"]["annotations"]["wamn.environment"].as_str()
                == Some(triple.to_string().as_str()),
            "refused: Deployment {reader} does not belong to {triple}"
        );
    }
    if let Some(object) = get(
        platform,
        &platform.target.namespace,
        "publication",
        database,
    )
    .await?
    {
        let labels = &object["metadata"]["labels"];
        anyhow::ensure!(
            labels["wamn.org"] == triple.org.as_str()
                && labels["wamn.project"] == triple.project.as_str()
                && labels["wamn.env"] == triple.env.as_str()
                && object["spec"]["dbname"] == database,
            "refused: Publication {database} does not belong to {triple}"
        );
    }
    Ok(())
}

pub(super) async fn delete_reader(platform: &Platform, triple: &Triple) -> anyhow::Result<()> {
    let cdc = wamn_provision::cdc_reader_name(&triple.org, &triple.project, &triple.env);
    if get(platform, &platform.target.namespace, "deployment", &cdc)
        .await?
        .is_some()
    {
        let release = name::environment_resource_name(&triple.org, &triple.project, &triple.env);
        wamn_platform_install::environment::update_project(
            &platform.target, &platform.chart, &release,
            serde_json::json!({"cdcReader":{"pause":true},"publication":{"reclaimPolicy":"delete"}}),
        ).await?;
    }
    crate::environment::cdc_reader::wait_reader_stopped(
        &platform.target.kubeconfig,
        &platform.target.context,
        &platform.target.namespace,
        triple,
    )
    .await
}

pub(super) async fn delete_database_certificate_secrets(
    platform: &Platform,
    database: &str,
) -> anyhow::Result<()> {
    // cert-manager is configured to retain its Secrets after Certificate deletion.
    let cluster = name::project_cluster(database);
    let root = format!("{cluster}-login-ca");
    let namespace = &platform.target.namespace;
    anyhow::ensure!(
        get(platform, namespace, "certificate.cert-manager.io", &root)
            .await?
            .is_none(),
        "cannot remove database certificate Secrets while Certificate {root} exists"
    );
    let secrets: Vec<_> = wamn_provision::WorkloadRoleFamily::ALL
        .into_iter()
        .map(wamn_provision::WorkloadRoleFamily::login_name)
        .chain([wamn_role::CDC_LOGIN])
        .map(|login| name::project_login_certificate(database, login))
        .chain([root, format!("{cluster}-client-repl")])
        .collect();
    let mut arguments = vec!["delete", "secret", "--ignore-not-found"];
    arguments.extend(secrets.iter().map(String::as_str));
    super::kubectl(platform, namespace, &arguments, None)
        .await
        .map(drop)
}

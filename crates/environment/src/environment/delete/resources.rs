//! Ownership tests and deletion of resources installed outside Helm.

use anyhow::Context as _;
use serde_json::Value;
use wamn_registry::Triple;

use super::super::{Platform, ensure::kubectl};

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
    let reader = wamn_control_provision::project_env_cdc_secret_name(
        &triple.org,
        &triple.project,
        &triple.env,
    );
    if let Some(object) = get(platform, namespace, "deployment", &reader).await? {
        anyhow::ensure!(
            object["metadata"]["annotations"]["wamn.environment"].as_str()
                == Some(triple.to_string().as_str()),
            "refused: Deployment {reader} does not belong to {triple}"
        );
    }
    if let Some(object) = get(
        platform,
        &platform.database_namespace,
        "publication",
        database,
    )
    .await?
    {
        let labels = &object["metadata"]["labels"];
        anyhow::ensure!(
            labels["wamn.org"] == triple.org
                && labels["wamn.project"] == triple.project
                && labels["wamn.env"] == triple.env.as_str()
                && object["spec"]["dbname"] == database,
            "refused: Publication {database} does not belong to {triple}"
        );
    }
    let project_env_key =
        wamn_control_provision::project_env_key(&triple.org, &triple.project, &triple.env);
    let certificate = wamn_registry::name::nats_certificate(&project_env_key);
    if let Some(object) = get(platform, namespace, "certificate", &certificate).await? {
        let user = wamn_registry::environment_nats_user(&project_env_key);
        anyhow::ensure!(
            object["spec"]["secretName"] == certificate
                && object["spec"]["commonName"] == user
                && object["spec"]["dnsNames"] == serde_json::json!([user]),
            "refused: Certificate {certificate} does not belong to {triple}"
        );
    }
    if let Some(object) = get(platform, namespace, "secret", &certificate).await? {
        anyhow::ensure!(
            object["metadata"]["annotations"]["cert-manager.io/certificate-name"] == certificate,
            "refused: Secret {certificate} does not belong to its environment Certificate"
        );
    }
    Ok(())
}

pub(super) async fn delete_reader(platform: &Platform, triple: &Triple) -> anyhow::Result<()> {
    let name = wamn_control_provision::project_env_cdc_secret_name(
        &triple.org,
        &triple.project,
        &triple.env,
    );
    kubectl(
        platform,
        &platform.target.namespace,
        &[
            "delete",
            "deployment",
            &name,
            "--ignore-not-found",
            "--cascade=foreground",
            "--wait=true",
            "--timeout=120s",
        ],
        None,
    )
    .await?;
    wamn_events::lifecycle::wait_reader_stopped(
        &platform.target.kubeconfig,
        &platform.target.context,
        &platform.target.namespace,
        triple,
    )
    .await
}

pub(super) async fn delete_publication(platform: &Platform, database: &str) -> anyhow::Result<()> {
    let namespace = &platform.database_namespace;
    if get(platform, namespace, "publication", database)
        .await?
        .is_some()
    {
        kubectl(
            platform,
            namespace,
            &[
                "patch",
                "publication",
                database,
                "--type",
                "merge",
                "-p",
                r#"{"spec":{"publicationReclaimPolicy":"delete"}}"#,
            ],
            None,
        )
        .await?;
        kubectl(
            platform,
            namespace,
            &[
                "delete",
                "publication",
                database,
                "--ignore-not-found",
                "--wait=true",
                "--timeout=120s",
            ],
            None,
        )
        .await
        .context("delete the environment Publication CR")?;
    }
    Ok(())
}

pub(super) async fn delete_event_certificate(
    platform: &Platform,
    project_env_key: &str,
) -> anyhow::Result<()> {
    let name = wamn_registry::name::nats_certificate(project_env_key);
    for type_ in ["certificate", "secret"] {
        kubectl(
            platform,
            &platform.target.namespace,
            &[
                "delete",
                type_,
                &name,
                "--ignore-not-found",
                "--wait=true",
                "--timeout=120s",
            ],
            None,
        )
        .await?;
    }
    Ok(())
}

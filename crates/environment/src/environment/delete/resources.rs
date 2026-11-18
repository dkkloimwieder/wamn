//! Ownership tests and deletion of resources installed outside Helm.

use serde_json::Value;
use wamn_registry::{Triple, name};

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
    let reader =
        wamn_provision::project_env_cdc_secret_name(&triple.org, &triple.project, &triple.env);
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
            labels["wamn.org"] == triple.org.as_str()
                && labels["wamn.project"] == triple.project.as_str()
                && labels["wamn.env"] == triple.env.as_str()
                && object["spec"]["dbname"] == database,
            "refused: Publication {database} does not belong to {triple}"
        );
    }
    let (org, env) = (&triple.org, &triple.env);
    let cdc = name::cdc_nats_certificate(triple);
    let recovery = name::recovery_nats_user(triple);
    for (certificate, secret, user) in [
        (
            name::environment_nats_certificate(org, env),
            name::environment_nats_secret(org, env),
            name::environment_nats_user(org, env),
        ),
        (cdc.clone(), cdc, name::cdc_nats_user(org, &triple.project, env)),
        (recovery.clone(), recovery.clone(), recovery),
    ] {
        if let Some(object) = get(platform, namespace, "certificate", &certificate).await? {
            anyhow::ensure!(
                object["spec"]["secretName"] == secret
                    && object["spec"]["commonName"] == user
                    && object["spec"]["dnsNames"] == serde_json::json!([user]),
                "refused: Certificate {certificate} does not belong to {triple}"
            );
        }
        if let Some(object) = get(platform, namespace, "secret", &secret).await? {
            anyhow::ensure!(
                object["metadata"]["annotations"]["cert-manager.io/certificate-name"]
                    == certificate,
                "refused: Secret {secret} does not belong to Certificate {certificate}"
            );
        }
    }
    Ok(())
}

pub(super) async fn delete_reader(platform: &Platform, triple: &Triple) -> anyhow::Result<()> {
    let cdc =
        wamn_provision::project_env_cdc_secret_name(&triple.org, &triple.project, &triple.env);
    if get(platform, &platform.target.namespace, "deployment", &cdc)
        .await?
        .is_some()
    {
        let release = name::environment_resource_name(&triple.org, &triple.project, &triple.env);
        wamn_platform_install::environment::update_project(
            &platform.target, &platform.chart, &release,
            &serde_json::json!({"cdcReader":{"pause":true},"publication":{"reclaimPolicy":"delete"}}),
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

pub(super) async fn delete_event_certificates(
    platform: &Platform,
    triple: &Triple,
    last: bool,
) -> anyhow::Result<()> {
    delete_secret(platform, &name::cdc_nats_certificate(triple)).await?;
    delete_secret(platform, &name::recovery_nats_user(triple)).await?;
    if last {
        let host_release = name::environment_host_name(&triple.org, &triple.env);
        wamn_kubernetes::helm::uninstall(&platform.target, &host_release).await?;
        delete_secret(
            platform,
            &name::environment_nats_secret(&triple.org, &triple.env),
        )
        .await?;
    }
    Ok(())
}

async fn delete_secret(platform: &Platform, secret: &str) -> anyhow::Result<()> {
    kubectl(
        platform,
        &platform.target.namespace,
        &["delete", "secret", secret, "--ignore-not-found"],
        None,
    )
    .await
    .map(drop)
}

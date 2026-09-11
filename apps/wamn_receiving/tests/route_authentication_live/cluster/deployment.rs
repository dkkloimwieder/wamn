//! Receiving workload publication and private native credentials.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_test_infrastructure::rendering::{
    EventIdentity, HttpClaims, HttpWorkloadInput, MaterializerInput, render_http_workload,
    render_materializer,
};

use super::super::{ENVIRONMENT, ORG, PROJECT, TENANT};
use super::{ReceivingCluster, apply, checked, kubectl, write_private};

pub(super) fn native_secrets(cluster: &ReceivingCluster) -> anyhow::Result<Vec<PathBuf>> {
    let event_path = cluster.resources.work.join("wamn-event-nats.json");
    let materializer_path = cluster.resources.work.join("wamn-materializer-nats.json");
    write_private(
        &event_path,
        &serde_json::to_vec(&json!({
            "apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"wamn-event-nats","namespace":cluster.resources.name},
            "stringData":{
                "username":cluster.broker.runtime.username,
                "password":fs::read_to_string(&cluster.broker.runtime.password_file)?,
                "org":ORG,"project":PROJECT,"environment":ENVIRONMENT,
                "stream_replicas":cluster.source.num_replicas.to_string(),
                "dup_window_secs":cluster.source.duplicate_window.as_secs().to_string(),
            },
        }))?,
    )?;
    write_private(
        &materializer_path,
        &serde_json::to_vec(&json!({
            "apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"wamn-materializer-nats","namespace":cluster.resources.name},
            "stringData":{"binding.json":fs::read_to_string(&cluster.broker.binding)?},
        }))?,
    )?;
    Ok(vec![event_path, materializer_path])
}

pub(super) async fn publish_platform_components(
    cluster: &ReceivingCluster,
) -> anyhow::Result<(String, String)> {
    let wash = install_wash(cluster).await?;
    let http = push(cluster, &wash, "flow-http", &cluster.artifacts.http).await?;
    let materializer = push(
        cluster,
        &wash,
        "materializer",
        &cluster.artifacts.materializer,
    )
    .await?;
    Ok((http, materializer))
}

pub(super) async fn publish_http(cluster: &ReceivingCluster) -> anyhow::Result<String> {
    let wash = install_wash(cluster).await?;
    push(cluster, &wash, "flow-http", &cluster.artifacts.http).await
}

async fn install_wash(cluster: &ReceivingCluster) -> anyhow::Result<PathBuf> {
    let output = checked(&mut Command::new(
        cluster.resources.repository.join("tools/install-wash"),
    ))
    .await?;
    let wash = PathBuf::from(String::from_utf8(output)?.trim());
    ensure!(
        wash.is_file(),
        "the existing wash installer must return its executable"
    );
    Ok(wash)
}

async fn push(
    cluster: &ReceivingCluster,
    wash: &Path,
    name: &str,
    bytes: &Path,
) -> anyhow::Result<String> {
    let authority = cluster
        .inputs
        .component_artifact_base
        .strip_suffix("/wamn/components")
        .context("the Receiving component repository has its declared suffix")?;
    let repository = format!("{authority}/wamn/{name}");
    let reference = format!("{repository}:{}", cluster.resources.name);
    let output = checked(
        Command::new(wash)
            .args(["-o", "json", "oci", "push", "--insecure"])
            .arg(&reference)
            .arg(bytes)
            .env(
                "DOCKER_CONFIG",
                cluster
                    .inputs
                    .registry_auth_file
                    .parent()
                    .context("the registry credential has a directory")?,
            ),
    )
    .await?;
    let result: Value = serde_json::from_slice(&output)?;
    let digest = result["data"]["digest"]
        .as_str()
        .context("the native push returned a digest")?;
    ensure!(
        result["success"] == true
            && result["data"]["success"] == true
            && digest
                .strip_prefix("sha256:")
                .is_some_and(|hex| hex.len() == 64
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())),
        "the native component push must succeed with one canonical digest"
    );
    fs::write(
        cluster.resources.evidence.join(format!("{name}-push.json")),
        &output,
    )?;
    Ok(format!("{repository}@{digest}"))
}

pub(super) async fn install_http(cluster: &ReceivingCluster, image: &str) -> anyhow::Result<()> {
    let input = HttpWorkloadInput {
        namespace: cluster.resources.name.clone(),
        image: image.to_owned(),
        route_host: cluster.inputs.route_host.clone(),
        claims: HttpClaims {
            tenant: TENANT.to_owned(),
            catalog: "default".to_owned(),
            environment: cluster.resources.name.clone(),
            project: PROJECT.to_owned(),
            schema: "receiving".to_owned(),
        },
    };
    let rendered = render_http_workload(
        &fs::read_to_string(
            cluster
                .resources
                .repository
                .join("deploy/platform/http-route-workload.example.yaml"),
        )?,
        &input,
    )?;
    let path = cluster.resources.work.join("http-route-workload.yaml");
    fs::write(&path, rendered)?;
    apply(&cluster.resources, &path).await?;
    checked(kubectl(&cluster.resources).args([
        "-n",
        &cluster.resources.name,
        "wait",
        "--for=condition=Ready",
        "workloaddeployment/flow-http",
        "--timeout=240s",
    ]))
    .await?;
    Ok(())
}

pub(super) async fn install_materializer(
    cluster: &ReceivingCluster,
    image: &str,
) -> anyhow::Result<MaterializerInput> {
    let input = MaterializerInput {
        workload: "receiving-materializer".to_owned(),
        namespace: cluster.resources.name.clone(),
        image: image.to_owned(),
        tenant: TENANT.to_owned(),
        event: EventIdentity {
            org: ORG.to_owned(),
            project: PROJECT.to_owned(),
            environment: ENVIRONMENT.to_owned(),
        },
        event_stream: cluster.source.name.clone(),
        fetch_ms: 500,
        sweep_ms: 500,
    };
    let rendered = render_materializer(
        &fs::read_to_string(
            cluster
                .resources
                .repository
                .join("deploy/platform/materializer.example.yaml"),
        )?,
        &input,
    )?;
    let path = cluster.resources.work.join("materializer.yaml");
    fs::write(&path, rendered)?;
    apply(&cluster.resources, &path).await?;
    checked(kubectl(&cluster.resources).args([
        "-n",
        &cluster.resources.name,
        "wait",
        "--for=condition=Ready",
        "workloaddeployment/receiving-materializer",
        "--timeout=240s",
    ]))
    .await?;
    Ok(input)
}

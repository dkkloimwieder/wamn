//! Receiving's three retained session modes use the composed deployed hosts.

use std::fs;
use std::path::Path;

use anyhow::Context as _;
use serde_json::Value;
use wamn_ctl::print_release_env::lookup_release_carrier;
use wamn_test_infrastructure::workload;

use super::{deployment, install_host, provision, route_cases, session_cluster, start};

#[tokio::test]
#[ignore = "builds and runs the Receiving session issuer and two native hosts"]
async fn session_hosts_preserve_the_original_caller() -> anyhow::Result<()> {
    let evidence = super::evidence_directory().await?;
    super::with_signals(&evidence, run(&evidence, false, false)).await
}

#[tokio::test]
#[ignore = "builds and runs Receiving fresh-only session selection"]
async fn fresh_only_session_selection() -> anyhow::Result<()> {
    let evidence = super::evidence_directory().await?;
    super::with_signals(&evidence, run(&evidence, true, false)).await
}

#[tokio::test]
#[ignore = "builds and runs Receiving client login and fresh session selection"]
async fn session_client_login_and_fresh_selection() -> anyhow::Result<()> {
    let evidence = super::evidence_directory().await?;
    super::with_signals(&evidence, run(&evidence, true, true)).await
}

async fn run(evidence: &Path, fresh_only: bool, session_client: bool) -> anyhow::Result<()> {
    let mut cluster = start(evidence, true, true).await?;
    if fresh_only {
        cluster.inputs.fresh_only_packages =
            Some(cluster.resources.work.join("fresh-only-packages"));
    }
    let result = async {
        let (route, _) = provision(&cluster.inputs, &cluster.artifacts).await?;
        let fixture = cluster.resources.work.join("session-host-fixture.json");
        super::super::sessions::prepare_session_host_fixture(&cluster.inputs, &fixture).await?;
        let carrier = lookup_release_carrier(
            &route.database_url,
            super::super::TENANT,
            super::super::RELEASE_ID + 1,
            &cluster.inputs.release_artifact_base,
        )
        .await?;
        let issuer = session_cluster::prepare(&cluster, &carrier, &fixture).await?;
        let document: Value = serde_json::from_slice(&fs::read(&fixture)?)?;
        let suffix = document["instance_suffix"]
            .as_str()
            .context("the selected session fixture has its instance suffix")?;
        let secrets = deployment::native_secrets(&cluster)?;
        let resources = &cluster.resources;
        install_host(
            resources,
            &cluster.inputs,
            &carrier,
            2,
            &cluster.nats_url,
            &secrets,
            &cluster.source,
            Some((&issuer, suffix)),
        )
        .await?;
        let digest = workload::image_ready(
            &resources.lifecycle,
            &resources.name,
            &resources.work,
            &resources.host_image,
            &resources.source,
            "release",
            evidence,
        )
        .await?;
        workload::hosts_ready(
            &resources.lifecycle,
            &resources.name,
            &resources.work,
            &resources.name,
            &resources.host_image,
            &digest,
            2,
            evidence,
        )
        .await?;
        let http = deployment::publish_http(&cluster).await?;
        session_cluster::assert_session(
            &cluster,
            &fixture,
            &issuer,
            &http,
            fresh_only,
            session_client,
        )
        .await?;
        super::assert_source_unchanged(resources).await
    }
    .await;
    route_cases::finish(&mut cluster, result).await
}

//! Receiving measurements retain the declared single-host starting condition.

use std::path::Path;

use wamn_control::provision_project_env::ProvisionedRoute;
use wamn_gate_harness::case_release::NAMESPACE;
use wamn_test_infrastructure::{release_chart, workload};

use super::{
    ReceivingCluster, deployment, install_release, measurement, provision, route_cases, start,
};

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl"]
async fn startup_and_steady_request_overhead() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "jq", "curl"]);
    let evidence = super::evidence_directory()?;
    super::with_signals(&evidence, async {
        let (mut cluster, _, cold) = Box::pin(prepare(&evidence)).await?;
        let result = measurement::measure_startup(&cluster, &cold).await;
        finish(&mut cluster, result).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl"]
async fn receiving_throughput() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "jq", "curl"]);
    let evidence = super::evidence_directory()?;
    super::with_signals(&evidence, async {
        let (mut cluster, route, cold) = Box::pin(prepare(&evidence)).await?;
        let result = measurement::throughput(&cluster, &cold, &route.database_url).await;
        finish(&mut cluster, result).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl"]
async fn receiving_fresh_authority() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "jq", "curl"]);
    let evidence = super::evidence_directory()?;
    super::with_signals(&evidence, async {
        let (mut cluster, route, cold) = Box::pin(prepare(&evidence)).await?;
        let result = measurement::fresh_auth(&cluster, &cold, &route.database_url).await;
        finish(&mut cluster, result).await
    })
    .await
}

async fn prepare(
    evidence: &Path,
) -> anyhow::Result<(ReceivingCluster, ProvisionedRoute, measurement::ColdHost)> {
    let cluster = start(evidence, false).await?;
    let (route, carrier) = provision(&cluster.inputs, &cluster.artifacts).await?;
    let secrets = deployment::native_secrets(&cluster)?;
    let (issuer, instance) =
        super::session_cluster::prepare_application(&cluster, &carrier).await?;
    let deployed = install_release(
        &cluster,
        &route.database_url,
        &carrier,
        &super::HostBinding {
            replicas: 1,
            nats_url: &cluster.nats_url,
            native_nats_secrets: &secrets,
            source: &cluster.source,
            session: Some((&issuer, &instance)),
        },
    )
    .await?;
    let cold = measurement::cold_host(&cluster, &deployed.name).await?;
    let resources = &cluster.resources;
    let release = deployed.release(resources, &carrier);
    let hosts =
        release_chart::hosts_ready(&release, &deployed.host_image, &deployed.runtime_digest, 1)
            .await?;
    release_chart::role_placed(&release, "http", &hosts).await?;
    workload::unknown_route(
        &resources.name,
        &resources.work,
        NAMESPACE,
        &deployed.http_service(),
        &cluster.inputs.route_host,
        evidence,
    )
    .await?;
    Ok((cluster, route, cold))
}

async fn finish(cluster: &mut ReceivingCluster, result: anyhow::Result<()>) -> anyhow::Result<()> {
    let result = match result {
        Ok(()) => super::assert_source_unchanged(&cluster.resources).await,
        Err(error) => Err(error),
    };
    route_cases::finish(cluster, result).await
}

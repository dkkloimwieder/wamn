//! Receiving measurements retain the declared single-host starting condition.

use std::path::Path;

use wamn_control::provision_project_env::ProvisionedRoute;
use wamn_test_infrastructure::workload;

use super::{
    ReceivingCluster, deployment, install_host, measurement, provision, route_cases, start,
};

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl"]
async fn startup_and_steady_request_overhead() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "jq", "curl"]);
    let evidence = super::evidence_directory()?;
    super::with_signals(&evidence, async {
        let (mut cluster, _, cold) = prepare(&evidence).await?;
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
        let (mut cluster, route, cold) = prepare(&evidence).await?;
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
        let (mut cluster, route, cold) = prepare(&evidence).await?;
        let result = measurement::fresh_auth(&cluster, &cold, &route.database_url).await;
        finish(&mut cluster, result).await
    })
    .await
}

async fn prepare(
    evidence: &Path,
) -> anyhow::Result<(ReceivingCluster, ProvisionedRoute, measurement::ColdHost)> {
    let cluster = start(evidence, false, false).await?;
    let (route, carrier) = provision(&cluster.inputs, &cluster.artifacts).await?;
    let secrets = deployment::native_secrets(&cluster)?;
    let resources = &cluster.resources;
    install_host(
        resources,
        &cluster.inputs,
        &carrier,
        0,
        &cluster.nats_url,
        &secrets,
        &cluster.source,
        None,
    )
    .await?;
    let cold = measurement::cold_host(&cluster).await?;
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
    let hosts = workload::hosts_ready(
        &workload::HostsReadyInput {
            lifecycle: &resources.lifecycle,
            cluster: &resources.name,
            work: &resources.work,
            namespace: &resources.name,
            image: &resources.host_image,
            runtime_digest: &digest,
            replicas: 1,
            evidence,
        },
    )
    .await?;
    let http = deployment::publish_http(&cluster).await?;
    deployment::install_http(&cluster, &http).await?;
    workload::http_ready(
        &resources.name,
        &resources.work,
        &resources.name,
        &hosts,
        evidence,
    )
    .await?;
    workload::unknown_route(
        &resources.name,
        &resources.work,
        &resources.name,
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

//! The default Receiving journey as stages that each run alone on one kept cluster.
//!
//! `setup` builds, creates and readies a cluster, keeps it, and prints
//! `WAMN_RECEIVING_CLUSTER=<name>`. With that variable set, `materializer`,
//! `startup` and `outage` attach to the kept cluster, and each can run again
//! after a failure without a new build or cluster. `teardown` removes the
//! cluster. `default_case` runs every stage in one process.
//!
//! An attached stage runs the test code at HEAD against images of the setup
//! commit, and refuses when the production source changed since the setup.

use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use wamn_control::print_release_env::{ReleaseCarrier, lookup_release_carrier};
use wamn_control_provision::events::source_stream_config;
use wamn_control_registry::Triple;
use wamn_test_infrastructure::event_broker;
use wamn_test_infrastructure::workload::{self, HostObservation};

use super::{
    ENVIRONMENT, ORG, PROJECT, RELEASE_ID, ReceivingCluster, TENANT, build, cdc, checked,
    deployment, install_host, journey_inputs, kubectl, materializer_case, postcommit_case,
    provision, repository_root, resources, route_cases, startup_case, write_private,
};

const KEPT: &str = "stage.json";
const CLUSTER_VARIABLE: &str = "WAMN_RECEIVING_CLUSTER";

/// What a later stage needs from the setup stage that the cluster cannot
/// tell it. The file is private to the kept cluster's working directory.
#[derive(Serialize, Deserialize)]
pub(super) struct Kept {
    pub cluster: String,
    /// The commit the images were built from.
    pub source: String,
    /// The host image identity of that commit, without test-only files.
    pub host_identity: String,
    pub host_image: String,
    pub identity_image: Option<String>,
    postgres_address: String,
    registry_authority: String,
    nats_url: String,
    route_database_url: String,
    issuer: String,
    instance: String,
}

/// The released route, its carrier and the readied Hosts, which every later
/// stage reads.
pub(super) struct Ready {
    route_database_url: String,
    carrier: ReleaseCarrier,
    issuer: String,
    instance: String,
    hosts: HostObservation,
}

/// Provision the release, start the CDC reader, and ready the Hosts,
/// flow-http, the unknown route, the cross-environment refusal and the
/// materializer.
pub(super) async fn setup(cluster: &mut ReceivingCluster) -> anyhow::Result<Ready> {
    let evidence = cluster.resources.evidence.clone();
    let (route, carrier) = provision(&cluster.inputs, &cluster.artifacts).await?;
    let replication_password = uuid::Uuid::new_v4().simple().to_string();
    let reader = cdc::configure(
        &cluster.inputs,
        &route,
        &cluster.resources.work,
        &replication_password,
        cdc::BrokerBinding {
            nats_url: &cluster.nats_url,
            nats_username: cluster.broker.publisher.username.clone(),
            nats_password_file: cluster.broker.publisher.password_file.clone(),
            provisioning: &cluster.broker.provisioning,
            consumers: &super::declared_consumers()?,
            source: &cluster.source,
        },
    )
    .await?;
    cluster.resources.reader = Some(cdc::start(reader, &evidence.join("cdc-reader.log"))?);
    reader_ready(cluster).await?;
    let secrets = deployment::native_secrets(cluster)?;
    let (issuer, instance) = super::session_cluster::prepare_application(cluster, &carrier).await?;
    install_host(
        &cluster.resources,
        &cluster.inputs,
        &carrier,
        &super::HostBinding {
            replicas: 3,
            nats_url: &cluster.nats_url,
            native_nats_secrets: &secrets,
            source: &cluster.source,
            session: Some((&issuer, &instance)),
        },
    )
    .await?;
    let hosts = hosts_ready(cluster).await?;
    let (http, materializer) = deployment::publish_platform_components(cluster).await?;
    deployment::install_http(cluster, &http).await?;
    let resources = &cluster.resources;
    let http = workload::http_ready(
        &resources.name,
        &resources.work,
        &resources.name,
        &hosts,
        &evidence,
    )
    .await?;
    workload::unknown_route(
        &resources.name,
        &resources.work,
        &resources.name,
        &cluster.inputs.route_host,
        &evidence,
    )
    .await?;
    workload::cross_environment_refused(
        &resources.name,
        &resources.work,
        &resources.name,
        &http,
        &evidence,
    )
    .await?;
    let input = deployment::install_materializer(cluster, &materializer).await?;
    workload::materializer_ready(&resources.name, &resources.work, &input, &hosts, &evidence)
        .await?;
    Ok(Ready {
        route_database_url: route.database_url,
        carrier,
        issuer,
        instance,
        hosts,
    })
}

/// Commit the seeded update and receipt through the released routes, and
/// check the materializer's causation, logs and traces.
pub(super) async fn materializer(
    cluster: &mut ReceivingCluster,
    ready: &Ready,
    order: &materializer_case::TriggerOrder,
) -> anyhow::Result<()> {
    if cluster.resources.reader.is_none() {
        let args = cdc::reader_args(
            &cluster.resources.work,
            &cluster.nats_url,
            cluster.broker.publisher.username.clone(),
            cluster.broker.publisher.password_file.clone(),
            &cluster.source,
        )?;
        cluster.resources.reader = Some(cdc::start(
            args,
            &cluster.resources.evidence.join("cdc-reader.log"),
        )?);
        reader_ready(cluster).await?;
    }
    let resources = &cluster.resources;
    let observer = event_broker::connect(&cluster.broker.observer, &cluster.nats_url).await?;
    let baseline = super::super::materializer::materializer_baseline(observer.clone()).await?;
    let endpoint = materializer_case::endpoint(cluster, "receiving-materializer-nodeport").await?;
    let (phase, update_trace, receipt_trace) =
        materializer_case::trigger(cluster, &ready.route_database_url, &endpoint, order).await?;
    super::super::materializer::assert_materializer_causation(&phase, observer, &baseline).await?;
    postcommit_case::assert_materializer_logs(cluster, &ready.hosts.pods).await?;
    wamn_test_infrastructure::traces::telemetry::collect(
        &wamn_test_infrastructure::traces::telemetry::TelemetryInput {
            cluster: &resources.name,
            work: &resources.work,
            namespace: &resources.name,
            source: &resources.source,
            tenant: super::super::TENANT,
            project: super::super::PROJECT,
            environment: super::super::ENVIRONMENT,
            requests: [("update", &update_trace), ("receipt", &receipt_trace)],
            evidence: &resources.evidence.join("telemetry"),
        },
    )
    .await?;
    for kind in ["service", "endpointslice"] {
        checked(kubectl(resources).args([
            "-n",
            &resources.name,
            "delete",
            kind,
            "receiving-materializer-nodeport",
        ]))
        .await?;
    }
    Ok(())
}

/// Start a native host beside the cluster and check its start demand and
/// progress.
pub(super) async fn startup(cluster: &ReceivingCluster, ready: &Ready) -> anyhow::Result<()> {
    startup_case::assert_startup(cluster, &ready.carrier, &ready.issuer, &ready.instance).await
}

/// Stop the scheduler, check the heartbeat guard and the supervised operator
/// exits, restore it, and restart the operator.
pub(super) async fn outage(cluster: &ReceivingCluster) -> anyhow::Result<()> {
    super::operator_recovery::assert_recovery(cluster).await?;
    fs::copy(
        cluster
            .resources
            .evidence
            .join("operator-recovery/evidence.sha256"),
        cluster.resources.evidence.join("operator-recovery.sha256"),
    )?;
    Ok(())
}

async fn reader_ready(cluster: &ReceivingCluster) -> anyhow::Result<()> {
    let observer = event_broker::connect(&cluster.broker.observer, &cluster.nats_url).await?;
    cdc::ready(
        &cluster
            .resources
            .reader
            .as_ref()
            .context("the owned CDC reader is started")?
            .1,
        &observer,
        &cluster.resources.evidence,
    )
    .await
}

async fn hosts_ready(cluster: &ReceivingCluster) -> anyhow::Result<HostObservation> {
    let resources = &cluster.resources;
    let digest = workload::image_ready(
        &resources.lifecycle,
        &resources.name,
        &resources.work,
        &resources.host_image,
        &resources.source,
        "release",
        &resources.evidence,
    )
    .await?;
    workload::hosts_ready(&workload::HostsReadyInput {
        lifecycle: &resources.lifecycle,
        cluster: &resources.name,
        work: &resources.work,
        namespace: &resources.name,
        image: &resources.host_image,
        runtime_digest: &digest,
        replicas: 3,
        evidence: &resources.evidence,
    })
    .await
}

/// Record what later stages need, and keep the cluster after this process.
async fn keep(cluster: &mut ReceivingCluster, ready: &Ready) -> anyhow::Result<()> {
    let resources = &cluster.resources;
    let system = reqwest::Url::parse(&cluster.inputs.system_pg_url)?;
    let kept = Kept {
        cluster: resources.name.clone(),
        source: resources.source.clone(),
        host_identity: resources::host_identity(&resources.repository).await?,
        host_image: resources.host_image.clone(),
        identity_image: resources.identity_image.clone(),
        postgres_address: system
            .host_str()
            .context("the system database has a host")?
            .to_owned(),
        registry_authority: cluster
            .inputs
            .component_artifact_base
            .strip_suffix("/wamn/components")
            .context("the component artifact base names the owned registry")?
            .to_owned(),
        nats_url: cluster.nats_url.clone(),
        route_database_url: ready.route_database_url.clone(),
        issuer: ready.issuer.clone(),
        instance: ready.instance.clone(),
    };
    write_private(
        &resources.work.join(KEPT),
        &serde_json::to_vec_pretty(&kept)?,
    )?;
    cluster.resources.keep();
    println!("{CLUSTER_VARIABLE}={}", cluster.resources.name);
    Ok(())
}

/// Attach to the kept cluster that `WAMN_RECEIVING_CLUSTER` names.
async fn attach(evidence: &Path, startup_burst: bool) -> anyhow::Result<(ReceivingCluster, Ready)> {
    let kept = read_kept()?;
    let repository = repository_root()?;
    let test_source = resources::committed_head(&repository).await?;
    anyhow::ensure!(
        resources::host_identity(&repository).await? == kept.host_identity,
        "the production source changed since the setup stage; run the setup stage again"
    );
    let resources = resources::attach(&repository, evidence, &kept, &test_source).await?;
    let artifacts = build::components_and_tools(&repository, evidence, startup_burst).await?;
    let broker = event_broker::load(&resources.work)?;
    let scope = Triple::new(ORG, PROJECT, ENVIRONMENT);
    let source = source_stream_config(&scope, 1, Duration::from_secs(120));
    let inputs = journey_inputs(
        &resources,
        &artifacts,
        &kept.postgres_address,
        &kept.registry_authority,
        resources.work.join("docker/config.json"),
    );
    let carrier = lookup_release_carrier(
        &kept.route_database_url,
        TENANT,
        RELEASE_ID,
        &inputs.release_artifact_base,
    )
    .await?;
    let cluster = ReceivingCluster {
        resources,
        inputs,
        artifacts,
        broker,
        nats_url: kept.nats_url,
        source,
    };
    // The readiness checks run again on attach. They also write the host and
    // flow-http records that the startup stage reads from this stage's results.
    let hosts = hosts_ready(&cluster).await?;
    let resources = &cluster.resources;
    workload::http_ready(
        &resources.name,
        &resources.work,
        &resources.name,
        &hosts,
        &resources.evidence,
    )
    .await?;
    let ready = Ready {
        route_database_url: kept.route_database_url,
        carrier,
        issuer: kept.issuer,
        instance: kept.instance,
        hosts,
    };
    Ok((cluster, ready))
}

async fn attached(
    startup_burst: bool,
    stage: impl AsyncFnOnce(&mut ReceivingCluster, &Ready) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let evidence = super::evidence_directory()?;
    Box::pin(super::with_signals(&evidence, async {
        let (mut cluster, ready) = attach(&evidence, startup_burst).await?;
        let result = stage(&mut cluster, &ready).await;
        route_cases::finish(&mut cluster, result).await
    }))
    .await
}

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl; keeps its cluster"]
async fn setup_stage() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "jq", "curl"]);
    let evidence = super::evidence_directory()?;
    Box::pin(super::with_signals(&evidence, async {
        let mut cluster = super::start_with(&evidence, false, true).await?;
        let result = async {
            let ready = setup(&mut cluster).await?;
            keep(&mut cluster, &ready).await
        }
        .await;
        route_cases::finish(&mut cluster, result).await
    }))
    .await
}

#[tokio::test]
#[ignore = "requires: WAMN_RECEIVING_CLUSTER from the setup stage"]
async fn materializer_stage() -> anyhow::Result<()> {
    attached(false, async |cluster, ready| {
        let order = materializer_case::TriggerOrder::fresh(&ready.route_database_url).await?;
        materializer(cluster, ready, &order).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires: WAMN_RECEIVING_CLUSTER from the setup stage"]
async fn startup_stage() -> anyhow::Result<()> {
    attached(true, async |cluster, ready| startup(cluster, ready).await).await
}

#[tokio::test]
#[ignore = "requires: WAMN_RECEIVING_CLUSTER from the setup stage"]
async fn outage_stage() -> anyhow::Result<()> {
    attached(false, async |cluster, _| outage(cluster).await).await
}

/// Remove the kept cluster, its containers, its private files and its image
/// tags. It reads nothing from the cluster, so it also removes a broken one.
#[tokio::test]
#[ignore = "requires: WAMN_RECEIVING_CLUSTER from the setup stage; removes the cluster"]
async fn teardown_stage() -> anyhow::Result<()> {
    let evidence = super::evidence_directory()?;
    Box::pin(super::with_signals(&evidence, async {
        let kept = read_kept()?;
        let mut resources =
            resources::attach(&repository_root()?, &evidence, &kept, "teardown").await?;
        resources.release();
        resources::remove(&mut resources).await
    }))
    .await
}

fn read_kept() -> anyhow::Result<Kept> {
    let name = std::env::var(CLUSTER_VARIABLE)
        .with_context(|| format!("{CLUSTER_VARIABLE} names the kept cluster of a setup stage"))?;
    anyhow::ensure!(
        name.starts_with("wamn-receiving-") && !name.contains('/'),
        "{CLUSTER_VARIABLE} must name a kept Receiving cluster"
    );
    Ok(serde_json::from_slice(
        &fs::read(std::env::temp_dir().join(&name).join(KEPT))
            .with_context(|| format!("read the kept state of {name}"))?,
    )?)
}

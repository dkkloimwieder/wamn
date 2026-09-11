//! The complete Receiving route, event, startup, and environment-isolation case.

use anyhow::Context as _;
use wamn_control_registry::Triple;
use wamn_test_infrastructure::{event_broker, executor, workload};

use super::{
    cdc, checked, deployment, install_host, kubectl, materializer_case, postcommit_case, provision,
    route_cases, start, startup_case,
};

#[tokio::test]
#[ignore = "builds and runs the complete Receiving application on owned local services"]
async fn released_routes_materializer_startup_and_environment_isolation() -> anyhow::Result<()> {
    let evidence = super::evidence_directory().await?;
    super::with_signals(&evidence, run(&evidence)).await
}

async fn run(evidence: &std::path::Path) -> anyhow::Result<()> {
    let mut cluster = start(evidence, false, false).await?;
    let result = async {
        let (route, carrier) = provision(&cluster.inputs, &cluster.artifacts).await?;
        let replication_password = uuid::Uuid::new_v4().simple().to_string();
        let reader = cdc::configure(
            &cluster.inputs,
            &route,
            &cluster.resources.work,
            &replication_password,
            &cluster.nats_url,
            cluster.broker.publisher.username.clone(),
            cluster.broker.publisher.password_file.clone(),
            &cluster.broker.provisioning,
            &super::declared_consumers()?,
            &cluster.source,
        )
        .await?;
        cluster.resources.reader = Some(cdc::start(reader, &evidence.join("cdc-reader.log"))?);
        let observer = event_broker::connect(&cluster.broker.observer, &cluster.nats_url).await?;
        cdc::ready(
            &cluster
                .resources
                .reader
                .as_ref()
                .context("the owned CDC reader is started")?
                .1,
            &observer,
            &evidence,
        )
        .await?;
        let resources = &cluster.resources;
        let scope = Triple::new(
            super::super::ORG,
            super::super::PROJECT,
            super::super::ENVIRONMENT,
        );
        executor::assert_idle_lifecycle(
            &executor::ExecutorInput {
                binary: &cluster.artifacts.target.join("debug/wamn-run-worker"),
                host_secrets: &cluster.inputs.host_secret_directory,
                component_artifact_base: &cluster.inputs.component_artifact_base,
                release_artifact_base: &carrier.artifact_base,
                manifest_digest: &carrier.manifest_digest.to_string(),
                registry_auth: &cluster.inputs.registry_auth_file,
                nats_url: &cluster.nats_url,
                event_scope: &scope,
                project: super::super::PROJECT,
                schema: "receiving",
                credentials: &cluster.broker.runtime,
                source: &resources.source,
                stream: &cluster.source,
            },
            &evidence,
        )
        .await?;
        let secrets = deployment::native_secrets(&cluster)?;
        install_host(
            resources,
            &cluster.inputs,
            &carrier,
            3,
            &cluster.nats_url,
            &secrets,
            &cluster.source,
            None,
        )
        .await?;
        let digest = workload::image_ready(
            &resources.lifecycle,
            &resources.name,
            &resources.work,
            &resources.host_image,
            &resources.source,
            "release",
            &evidence,
        )
        .await?;
        let hosts = workload::hosts_ready(
            &resources.lifecycle,
            &resources.name,
            &resources.work,
            &resources.name,
            &resources.host_image,
            &digest,
            3,
            &evidence,
        )
        .await?;
        let (http, materializer) = deployment::publish_platform_components(&cluster).await?;
        deployment::install_http(&cluster, &http).await?;
        let observed_http = workload::http_ready(
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
            &resources.host_image,
            &digest,
            &cluster.inputs.route_host,
            &evidence,
        )
        .await?;
        let input = deployment::install_materializer(&cluster, &materializer).await?;
        workload::materializer_ready(&resources.name, &resources.work, &input, &hosts, &evidence)
            .await?;
        let endpoint =
            materializer_case::endpoint(&cluster, "receiving-materializer-nodeport").await?;
        let (phase, update_trace, receipt_trace) =
            materializer_case::trigger(&cluster, &route, &endpoint).await?;
        super::super::materializer::assert_materializer_causation(&phase, observer).await?;
        postcommit_case::assert_materializer_logs(&cluster, &hosts.pods).await?;
        wamn_test_infrastructure::traces::telemetry::collect(
            &resources.name,
            &resources.work,
            &resources.name,
            &resources.source,
            super::super::TENANT,
            super::super::PROJECT,
            super::super::ENVIRONMENT,
            [("update", &update_trace), ("receipt", &receipt_trace)],
            &evidence.join("telemetry"),
        )
        .await?;
        checked(kubectl(resources).args([
            "-n",
            &resources.name,
            "delete",
            "service",
            "receiving-materializer-nodeport",
        ]))
        .await?;
        checked(kubectl(resources).args([
            "-n",
            &resources.name,
            "delete",
            "endpointslice",
            "receiving-materializer-nodeport",
        ]))
        .await?;
        startup_case::assert_startup(&cluster, &carrier).await?;
        workload::cross_environment_refused(
            &resources.name,
            &resources.work,
            &resources.name,
            &observed_http,
            &evidence,
        )
        .await?;
        super::operator_recovery::assert_recovery(&cluster).await?;
        std::fs::copy(
            evidence.join("operator-recovery/evidence.sha256"),
            evidence.join("operator-recovery.sha256"),
        )?;
        super::assert_source_unchanged(resources).await
    }
    .await;
    route_cases::finish(&mut cluster, result).await
}

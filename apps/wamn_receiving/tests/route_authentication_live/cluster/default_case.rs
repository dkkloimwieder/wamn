//! The complete Receiving route, event, startup, and environment-isolation case.

use anyhow::Context as _;
use wamn_control_registry::Triple;
use wamn_test_infrastructure::{event_broker, executor, workload};

use super::{
    cdc, checked, deployment, install_host, kubectl, materializer_case, postcommit_case, provision,
    route_cases, start_with, startup_case,
};

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl"]
async fn released_routes_materializer_startup_and_environment_isolation() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "jq", "curl"]);
    let evidence = super::evidence_directory()?;
    Box::pin(super::with_signals(&evidence, run(&evidence))).await
}

async fn run(evidence: &std::path::Path) -> anyhow::Result<()> {
    let mut cluster = start_with(evidence, false, false, true).await?;
    let result = async {
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
        let observer = event_broker::connect(&cluster.broker.observer, &cluster.nats_url).await?;
        cdc::ready(
            &cluster
                .resources
                .reader
                .as_ref()
                .context("the owned CDC reader is started")?
                .1,
            &observer,
            evidence,
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
            evidence,
        )
        .await?;
        let secrets = deployment::native_secrets(&cluster)?;
        install_host(
            resources,
            &cluster.inputs,
            &carrier,
            &super::HostBinding {
                replicas: 3,
                nats_url: &cluster.nats_url,
                native_nats_secrets: &secrets,
                source: &cluster.source,
                session: None,
            },
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
        let hosts = workload::hosts_ready(&workload::HostsReadyInput {
            lifecycle: &resources.lifecycle,
            cluster: &resources.name,
            work: &resources.work,
            namespace: &resources.name,
            image: &resources.host_image,
            runtime_digest: &digest,
            replicas: 3,
            evidence,
        })
        .await?;
        let (http, materializer) = deployment::publish_platform_components(&cluster).await?;
        deployment::install_http(&cluster, &http).await?;
        let observed_http = workload::http_ready(
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
        let input = deployment::install_materializer(&cluster, &materializer).await?;
        workload::materializer_ready(&resources.name, &resources.work, &input, &hosts, evidence)
            .await?;
        let endpoint =
            materializer_case::endpoint(&cluster, "receiving-materializer-nodeport").await?;
        let (phase, update_trace, receipt_trace) =
            materializer_case::trigger(&cluster, &route, &endpoint).await?;
        super::super::materializer::assert_materializer_causation(&phase, observer).await?;
        postcommit_case::assert_materializer_logs(&cluster, &hosts.pods).await?;
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
                evidence: &evidence.join("telemetry"),
            },
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
            evidence,
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

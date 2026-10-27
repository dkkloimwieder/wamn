//! Unchanged-overlay routes and materializer replay on an owned Receiving cluster.

use std::fs;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_gate_harness::journey::PostcommitPhase;
use wamn_test_infrastructure::{event_broker, release_chart, workload};

use super::{
    NAMESPACE, ReceivingCluster, cdc, checked, deployment, install_release, kubectl,
    materializer_case, provision, resources, seed, start,
};

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm"]
async fn baseline_overlay_and_materializer_progress() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm"]);
    let evidence = super::evidence_directory()?;
    Box::pin(super::with_signals(&evidence, run(&evidence))).await
}

async fn run(evidence: &std::path::Path) -> anyhow::Result<()> {
    let mut cluster = start(evidence, true).await?;
    let outcome = exercise(&mut cluster).await;
    if outcome.is_err() {
        resources::capture_failure(&cluster.resources).await;
    }
    let cleanup = resources::remove(&mut cluster.resources).await;
    let result = match (outcome, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("Receiving cleanup also failed: {cleanup:#}")))
        }
    };
    fs::write(
        evidence.join("verdict.json"),
        serde_json::to_vec_pretty(&json!({
            "schema":"wamn-receiving-postcommit/v1",
            "verdict":if result.is_ok() { "pass" } else { "fail" },
            "failure":result.as_ref().err().map(|error| format!("{error:#}")),
            "teardown_leftovers":cluster.resources.teardown_leftovers,
            "passing_arms":if result.is_ok() { vec!["unchanged-overlay-routes", "schema-requirements-observed",
                "duplicate-handler-delivery", "retry-exhaustion-advisory", "independent-event-progress"] } else { Vec::new() },
        }))?,
    )?;
    result?;
    if let Some((candidate, _)) = &cluster.resources.candidate {
        wamn_control::delivery::report_candidate_success(candidate)?;
    }
    Ok(())
}

async fn exercise(cluster: &mut ReceivingCluster) -> anyhow::Result<()> {
    let (route, carrier) = provision(cluster).await?;
    seed(&route).await?;
    // The unchanged overlay of the candidate against the fresh installation
    // of its base.
    let (_, manifest) = cluster.candidate()?;
    let overlay = manifest
        .components
        .iter()
        .find(|component| component.package_id == super::super::OVERLAY_PACKAGE_ID)
        .context("the candidate carries the overlay component")?;
    super::super::overlay_compatibility::unchanged_overlay(
        &cluster
            .resources
            .evidence
            .join("overlay-compatibility.json"),
        &route.database_url,
        cluster
            .packages
            .get(super::super::OVERLAY_PACKAGE_ID)
            .context("the candidate carries the overlay package")?,
        overlay.descriptor.component_digest.as_str(),
    )
    .await?;
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
            consumers: &cluster.consumers,
            source: &cluster.source,
        },
    )
    .await?;
    cluster.resources.reader = Some(cdc::start(
        reader,
        &cluster.resources.evidence.join("cdc-reader.log"),
    )?);
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
    .await?;
    let secrets = deployment::native_secrets(cluster)?;
    let (issuer, instance) = super::session_cluster::prepare_application(cluster, &carrier).await?;
    let deployed = install_release(
        cluster,
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
    let resources = &cluster.resources;
    assert_gates_layers(resources).await?;
    let gates_evidence = resources.evidence.join("gates-image");
    fs::create_dir(&gates_evidence)?;
    workload::image_ready(
        &resources.lifecycle,
        &resources.name,
        &resources.work,
        resources
            .gates_image
            .as_deref()
            .context("the test built its gates image")?,
        "release",
        &gates_evidence,
    )
    .await?;
    let release = deployed.release(resources, &carrier);
    let hosts =
        release_chart::hosts_ready(&release, &deployed.host_image, &deployed.runtime_digest, 3)
            .await?;
    release_chart::role_placed(&release, "http", &hosts).await?;
    let endpoint = materializer_case::endpoint(
        cluster,
        NAMESPACE,
        &deployed.http_service(),
        "receiving-postcommit-nodeport",
    )
    .await?;
    assert_unknown_route(cluster, &endpoint).await?;
    release_chart::role_placed(&release, "materializer", &hosts).await?;
    let baseline = super::super::materializer::materializer_baseline(observer.clone()).await?;
    let (phase, update_trace, receipt_trace) = materializer_case::trigger(
        cluster,
        &route.database_url,
        &endpoint,
        &materializer_case::TriggerOrder::seeded(),
    )
    .await?;
    super::super::materializer::assert_materializer_causation(&phase, observer.clone(), &baseline)
        .await?;
    cluster.inputs.materializer = Some(phase);
    assert_materializer_logs(cluster, NAMESPACE, &hosts.pods).await?;
    wamn_test_infrastructure::telemetry::collect(
        &wamn_test_infrastructure::telemetry::TelemetryInput {
            cluster: &resources.name,
            work: &resources.work,
            namespace: NAMESPACE,

            env_key: super::super::identity().env_key().as_str(),
            project: super::super::identity().project.as_str(),
            environment: super::super::identity().environment.as_str(),
            requests: [("update", &update_trace), ("receipt", &receipt_trace)],
            evidence: &resources.evidence.join("telemetry"),
        },
    )
    .await?;
    cluster.inputs.postcommit = Some(PostcommitPhase {
        route_endpoint: endpoint,
        evidence_file: resources.evidence.join("postcommit.json"),
        kubeconfig: resources.work.join("kubeconfig"),
        context: format!("kind-{}", resources.name),
        namespace: NAMESPACE.to_owned(),
        materializer_workload: wamn_control::release_chart::workload_name(
            &deployed.name,
            wamn_control::release_chart::RoleName::Materializer,
        ),
        statement_timeout_ms: statement_timeout(resources, &deployed.name).await?,
    });
    let replay = event_broker::connect(&cluster.broker.runtime, &cluster.nats_url).await?;
    let wiring = wamn_control::author_workflow::read_wiring_document(
        &super::super::overlay_package_root()
            .join("publication/wirings/quality_create_inspection.json"),
    )?;
    let graph = serde_json::to_value(&wiring)?;
    tokio::time::timeout(
        Duration::from_secs(600),
        super::super::postcommit::assert_postcommit(
            &cluster.inputs,
            &wamn_test_infrastructure::operations::sealed_in(
                &cluster
                    .packages
                    .get(super::super::OVERLAY_PACKAGE_ID)
                    .context("the candidate carries the Acme package")?
                    .manifest()?,
                "client-acme-receiving:quality/create-inspection",
            )?,
            &graph,
            &format!("kind-{}", resources.name),
            NAMESPACE,
            &observer,
            &replay,
        ),
    )
    .await
    .context("the retained materializer replay test exceeded 600 seconds")?
}

pub(super) async fn assert_gates_layers(resources: &resources::Resources) -> anyhow::Result<()> {
    let gates = resources
        .gates_image
        .as_ref()
        .context("the test built its gates image")?;
    let host: Vec<String> = serde_json::from_slice(
        &checked(Command::new(&resources.lifecycle).args(["image-layers", &resources.host_image]))
            .await?,
    )?;
    let gates: Vec<String> = serde_json::from_slice(
        &checked(Command::new(&resources.lifecycle).args(["image-layers", gates])).await?,
    )?;
    ensure!(
        gates.starts_with(&host),
        "the gates image must inherit the exact host image layers"
    );
    fs::write(
        resources.evidence.join("image-layers.json"),
        serde_json::to_vec_pretty(&json!({"host":host,"gates":gates}))?,
    )?;
    Ok(())
}

pub(super) async fn assert_unknown_route(
    cluster: &ReceivingCluster,
    endpoint: &str,
) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut attempt = 0;
    let response = loop {
        match client
            .get(format!("{endpoint}/no-such-route"))
            .header("Host", &cluster.inputs.route_host)
            .send()
            .await
        {
            Ok(response) => break response,
            Err(error)
                if error.is_connect() && attempt < 15 && tokio::time::Instant::now() < deadline =>
            {
                attempt += 1;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(error) => return Err(error.into()),
        }
    };
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .context("the route refusal has its content type")?
        .to_str()?
        .to_owned();
    let body = response.bytes().await?;
    fs::write(
        cluster.resources.evidence.join("flow-http-probe.body"),
        &body,
    )?;
    ensure!(
        status == reqwest::StatusCode::NOT_FOUND
            && content_type == "application/json"
            && body.as_ref() == br#"{"error":{"code":"route-not-found"}}"#,
        "the released route must keep its exact typed unknown-route refusal"
    );
    fs::write(
        cluster.resources.evidence.join("flow-http-response.json"),
        serde_json::to_vec_pretty(&json!({
            "status":status.as_u16(),"content_type":content_type,"host":cluster.inputs.route_host,
            "body":serde_json::from_slice::<Value>(&body)?,
        }))?,
    )?;
    Ok(())
}

pub(super) async fn assert_materializer_logs(
    cluster: &ReceivingCluster,
    namespace: &str,
    pods: &Value,
) -> anyhow::Result<()> {
    let mut log = Vec::new();
    for pod in pods["items"]
        .as_array()
        .context("the observed host pods are present")?
    {
        log.extend(
            checked(
                kubectl(&cluster.resources).args([
                    "-n",
                    namespace,
                    "logs",
                    pod["metadata"]["name"]
                        .as_str()
                        .context("the observed host pod has a name")?,
                    "-c",
                    "host",
                ]),
            )
            .await?,
        );
    }
    fs::write(
        cluster.resources.evidence.join("materializer-host.log"),
        &log,
    )?;
    ensure!(
        !String::from_utf8_lossy(&log).lines().any(|line| {
            line.contains("router delivery did not settle")
                || line
                    .split_once("wamn::materializer ")
                    .is_some_and(|(_, tail)| tail.starts_with("REFUSED") || tail.contains("failed"))
        }),
        "the production materializer logged a refusal or unsettled delivery"
    );
    Ok(())
}

async fn statement_timeout(resources: &resources::Resources, group: &str) -> anyhow::Result<u64> {
    let bytes = checked(kubectl(resources).args([
        "-n",
        NAMESPACE,
        "get",
        "deployment",
        &format!("hostgroup-{group}"),
        "-o",
        "json",
    ]))
    .await?;
    fs::write(
        resources
            .evidence
            .join("postcommit-host-configuration.json"),
        &bytes,
    )?;
    let deployment: Value = serde_json::from_slice(&bytes)?;
    let mut values = Vec::new();
    for container in deployment["spec"]["template"]["spec"]["containers"]
        .as_array()
        .context("the deployed host has containers")?
        .iter()
        .filter(|container| container["name"] == "host")
    {
        if let Some(env) = container["env"].as_array() {
            values.extend(
                env.iter()
                    .filter(|value| value["name"] == "WAMN_PG_STATEMENT_TIMEOUT_MS"),
            );
        }
    }
    let value = match values.as_slice() {
        [] => 5_000,
        [value] => value["value"]
            .as_str()
            .context("the deployed statement timeout must be literal")?
            .parse::<u64>()?,
        _ => anyhow::bail!("ambiguous host statement timeout"),
    };
    ensure!(
        value > 0 && value <= 10_000,
        "the deployed statement timeout must retain its existing bound"
    );
    Ok(value)
}

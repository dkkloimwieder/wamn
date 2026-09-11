//! Unchanged-overlay routes and materializer replay on an owned Receiving cluster.

use std::fs;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_gate_harness::journey::{BaseCandidate, CompatibilityPhase, PostcommitPhase};
use wamn_test_infrastructure::{event_broker, workload};

use super::{
    ReceivingCluster, cdc, checked, deployment, install_host, kubectl, materializer_case,
    provision, resources, start,
};

#[tokio::test]
#[ignore = "builds and runs the complete baseline Receiving cluster on owned local services"]
async fn baseline_overlay_and_materializer_progress() -> anyhow::Result<()> {
    run(BaseCandidate::Baseline).await
}

#[tokio::test]
#[ignore = "builds and runs the complete additive Receiving cluster on owned local services"]
async fn additive_overlay_and_materializer_progress() -> anyhow::Result<()> {
    run(BaseCandidate::Additive).await
}

async fn run(base: BaseCandidate) -> anyhow::Result<()> {
    let evidence = super::evidence_directory().await?;
    super::with_signals(&evidence, run_selected(base, &evidence)).await
}

pub(super) async fn run_selected(
    base: BaseCandidate,
    evidence: &std::path::Path,
) -> anyhow::Result<()> {
    let mut cluster = start(evidence, true, false).await?;
    cluster.inputs.overlay_compatibility = Some(CompatibilityPhase {
        base,
        package_directory: cluster.resources.work.join("compatibility-packages"),
        evidence_file: evidence.join("overlay-compatibility.json"),
    });
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
            "schema":"wamn-receiving-postcommit/v1", "source":cluster.resources.source, "base":base,
            "verdict":if result.is_ok() { "pass" } else { "fail" },
            "failure":result.as_ref().err().map(|error| format!("{error:#}")),
            "passing_arms":if result.is_ok() { vec!["unchanged-overlay-routes", "schema-requirements-observed",
                "duplicate-handler-delivery", "retry-exhaustion-advisory", "independent-event-progress"] } else { Vec::new() },
        }))?,
    )?;
    result
}

async fn exercise(cluster: &mut ReceivingCluster) -> anyhow::Result<()> {
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
    install_host(
        &cluster.resources,
        &cluster.inputs,
        &carrier,
        3,
        &cluster.nats_url,
        &secrets,
        &cluster.source,
        None,
    )
    .await?;
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
        &resources.source,
        "release",
        &gates_evidence,
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
        &resources.evidence,
    )
    .await?;
    let (http_image, materializer_image) = deployment::publish_platform_components(cluster).await?;
    deployment::install_http(cluster, &http_image).await?;
    workload::http_ready(
        &resources.name,
        &resources.work,
        &resources.name,
        &hosts,
        &resources.evidence,
    )
    .await?;
    let endpoint = materializer_case::endpoint(cluster, "receiving-postcommit-nodeport").await?;
    assert_unknown_route(cluster, &endpoint).await?;
    let materializer = deployment::install_materializer(cluster, &materializer_image).await?;
    workload::materializer_ready(
        &resources.name,
        &resources.work,
        &materializer,
        &hosts,
        &resources.evidence,
    )
    .await?;
    let (phase, update_trace, receipt_trace) =
        materializer_case::trigger(cluster, &route, &endpoint).await?;
    super::super::materializer::assert_materializer_causation(&phase, observer.clone()).await?;
    cluster.inputs.materializer = Some(phase);
    assert_materializer_logs(cluster, &hosts.pods).await?;
    wamn_test_infrastructure::traces::telemetry::collect(
        &resources.name,
        &resources.work,
        &resources.name,
        &resources.source,
        super::super::TENANT,
        super::super::PROJECT,
        super::super::ENVIRONMENT,
        [("update", &update_trace), ("receipt", &receipt_trace)],
        &resources.evidence.join("telemetry"),
    )
    .await?;
    cluster.inputs.postcommit = Some(PostcommitPhase {
        route_endpoint: endpoint,
        evidence_file: resources.evidence.join("postcommit.json"),
        kubeconfig: resources.work.join("kubeconfig"),
        context: format!("kind-{}", resources.name),
        namespace: resources.name.clone(),
        materializer_workload: "receiving-materializer".to_owned(),
        source_commit: resources.source.clone(),
        statement_timeout_ms: statement_timeout(resources).await?,
    });
    let replay = event_broker::connect(&cluster.broker.runtime, &cluster.nats_url).await?;
    tokio::time::timeout(
        Duration::from_secs(600),
        super::super::postcommit::assert_postcommit(
            &cluster.inputs,
            &format!("kind-{}", resources.name),
            &resources.name,
            &observer,
            &replay,
        ),
    )
    .await
    .context("the retained materializer replay test exceeded 600 seconds")??;
    super::assert_source_unchanged(resources).await
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
                    &cluster.resources.name,
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

async fn statement_timeout(resources: &resources::Resources) -> anyhow::Result<u64> {
    let bytes = checked(kubectl(resources).args([
        "-n",
        &resources.name,
        "get",
        "deployment",
        "hostgroup-default",
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

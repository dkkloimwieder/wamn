//! The host serves requests that do not need NATS during an outage.
//! This target stops the platform NATS server. Run it alone.
//!
//! A port-forward reaches the host pod directly. The test logs the web edge status
//! during the outage as stock runtime-operator behaviour. After NATS returns, the
//! test checks operator readiness, host reconnect, edge routing, and a new deployment.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Context as _;
use common::host::{
    application_objects, condition, declare, fixture, host_object, ready, ready_host_pod, reapply,
    until,
};
use common::{Application, PortForward, Routes};
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kube::Api;
use kube::api::{ListParams, Patch, PatchParams};
use serde::Deserialize as _;
use serde_json::json;

const PLATFORM: &str = "platform";
const NATS: &str = "wamn-nats";
const NATS_POD: &str = "wamn-nats-0";
/// The pod label of the stock runtime-operator Deployment.
const OPERATOR: &str = "wasmcloud.com/name=runtime-operator";
/// A second application with the fixture package, applied after the outage.
const SECOND: &str = "second_fixture";
/// How long one host pod waits to become the only Ready one after `wamn env apply`.
const HOST_DEADLINE: Duration = Duration::from_secs(300);
/// How long the NATS pod takes to stop or to become Ready again.
const NATS_DEADLINE: Duration = Duration::from_secs(300);
/// How long the operator takes to change the Host object's Reporting condition.
const REPORTING_DEADLINE: Duration = Duration::from_secs(300);
/// How long the operator takes to exit without NATS, or to become Ready again after
/// its restart backoff.
const OPERATOR_DEADLINE: Duration = Duration::from_secs(300);

/// During the outage, the host pod serves a call that needs no NATS.
/// The test logs the web edge status during the outage.
/// When NATS returns, the operator is Ready and the Host reports a newer `lastSeen`.
/// The web edge serves a call and the second `WorkloadDeployment` is Ready.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn the_host_serves_through_a_nats_outage_and_recovers() -> anyhow::Result<()> {
    let application = Application::prepare(&fixture()).await?;
    let environment = &application.environment;
    let client = &environment.org.cluster.client;
    let pod = ready_host_pod(environment, HOST_DEADLINE).await?;
    let pod = &pod;
    let reporting = || async move {
        let host = host_object(client, pod).await?;
        Ok(host.and_then(|host| {
            (condition(&host, "Reporting") == Some("True"))
                .then(|| Time::deserialize(&host.data["status"]["lastSeen"]).ok())
        }))
    };
    let before = until(REPORTING_DEADLINE, "the Host object reports", reporting)
        .await?
        .context("the reporting Host object has no lastSeen")?;

    let namespace = pod
        .metadata
        .namespace
        .as_deref()
        .context("host pod namespace")?;
    let pod_name = pod.metadata.name.as_deref().context("host pod name")?;
    let port = pod
        .spec
        .iter()
        .flat_map(|spec| &spec.containers)
        .flat_map(|container| container.ports.iter().flatten())
        .find(|port| port.name.as_deref() == Some("http"))
        .context("the host pod has no named http port")?;
    let forward = PortForward::start(
        Api::namespaced(client.clone(), namespace),
        pod_name,
        u16::try_from(port.container_port).context("invalid host HTTP port")?,
    )
    .await?;
    let edge = &application.routes;
    let direct = Routes {
        endpoint: format!("http://{}", forward.address),
        host: edge.host.clone(),
        bearer: edge.bearer.clone(),
        client: edge.client.clone(),
    };

    let pods = Api::<Pod>::namespaced(client.clone(), PLATFORM);
    let pods = &pods;
    let restarts = operator_restarts(pods).await?;
    anyhow::ensure!(
        !restarts.is_empty(),
        "no runtime-operator pod in {PLATFORM}"
    );

    let sets = Api::<StatefulSet>::namespaced(client.clone(), PLATFORM);
    let restore = Restore(sets.clone());
    scale(&sets, 0).await?;
    until(NATS_DEADLINE, "the NATS pod stops", || async {
        Ok(pods.get_opt(NATS_POD).await?.is_none().then_some(()))
    })
    .await?;
    // The stock behaviour: the runtime-operator exits while NATS is unreachable, so
    // its pod restarts or is replaced.
    until(
        OPERATOR_DEADLINE,
        "the runtime-operator exits during the NATS outage",
        || async { Ok((operator_restarts(pods).await? != restarts).then_some(())) },
    )
    .await?;
    let path = "/widget/create";
    let request = json!({
        "request_id": "outage-call",
        "id": uuid::Uuid::now_v7().to_string(),
        "idempotency_key": "outage-call",
        "code": "standard"
    });
    let edge_call = edge
        .client
        .post(format!("{}{path}", edge.endpoint))
        .header("Host", &edge.host)
        .bearer_auth(&edge.bearer)
        .json(std::slice::from_ref(&request));
    let created = direct.post(path, request).await;
    match edge_call.send().await {
        Ok(response) => eprintln!(
            "web edge during NATS outage (stock runtime-operator behaviour) answered {}",
            response.status()
        ),
        Err(error) => eprintln!("web edge request during NATS outage failed: {error:#}"),
    }

    let created = created.context("a direct call to the host pod during the NATS outage")?;
    anyhow::ensure!(
        created.get("value").is_some(),
        "the host pod creates a widget during the NATS outage: {created}"
    );
    drop(forward);

    scale(&sets, 1).await?;
    until(
        NATS_DEADLINE,
        "the NATS pod is Ready after the outage",
        || async { Ok(pods.get_opt(NATS_POD).await?.filter(ready).map(drop)) },
    )
    .await?;
    until(
        OPERATOR_DEADLINE,
        "the runtime-operator is Ready after NATS returns",
        || async {
            let operators = pods.list(&ListParams::default().labels(OPERATOR)).await?;
            Ok((!operators.items.is_empty() && operators.items.iter().all(ready)).then_some(()))
        },
    )
    .await?;
    until(
        REPORTING_DEADLINE,
        "the Host object reports with a newer lastSeen after NATS returns",
        || async { Ok(reporting().await?.flatten().filter(|seen| *seen > before)) },
    )
    .await?;
    let id = uuid::Uuid::now_v7().to_string();
    application
        .routes
        .create("recovery-call", "recovery-call", &id, "priority")
        .await
        .context("a call after NATS returns")?;
    drop(restore);

    reapply(environment, |file| declare(file, SECOND, "second")).await?;
    environment.deploy(&fixture(), SECOND).await?;
    until(
        HOST_DEADLINE,
        "the WorkloadDeployment applied after the outage is Ready",
        || async {
            let deployments = application_objects(
                environment,
                "WorkloadDeployment",
                "workloaddeployments",
                SECOND,
            )
            .await?;
            Ok((!deployments.is_empty()
                && deployments
                    .iter()
                    .all(|deployment| condition(deployment, "Ready") == Some("True")))
            .then_some(()))
        },
    )
    .await
}

/// The container restarts of each runtime-operator pod, by pod uid. A changed map
/// means that the operator exited.
async fn operator_restarts(pods: &Api<Pod>) -> anyhow::Result<BTreeMap<String, i32>> {
    pods.list(&ListParams::default().labels(OPERATOR))
        .await?
        .items
        .into_iter()
        .map(|pod| {
            let restarts = pod
                .status
                .iter()
                .flat_map(|status| status.container_statuses.iter().flatten())
                .map(|container| container.restart_count)
                .sum();
            let uid = pod
                .metadata
                .uid
                .context("a runtime-operator pod has no uid")?;
            Ok((uid, restarts))
        })
        .collect()
}

async fn scale(sets: &Api<StatefulSet>, replicas: u32) -> anyhow::Result<()> {
    sets.patch_scale(
        NATS,
        &PatchParams::default(),
        &Patch::Merge(json!({"spec": {"replicas": replicas}})),
    )
    .await?;
    Ok(())
}

/// Puts the NATS StatefulSet back to one replica when the test ends, also on a failure.
struct Restore(Api<StatefulSet>);

impl Drop for Restore {
    fn drop(&mut self) {
        let restored = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(scale(&self.0, 1))
        });
        if let Err(error) = restored {
            eprintln!("restore {NATS} to one replica: {error:#}");
        }
    }
}

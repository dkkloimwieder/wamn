//! NATS outage recovery (Epic 15 ruling C10). This target stops the platform NATS
//! server, so it disturbs every other test on the cluster: run it alone.

mod common;

use std::time::Duration;

use anyhow::Context as _;
use common::Application;
use common::host::{
    application_objects, condition, declare, fixture, host_object, ready, ready_host_pod, reapply,
    until,
};
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kube::Api;
use kube::api::{Patch, PatchParams};
use serde::Deserialize as _;
use serde_json::json;

const PLATFORM: &str = "platform";
const NATS: &str = "wamn-nats";
const NATS_POD: &str = "wamn-nats-0";
/// A second application with the fixture package, applied after the outage.
const SECOND: &str = "second_fixture";
/// How long one host pod waits to become the only Ready one after `wamn env apply`.
const HOST_DEADLINE: Duration = Duration::from_secs(300);
/// How long the NATS pod takes to stop or to become Ready again.
const NATS_DEADLINE: Duration = Duration::from_secs(300);
/// How long the operator takes to change the Host object's Reporting condition.
const REPORTING_DEADLINE: Duration = Duration::from_secs(300);

/// During a NATS outage the Host object stops reporting and the host still serves a call.
/// After it, the Host reports again, and a new application becomes Ready on the host.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn the_host_serves_through_a_nats_outage_and_recovers() -> anyhow::Result<()> {
    let application = Application::prepare(&fixture()).await?;
    let environment = &application.environment;
    let client = &environment.org.cluster.client;
    let pod = ready_host_pod(environment, HOST_DEADLINE).await?;
    let pod = &pod;
    let reporting = |want: bool| async move {
        let host = host_object(client, pod).await?;
        Ok(host.and_then(|host| {
            let reports = condition(&host, "Reporting") == Some("True");
            let seen = Time::deserialize(&host.data["status"]["lastSeen"]).ok();
            (reports == want).then_some(seen)
        }))
    };
    let before = until(REPORTING_DEADLINE, "the Host object reports", || {
        reporting(true)
    })
    .await?
    .context("the reporting Host object has no lastSeen")?;

    let sets = Api::<StatefulSet>::namespaced(client.clone(), PLATFORM);
    let restore = Restore(sets.clone());
    scale(&sets, 0).await?;
    let pods = Api::<Pod>::namespaced(client.clone(), PLATFORM);
    until(NATS_DEADLINE, "the NATS pod stops", || async {
        Ok(pods.get_opt(NATS_POD).await?.is_none().then_some(()))
    })
    .await?;
    until(
        REPORTING_DEADLINE,
        "the Host object leaves Reporting=True",
        || reporting(false),
    )
    .await?;
    let id = uuid::Uuid::now_v7().to_string();
    application
        .routes
        .create("outage-call", "outage-call", &id, "standard")
        .await
        .context("a call during the NATS outage")?;

    scale(&sets, 1).await?;
    until(NATS_DEADLINE, "the NATS pod is Ready", || async {
        Ok(pods.get_opt(NATS_POD).await?.filter(ready).map(drop))
    })
    .await?;
    until(
        REPORTING_DEADLINE,
        "the Host object reports with a newer lastSeen",
        || async {
            Ok(reporting(true)
                .await?
                .flatten()
                .filter(|seen| *seen > before))
        },
    )
    .await?;
    drop(restore);

    reapply(environment, |file| declare(file, SECOND, "second")).await?;
    environment.deploy(&fixture(), SECOND).await?;
    until(
        HOST_DEADLINE,
        "the new WorkloadDeployment is Ready",
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

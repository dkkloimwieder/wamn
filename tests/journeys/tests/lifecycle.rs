//! Application rollback and environment deletion through product commands.

mod common;

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use common::host::{FIXTURE, fixture, ready, runtime};
use common::request::{output, until, wamn};
use common::{
    Application, Environment, Org, Routes, active_project_cluster, observe_project_cluster,
    probe_image, run_job,
};
use futures_util::future::try_join_all;
use hyper::StatusCode;
use k8s_openapi::api::core::v1::{Container, Pod, PodSpec, PodTemplateSpec, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::Api;
use kube::api::{ApiResource, DynamicObject, GroupVersionKind, ListParams};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio::time::{Instant, timeout};
use wamn_http_client::{HttpClient, Redirects};
use wamn_registry::{name, resource_names};

/// The CLI allows 16 minutes for a Job after submission, including the host drain.
const COMMAND_DEADLINE: Duration = Duration::from_mins(17);
/// Source deployment also builds and publishes the fixture.
const DEPLOY_DEADLINE: Duration = Duration::from_mins(20);
const OBSERVE_DEADLINE: Duration = Duration::from_mins(5);
const CALL_DEADLINE: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn rollback_serves_every_call_through_web() -> anyhow::Result<()> {
    let v1 = rollback_source("WAMN_ROLLBACK_FIXTURE_V1_SOURCE")?;
    let v2 = rollback_source("WAMN_ROLLBACK_FIXTURE_V2_SOURCE")?;
    let mut application = within(
        DEPLOY_DEADLINE,
        "prepare rollback fixture version 1",
        Application::prepare(&v1),
    )
    .await?;
    application.routes.client = HttpClient::builder()
        .timeout(CALL_DEADLINE)
        .redirects(Redirects::None)
        .build()?;
    let environment = &application.environment;
    let routes = &application.routes;
    let widget = create_widget(routes).await?;
    require_version(routes, &widget, Version::One).await?;

    let triple = &environment.triple;
    let release = resource_names::application_release_name(
        &triple.org,
        &triple.project,
        &triple.env,
        &FIXTURE.parse()?,
    );
    let workload = format!("{release}-http");
    let workloads = runtime(
        &environment.org.cluster.client,
        triple.org.as_str(),
        "WorkloadDeployment",
        "workloaddeployments",
    );
    let (revision, original) = within(OBSERVE_DEADLINE, "read version 1 workload", async {
        let object = workloads.get(&workload).await?;
        let revision: NonZeroU32 = object
            .metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get("wamn.release-revision"))
            .context("the version 1 workload has no Helm revision")?
            .parse()
            .context("parse the version 1 Helm revision")?;
        Ok((revision, component_digests(&object)?))
    })
    .await?;

    within(
        DEPLOY_DEADLINE,
        "deploy rollback fixture version 2",
        environment.deploy(&v2, FIXTURE),
    )
    .await?;
    let second = within(OBSERVE_DEADLINE, "read version 2 workload", async {
        component_digests(&workloads.get(&workload).await?)
    })
    .await?;
    ensure!(
        original
            .iter()
            .any(|(name, digest)| { second.get(name).is_some_and(|deployed| deployed != digest) }),
        "the fixture versions need different component digests: {original:?} and {second:?}"
    );
    require_version(routes, &widget, Version::Two).await?;

    let mut command = environment.org.command();
    command
        .args([
            "rollback",
            FIXTURE,
            "--revision",
            &revision.to_string(),
            "--env",
        ])
        .arg(&environment.file);
    let (complete, completion) = oneshot::channel();
    let rollout = async {
        let result = async {
            within(COMMAND_DEADLINE, "roll back the fixture", output(command)).await?;
            let waiting = "version 1 WorkloadDeployment becomes Ready";
            within(
                OBSERVE_DEADLINE,
                waiting,
                until(OBSERVE_DEADLINE, waiting, || async {
                    let Some(object) = workloads.get_opt(&workload).await? else {
                        return Ok(None);
                    };
                    Ok(
                        (workload_ready(&object) && component_digests(&object)? == original)
                            .then_some(()),
                    )
                }),
            )
            .await
        }
        .await;
        let _ = complete.send(result.is_ok());
        result
    };
    // Poll the client first so its first request starts before the rollback command.
    let ((calls, restored), rollout) =
        tokio::join!(biased; call_during_rollback(routes, &widget, completion), rollout);
    let failures: Vec<_> = calls
        .iter()
        .filter_map(|call| {
            let error = call.result.as_ref().err()?;
            let status = call
                .status
                .map_or_else(|| "unavailable".into(), |code| code.to_string());
            Some(format!(
                "+{:?}, status {status}, error {error:#}",
                call.time
            ))
        })
        .collect();
    ensure!(
        failures.is_empty(),
        "{} of {} calls failed during rollback:\n{}\nRollback and readiness: {rollout:?}",
        failures.len(),
        calls.len(),
        failures.join("\n"),
    );
    rollout?;
    ensure!(
        restored,
        "the web edge did not serve version 1 within {OBSERVE_DEADLINE:?} after rollback and readiness"
    );
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Version {
    One,
    Two,
}

#[derive(Debug)]
struct Call {
    time: Duration,
    status: Option<StatusCode>,
    result: anyhow::Result<Version>,
}

async fn call_during_rollback(
    routes: &Routes,
    widget: &str,
    mut completion: oneshot::Receiver<bool>,
) -> (Vec<Call>, bool) {
    let start = Instant::now();
    let mut calls = Vec::new();
    let mut recovery_deadline = None;
    loop {
        let call = widget_version(routes, widget, start).await;
        let version_one = matches!(call.result, Ok(Version::One));
        calls.push(call);
        if recovery_deadline.is_none() {
            match completion.try_recv() {
                Ok(true) => recovery_deadline = Some(Instant::now() + OBSERVE_DEADLINE),
                Ok(false) | Err(oneshot::error::TryRecvError::Closed) => return (calls, false),
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        if let Some(end) = recovery_deadline
            && (version_one || Instant::now() >= end)
        {
            return (calls, version_one);
        }
    }
}

async fn widget_version(routes: &Routes, widget: &str, start: Instant) -> Call {
    let time = start.elapsed();
    let mut status = None;
    let result = async {
        let mut url = url::Url::parse(&format!("{}/widget/list", routes.endpoint))
            .context("parse widget list URL")?;
        url.query_pairs_mut().append_pair("selector", "{}");
        // Keep the status even if the body cannot be read within the call deadline.
        let response = routes
            .client
            .get(url.as_str())
            .header("Host", &routes.host)
            .bearer_auth(&routes.bearer)
            .send()
            .await
            .context("send widget list request")?;
        status = Some(response.status());
        ensure!(
            response.status() == StatusCode::OK,
            "unexpected HTTP status"
        );
        let bytes = response.bytes().await.context("read widget list body")?;
        let answer: Value = serde_json::from_slice(&bytes).context("decode widget list body")?;
        let Some([element]) = answer.as_array().map(Vec::as_slice) else {
            anyhow::bail!("invalid widget list answer: {answer}");
        };
        let rows = element["value"]["rows"]
            .as_array()
            .with_context(|| format!("widget list has no rows: {answer}"))?;
        let mut matching = rows.iter().filter(|row| row["id"] == widget);
        let row = matching
            .next()
            .with_context(|| format!("widget {widget} is missing: {answer}"))?;
        ensure!(
            matching.next().is_none(),
            "widget {widget} occurs twice: {answer}"
        );
        match row["code"].as_str() {
            Some("standard") => Ok(Version::One),
            Some("priority") => Ok(Version::Two),
            _ => anyhow::bail!("widget {widget} has invalid code: {row}"),
        }
    }
    .await;
    Call {
        time,
        status,
        result,
    }
}

async fn require_version(routes: &Routes, widget: &str, version: Version) -> anyhow::Result<()> {
    let start = Instant::now();
    let last_call = std::cell::Cell::new(None);
    let waiting = format!("fixture {version:?} answers through web");
    until(OBSERVE_DEADLINE, &waiting, || async {
        let call = widget_version(routes, widget, start).await;
        if call.result.as_ref().is_ok_and(|found| *found == version) {
            return Ok(Some(()));
        }
        last_call.set(Some(call));
        Ok(None)
    })
    .await
    .with_context(|| format!("{waiting}, last call: {:?}", last_call.into_inner()))
}

fn rollback_source(export: &str) -> anyhow::Result<PathBuf> {
    let path = std::env::var_os(export)
        .map(PathBuf::from)
        .with_context(|| {
            format!("{export} is missing; use the exports from cargo xtask prepare-tests")
        })?;
    ensure!(
        path.ends_with(FIXTURE) && path.join("wamn.k").is_file(),
        "{export} does not name a {FIXTURE} source package: {}",
        path.display()
    );
    Ok(path)
}

fn component_digests(workload: &DynamicObject) -> anyhow::Result<BTreeMap<String, String>> {
    #[derive(Deserialize)]
    struct Config {
        components: Vec<Component>,
    }

    #[derive(Deserialize)]
    struct Component {
        name: String,
        digest: String,
    }

    // The chart passes the application component digests to the host in this configuration.
    let config = workload.data["spec"]["template"]["spec"]["components"]
        .as_array()
        .and_then(|components| {
            components.iter().find_map(|component| {
                component["localResources"]["config"]["wamn.application"].as_str()
            })
        })
        .context("the WorkloadDeployment has no wamn.application configuration")?;
    let config: Config =
        serde_json::from_str(config).context("decode wamn.application configuration")?;
    ensure!(
        !config.components.is_empty(),
        "the WorkloadDeployment has no application components"
    );
    Ok(config
        .components
        .into_iter()
        .map(|component| (component.name, component.digest))
        .collect())
}

fn workload_ready(workload: &DynamicObject) -> bool {
    workload.metadata.deletion_timestamp.is_none()
        && workload.data["status"]["conditions"]
            .as_array()
            .is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|condition| condition["type"] == "Ready" && condition["status"] == "True")
            })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn plain_delete_keeps_data_and_apply_creates_a_new_cluster() -> anyhow::Result<()> {
    let application = within(
        DEPLOY_DEADLINE,
        "prepare the fixture",
        Application::prepare(&fixture()),
    )
    .await?;
    let environment = &application.environment;
    let platform = &environment.org.cluster;
    let namespace = environment.triple.org.as_str();
    let widget = create_widget(&application.routes).await?;
    let clusters = project_api(environment, "postgresql.cnpg.io", "Cluster", "clusters");
    let certificates = project_api(
        environment,
        "cert-manager.io",
        "Certificate",
        "certificates",
    );
    let secrets = Api::<Secret>::namespaced(platform.client.clone(), namespace);
    let capture = async {
        let kept = active_project_cluster(platform, &environment.triple).await?;
        wait_cluster_ready(&clusters, &kept).await?;
        let mut signed = Vec::new();
        for certificate in release_objects(environment, &certificates).await? {
            let name = certificate
                .metadata
                .name
                .context("a project Certificate has no name")?;
            let secret = certificate.data["spec"]["secretName"]
                .as_str()
                .with_context(|| format!("Certificate {name} has no secretName"))?;
            secrets
                .get_metadata(secret)
                .await
                .with_context(|| format!("read Secret {secret} before delete"))?;
            signed.push((name, secret.to_owned()));
        }
        Ok((kept, signed))
    };
    let (kept, signed) = within(
        OBSERVE_DEADLINE,
        "capture the active Cluster and certificates",
        capture,
    )
    .await?;
    within(
        OBSERVE_DEADLINE,
        "probe product access before plain delete",
        probe_project_primary(environment, &kept, "connected"),
    )
    .await?;

    plain_delete(environment).await?;
    let retained = async {
        require_no_active_project(environment).await?;
        wait_cluster_ready(&clusters, &kept).await?;
        for (certificate, secret) in &signed {
            require_present(&certificates, certificate).await?;
            let metadata = secrets
                .get_metadata(secret)
                .await
                .with_context(|| format!("read retained Secret {secret}"))?
                .metadata;
            ensure!(
                metadata.deletion_timestamp.is_none(),
                "retained Secret {secret} is being deleted"
            );
        }
        let mut database = observe_project_cluster(platform, namespace, &kept).await?;
        let code: String =
            sqlx::query_scalar("SELECT code::text FROM inventory.widget WHERE id = $1::uuid")
                .bind(&widget)
                .fetch_one(&mut database.connection)
                .await
                .with_context(|| format!("read widget {widget} on retained Cluster {kept}"))?;
        ensure!(code == "standard", "widget {widget} retained code {code:?}");
        Ok(())
    };
    within(
        OBSERVE_DEADLINE,
        "observe retained resources and data",
        retained,
    )
    .await?;
    within(
        OBSERVE_DEADLINE,
        "probe product access after plain delete",
        probe_project_primary(environment, &kept, "timeout"),
    )
    .await?;

    let mut apply = environment.org.command();
    apply.args(["env", "apply"]).arg(&environment.file);
    within(
        COMMAND_DEADLINE,
        "apply the same environment file again",
        output(apply),
    )
    .await?;
    let after_apply = async {
        let active = active_project_cluster(platform, &environment.triple).await?;
        ensure!(active != kept, "apply reused retained Cluster {kept}");
        wait_cluster_ready(&clusters, &active).await?;
        require_present(&clusters, &kept).await
    };
    within(
        OBSERVE_DEADLINE,
        "observe the new and retained Clusters",
        after_apply,
    )
    .await
}

async fn probe_project_primary(
    environment: &Environment,
    project: &str,
    expected: &str,
) -> anyhow::Result<()> {
    const POSTGRES: u16 = 5432;
    const PROBE: &str = r#"
export LC_ALL=C
host=$1 port=$2
diagnostic=$(timeout 5s bash -c 'exec 3<>/dev/tcp/$1/$2' bash "$host" "$port" 2>&1)
status=$?
if (( status == 0 )); then
    result=connected
elif (( status == 124 )); then
    result=timeout
elif (( status == 1 )) && [[ $diagnostic == *'Connection refused'* ]]; then
    result=refused
else
    result="error ($status) ${diagnostic//$'\n'/ }"
fi
printf '%s %s %s\n' "$host" "$port" "$result"
"#;

    let triple = &environment.triple;
    let namespace = triple.org.as_str();
    let client = &environment.org.cluster.client;
    let selector = format!("cnpg.io/cluster={project},cnpg.io/instanceRole=primary");
    let primary = Api::<Pod>::namespaced(client.clone(), namespace)
        .list(&ListParams::default().labels(&selector))
        .await
        .with_context(|| format!("list primary pods for Cluster {namespace}/{project}"))?
        .items
        .into_iter()
        .find(|pod| pod.metadata.deletion_timestamp.is_none() && ready(pod))
        .with_context(|| format!("Cluster {namespace}/{project} has no Ready primary"))?;
    let ip = primary
        .status
        .and_then(|status| status.pod_ip)
        .with_context(|| format!("Cluster {namespace}/{project} primary has no pod IP"))?
        .parse::<IpAddr>()
        .with_context(|| format!("parse the pod IP for Cluster {namespace}/{project} primary"))?;
    let address = SocketAddr::new(ip, POSTGRES);
    let image = probe_image(client, namespace, project).await?;
    let labels = BTreeMap::from([
        ("wasmcloud.com/name".into(), "hostgroup".into()),
        (
            "wasmcloud.com/hostgroup".into(),
            name::environment_host_name(&triple.org, &triple.env),
        ),
    ]);
    let template = PodTemplateSpec {
        metadata: Some(ObjectMeta {
            labels: Some(labels),
            ..ObjectMeta::default()
        }),
        spec: Some(PodSpec {
            automount_service_account_token: Some(false),
            containers: vec![Container {
                name: "probe".into(),
                image: Some(image),
                command: Some(vec![
                    "bash".into(),
                    "-c".into(),
                    PROBE.into(),
                    "probe".into(),
                    ip.to_string(),
                    POSTGRES.to_string(),
                ]),
                ..Container::default()
            }],
            ..PodSpec::default()
        }),
    };
    let (exit, logs) =
        Box::pin(run_job(client, namespace, template, Duration::from_mins(2))).await?;
    ensure!(
        exit == 0,
        "{triple} probe to {address} exited {exit}: {logs}"
    );
    let prefix = format!("{ip} {POSTGRES} ");
    let mut lines = logs.lines();
    let result = lines
        .next()
        .and_then(|line| line.strip_prefix(&prefix))
        .with_context(|| format!("{triple} has no valid probe result for {address}: {logs}"))?;
    ensure!(
        result == expected,
        "{triple} probe to {address} expected {expected}, got {result}"
    );
    ensure!(
        lines.next().is_none(),
        "{triple} probe emitted extra results: {logs}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn data_delete_removes_resources_kept_by_plain_delete() -> anyhow::Result<()> {
    let environment = within(
        COMMAND_DEADLINE,
        "create the org and apply the environment",
        async { Org::create().await?.apply(FIXTURE).await },
    )
    .await?;
    within(
        DEPLOY_DEADLINE,
        "deploy the fixture",
        environment.deploy(&fixture(), FIXTURE),
    )
    .await?;
    let clusters = project_api(&environment, "postgresql.cnpg.io", "Cluster", "clusters");
    let certificates = project_api(
        &environment,
        "cert-manager.io",
        "Certificate",
        "certificates",
    );
    let issuers = project_api(&environment, "cert-manager.io", "Issuer", "issuers");
    let capture = async {
        let active = active_project_cluster(&environment.org.cluster, &environment.triple).await?;
        wait_cluster_ready(&clusters, &active).await?;
        let names = async |api: &Api<DynamicObject>| {
            release_objects(&environment, api)
                .await?
                .into_iter()
                .map(|object| {
                    object
                        .metadata
                        .name
                        .context("a project resource has no name")
                })
                .collect::<anyhow::Result<Vec<_>>>()
        };
        let certificate_names = names(&certificates).await?;
        let issuer_names = names(&issuers).await?;
        Ok([
            (clusters, vec![active]),
            (certificates, certificate_names),
            (issuers, issuer_names),
        ])
    };
    let resources = within(
        OBSERVE_DEADLINE,
        "capture the active project's resources",
        capture,
    )
    .await?;
    let captured = || {
        resources
            .iter()
            .flat_map(|(api, names)| names.iter().map(move |name| (api, name)))
    };

    plain_delete(&environment).await?;
    let retained = async {
        require_no_active_project(&environment).await?;
        for (api, name) in captured() {
            require_present(api, name).await?;
        }
        Ok(())
    };
    within(
        OBSERVE_DEADLINE,
        "observe kept resources before data delete",
        retained,
    )
    .await?;

    let coordinate = environment.triple.to_string();
    let args = [
        "env",
        "delete",
        &coordinate,
        "--data",
        "--confirm",
        &coordinate,
    ];
    within(
        COMMAND_DEADLINE,
        "delete the kept project data",
        wamn(&environment.org, &args),
    )
    .await?;
    let gone = captured().map(|(api, name)| async move {
        let waiting = format!("{}/{name} is gone", api.resource_url());
        let poll = until(OBSERVE_DEADLINE, &waiting, || async {
            Ok(api.get_opt(name).await?.is_none().then_some(()))
        });
        within(OBSERVE_DEADLINE, &waiting, poll).await
    });
    try_join_all(gone).await?;
    Ok(())
}

async fn create_widget(routes: &Routes) -> anyhow::Result<String> {
    let id = uuid::Uuid::now_v7().to_string();
    let created = within(
        CALL_DEADLINE,
        "create the fixture widget",
        routes.create(&id, &id, &id, "standard"),
    )
    .await?;
    ensure!(
        created["value"]["id"] == id,
        "creating widget {id} answered {created}"
    );
    Ok(id)
}

async fn plain_delete(environment: &Environment) -> anyhow::Result<()> {
    let coordinate = environment.triple.to_string();
    // The shared Drop deletes the reapplied instance or accepts an absent registry row.
    within(
        COMMAND_DEADLINE,
        "plain delete the environment",
        wamn(&environment.org, &["env", "delete", &coordinate]),
    )
    .await?;
    Ok(())
}

async fn require_present(api: &Api<DynamicObject>, name: &str) -> anyhow::Result<()> {
    let path = api.resource_url();
    let object = api
        .get_metadata(name)
        .await
        .with_context(|| format!("read {path}/{name}"))?;
    ensure!(
        object.metadata.deletion_timestamp.is_none(),
        "{path}/{name} is being deleted"
    );
    Ok(())
}

async fn require_no_active_project(environment: &Environment) -> anyhow::Result<()> {
    let mut system = environment.org.cluster.observe_system().await?;
    let triple = &environment.triple;
    let active =
        wamn_environment::registry::deployment::instance_suffix(&mut system.connection, triple)
            .await
            .with_context(|| format!("read registry deployment for {triple}"))?;
    ensure!(
        active.is_none(),
        "plain delete left an active deployment for {triple}: {active:?}"
    );
    Ok(())
}

fn project_api(
    environment: &Environment,
    group: &str,
    resource: &str,
    plural: &str,
) -> Api<DynamicObject> {
    let gvk = GroupVersionKind::gvk(group, "v1", resource);
    Api::namespaced_with(
        environment.org.cluster.client.clone(),
        environment.triple.org.as_str(),
        &ApiResource::from_gvk_with_plural(&gvk, plural),
    )
}

async fn release_objects(
    environment: &Environment,
    api: &Api<DynamicObject>,
) -> anyhow::Result<Vec<DynamicObject>> {
    let triple = &environment.triple;
    let release = name::environment_resource_name(&triple.org, &triple.project, &triple.env);
    let mut list = api
        .list(&ListParams::default())
        .await
        .with_context(|| format!("list {} for {triple}", api.resource_url()))?;
    list.items.retain(|object| {
        object
            .metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get("meta.helm.sh/release-name"))
            == Some(&release)
    });
    ensure!(
        !list.items.is_empty(),
        "{} has no resources for release {release}",
        api.resource_url()
    );
    Ok(list.items)
}

async fn wait_cluster_ready(clusters: &Api<DynamicObject>, name: &str) -> anyhow::Result<()> {
    until(
        OBSERVE_DEADLINE,
        &format!("Cluster {name} is Ready"),
        || async {
            let cluster = clusters
                .get(name)
                .await
                .with_context(|| format!("read Cluster {name}"))?;
            Ok((cluster.metadata.deletion_timestamp.is_none()
                && common::host::condition(&cluster, "Ready") == Some("True"))
            .then_some(()))
        },
    )
    .await
}

async fn within<T>(
    deadline: Duration,
    action: &str,
    future: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    timeout(deadline, future)
        .await
        .with_context(|| format!("{action} within {deadline:?}"))?
        .with_context(|| action.to_owned())
}

//! Network isolation between environments in one org and across org namespaces (C6).

mod common;

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use common::host::{FIXTURE, fixture, ready, ready_host_pod};
use common::request::{self, OtherEnvironment, until};
use common::{
    Application, Cluster, Org, active_project_cluster, observe_project_cluster, probe_image,
    run_job,
};
use k8s_openapi::api::core::v1::{Container, Pod, PodSpec, PodTemplateSpec, Service};
use k8s_openapi::api::discovery::v1::EndpointSlice;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::Api;
use kube::api::ListParams;
use serde_json::json;
use tokio::time::timeout;
use wamn_registry::{Triple, name, resource_names};

const APPLY_DEADLINE: Duration = Duration::from_mins(10);
const DEPLOY_DEADLINE: Duration = Duration::from_mins(20);
const OBSERVE_DEADLINE: Duration = Duration::from_mins(5);
const JOB_DEADLINE: Duration = Duration::from_mins(2);
const HOST_NAME_LABEL: &str = "wasmcloud.com/name";
const HOST_GROUP_LABEL: &str = "wasmcloud.com/hostgroup";
const HOST_HTTP: u16 = 9191;
const HOST_PROBES: u16 = 8081;
const POSTGRES: u16 = 5432;
const APP_HTTP: u16 = 80;

const PROBE: &str = r#"
export LC_ALL=C
while (( $# )); do
    host=$1 port=$2
    shift 2
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
done
exit 0
"#;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn environments_in_one_org_are_isolated() -> anyhow::Result<()> {
    common::capture(
        "environments_in_one_org_are_isolated",
        environments_in_one_org_are_isolated_case,
    )
    .await
}

async fn environments_in_one_org_are_isolated_case() -> anyhow::Result<()> {
    let b = timeout(DEPLOY_DEADLINE, Application::prepare(&fixture()))
        .await
        .context("deploy the application in B")??;
    let org = &b.environment.org;
    let a = timeout(
        APPLY_DEADLINE,
        OtherEnvironment::apply(org, "alpha", FIXTURE),
    )
    .await
    .context("apply environment A in the shared org")??;
    check_isolation(&org.cluster, &a.triple, &b).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn environments_in_two_orgs_are_isolated() -> anyhow::Result<()> {
    common::capture(
        "environments_in_two_orgs_are_isolated",
        environments_in_two_orgs_are_isolated_case,
    )
    .await
}

async fn environments_in_two_orgs_are_isolated_case() -> anyhow::Result<()> {
    let b = timeout(DEPLOY_DEADLINE, Application::prepare(&fixture()))
        .await
        .context("deploy the application in B")??;
    let a = timeout(APPLY_DEADLINE, async {
        Org::create().await?.apply(FIXTURE).await
    })
    .await
    .context("create the second org and apply environment A")??;
    check_isolation(&a.org.cluster, &a.triple, &b).await
}

async fn check_isolation(cluster: &Cluster, a: &Triple, b: &Application) -> anyhow::Result<()> {
    let namespace = a.org.as_str();
    let client = &cluster.client;
    let (labels, image, control, targets) = timeout(OBSERVE_DEADLINE, async {
        let labels = source_labels(client, a).await?;
        let project = active_project_cluster(cluster, a).await?;
        let image = probe_image(client, namespace, &project).await?;
        let rw = format!("{project}-rw");
        let control = service_address(client, namespace, &rw, POSTGRES).await?;
        let targets = live_targets(b).await?;
        anyhow::Ok((labels, image, control, targets))
    })
    .await
    .with_context(|| {
        format!("observe the probe source {a} and its targets within {OBSERVE_DEADLINE:?}")
    })??;

    let mut command = vec!["bash".into(), "-c".into(), PROBE.into(), "probe".into()];
    for address in std::iter::once(control).chain(targets.iter().map(|(_, address)| *address)) {
        command.extend([address.ip().to_string(), address.port().to_string()]);
    }
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
                command: Some(command),
                ..Container::default()
            }],
            ..PodSpec::default()
        }),
    };
    let (exit, logs) = run_job(client, namespace, template, JOB_DEADLINE).await?;
    ensure!(exit == 0, "{a} probe exited {exit}: {logs}");
    let mut results = logs.lines();
    let control_result = probe_result(results.next(), control)
        .with_context(|| format!("setup: {a} has no valid result for its database at {control}"))?;
    ensure!(
        control_result == "connected",
        "setup: {a} could not connect to its database at {control}: {control_result}"
    );
    for (target, address) in targets {
        let result = probe_result(results.next(), address)?;
        ensure!(
            result != "connected",
            "{a} connected to B's {target} at {address}"
        );
        ensure!(
            matches!(result, "timeout" | "refused"),
            "{a} probe of B's {target} at {address} failed: {result}"
        );
    }
    ensure!(
        results.next().is_none(),
        "{a} probe emitted extra results: {logs}"
    );
    Ok(())
}

fn probe_result(line: Option<&str>, address: SocketAddr) -> anyhow::Result<&str> {
    let prefix = format!("{} {} ", address.ip(), address.port());
    line.and_then(|line| line.strip_prefix(&prefix))
        .with_context(|| format!("expected a probe result for {address}, got {line:?}"))
}

// OtherEnvironment exposes a coordinate, so look up its pod by the host group label.
async fn source_labels(
    client: &kube::Client,
    a: &Triple,
) -> anyhow::Result<BTreeMap<String, String>> {
    let group = name::environment_host_name(&a.org, &a.env);
    let selector = ListParams::default().labels(&format!("{HOST_GROUP_LABEL}={group}"));
    let pods = Api::<Pod>::namespaced(client.clone(), a.org.as_str());
    let pod = until(
        OBSERVE_DEADLINE,
        &format!("{a} has a Ready host pod"),
        || async {
            Ok(pods
                .list(&selector)
                .await?
                .items
                .into_iter()
                .find(|pod| pod.metadata.deletion_timestamp.is_none() && ready(pod)))
        },
    )
    .await?;
    let mut labels = pod.metadata.labels.context("A's host pod has no labels")?;
    [HOST_NAME_LABEL, HOST_GROUP_LABEL]
        .into_iter()
        .map(|key| {
            labels
                .remove_entry(key)
                .with_context(|| format!("{a} host pod lacks {key}"))
        })
        .collect()
}

async fn live_targets(b: &Application) -> anyhow::Result<[(&'static str, SocketAddr); 5]> {
    let environment = &b.environment;
    let triple = &environment.triple;
    let cluster = &environment.org.cluster;
    let client = &cluster.client;
    let namespace = triple.org.as_str();
    let host = ready_host_pod(environment, OBSERVE_DEADLINE).await?;
    let host_ip = pod_ip(&host)?;
    // The Ready condition proves the probe listener, and the web call proves HTTP.
    ensure!(
        host.spec
            .iter()
            .flat_map(|spec| &spec.containers)
            .any(|container| {
                let Some(probe) = container
                    .readiness_probe
                    .as_ref()
                    .and_then(|probe| probe.http_get.as_ref())
                else {
                    return false;
                };
                match &probe.port {
                    IntOrString::Int(port) => *port == i32::from(HOST_PROBES),
                    IntOrString::String(name) => container.ports.iter().flatten().any(|port| {
                        port.name.as_ref() == Some(name)
                            && port.container_port == i32::from(HOST_PROBES)
                    }),
                }
            }),
        "{triple} host lacks an HTTP readiness probe on {HOST_PROBES}"
    );

    let project = active_project_cluster(cluster, triple).await?;
    let rw = format!("{project}-rw");
    let primary_selector = format!("cnpg.io/cluster={project},cnpg.io/instanceRole=primary");
    let primary = Api::<Pod>::namespaced(client.clone(), namespace)
        .list(&ListParams::default().labels(&primary_selector))
        .await?
        .items
        .into_iter()
        .find(|pod| pod.metadata.deletion_timestamp.is_none() && ready(pod))
        .with_context(|| format!("{project} has no Ready primary pod"))?;
    let primary_ip = pod_ip(&primary)?;
    let mut observed = observe_project_cluster(cluster, namespace, &project).await?;
    let is_primary: bool = sqlx::query_scalar("SELECT NOT pg_is_in_recovery()")
        .fetch_one(&mut observed.connection)
        .await
        .context("query B's primary as the observer")?;
    ensure!(is_primary, "{project} observer reached a standby");

    let release = resource_names::application_release_name(
        &triple.org,
        &triple.project,
        &triple.env,
        &FIXTURE.parse()?,
    );
    let http = name::resource_suffix(&release, "http");
    let host_http = SocketAddr::new(host_ip, HOST_HTTP);
    let primary_postgres = SocketAddr::new(primary_ip, POSTGRES);
    let (rw_address, http_address) = tokio::try_join!(
        ready_service(client, namespace, &rw, POSTGRES, primary_postgres),
        ready_service(client, namespace, &http, APP_HTTP, host_http),
    )?;
    let (status, answer) = request::read(&b.routes, "/widget/list", &json!({}))
        .await
        .context("read B's fixture through the web edge")?;
    ensure!(
        status == 200,
        "{triple} fixture answered {status}: {answer}"
    );

    Ok([
        ("host HTTP", host_http),
        ("host probes", SocketAddr::new(host_ip, HOST_PROBES)),
        ("PostgreSQL Service", rw_address),
        ("PostgreSQL primary", primary_postgres),
        ("application Service", http_address),
    ])
}

fn pod_ip(pod: &Pod) -> anyhow::Result<IpAddr> {
    let address = pod
        .status
        .as_ref()
        .and_then(|status| status.pod_ip.as_deref())
        .with_context(|| format!("pod {:?} has no IP", pod.metadata.name))?;
    address
        .parse()
        .with_context(|| format!("invalid pod IP {address}"))
}

async fn service_address(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    port: u16,
) -> anyhow::Result<SocketAddr> {
    let spec = Api::<Service>::namespaced(client.clone(), namespace)
        .get(name)
        .await
        .with_context(|| format!("read Service {namespace}/{name}"))?
        .spec
        .with_context(|| format!("Service {namespace}/{name} has no spec"))?;
    ensure!(
        spec.ports
            .iter()
            .flatten()
            .any(|entry| entry.port == i32::from(port)),
        "Service {namespace}/{name} has no port {port}"
    );
    let ip = spec
        .cluster_ip
        .and_then(|ip| ip.parse().ok())
        .with_context(|| format!("Service {namespace}/{name} has no ClusterIP"))?;
    Ok(SocketAddr::new(ip, port))
}

async fn ready_service(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    port: u16,
    backend: SocketAddr,
) -> anyhow::Result<SocketAddr> {
    let address = service_address(client, namespace, name, port).await?;
    let slices = Api::<EndpointSlice>::namespaced(client.clone(), namespace);
    let selector = ListParams::default().labels(&format!("kubernetes.io/service-name={name}"));
    let ip = backend.ip().to_string();
    until(
        OBSERVE_DEADLINE,
        &format!("Service {namespace}/{name} has Ready endpoint {backend}"),
        || async {
            let serving = slices
                .list(&selector)
                .await?
                .items
                .into_iter()
                .any(|slice| {
                    slice
                        .ports
                        .iter()
                        .flatten()
                        .any(|port| port.port == Some(i32::from(backend.port())))
                        && slice.endpoints.iter().any(|endpoint| {
                            endpoint
                                .conditions
                                .as_ref()
                                .and_then(|conditions| conditions.ready)
                                != Some(false)
                                && endpoint.addresses.contains(&ip)
                        })
                });
            Ok(serving.then_some(address))
        },
    )
    .await
}

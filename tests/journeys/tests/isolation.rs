//! Network isolation between environments in one org and across org namespaces (C6).

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use common::host::{FIXTURE, fixture, ready, ready_host_pod};
use common::request::{self, OtherEnv, until};
use common::{
    App, Cluster, Org, PLATFORM, active_project_cluster, exec, identity_pods, identity_port,
    probe_image, run_job,
};
use k8s_openapi::api::core::v1::{Container, Pod, PodSpec, PodTemplateSpec, Service};
use k8s_openapi::api::discovery::v1::EndpointSlice;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::Api;
use kube::api::{AttachParams, ListParams};
use serde_json::json;
use tokio::time::timeout;
use wamn_http_client::HttpClient;
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
    diagnostic=$(timeout 5s bash -c 'exec 3<>"/dev/tcp/$1/$2"' bash "$host" "$port" 2>&1)
    status=$?
    if (( status == 0 )); then
        result=connected
    elif (( status == 124 )); then
        result=timeout
    elif (( status == 1 )) && [[ $diagnostic == *'Connection refused'* ]]; then
        result=refused
    else
        printf 'probe %s %s exited %s: %s\n' "$host" "$port" "$status" "$diagnostic" >&2
        exit 1
    fi
    printf '%s %s %s\n' "$host" "$port" "$result"
done
exit 0
"#;

const IDENTITY_POST: &str = r#"
set -o pipefail
{
    printf 'POST %s HTTP/1.1\r\nHost: %s\r\nX-Forwarded-For: %s\r\n' "$3" "$2" "$4"
    if (( $# == 5 )); then
        printf 'Authorization: Bearer %s\r\n' "$(cat "$5")"
    fi
    printf 'Content-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}'
} |
    timeout 10s openssl s_client -connect "$1" -servername "$2" -verify_hostname "$2" \
        -CAfile /etc/identity-ca/ca.crt -verify_return_error -quiet -ignore_unexpected_eof
"#;

async fn ready_identity(client: &kube::Client) -> anyhow::Result<(Pod, String)> {
    let (service, pods) = identity_pods(client).await?;
    let pod = pods.into_iter().find(ready).context("ready identity pod")?;
    let name = service.metadata.name.context("identity Service name")?;
    Ok((pod, format!("{name}.{PLATFORM}.svc.cluster.local")))
}

common::journey! {
    async fn envs_in_one_org_are_isolated() -> anyhow::Result<()> {
        let b = timeout(DEPLOY_DEADLINE, App::prepare(&fixture()))
            .await
            .context("deploy the application in B")??;
        let org = &b.env.org;
        let a = timeout(
            APPLY_DEADLINE,
            OtherEnv::apply(org, "alpha", FIXTURE),
        )
        .await
        .context("apply environment A in the shared org")??;
        check_isolation(&org.cluster, &a.triple, &b).await
    }
}

common::journey! {
    async fn host_cannot_reach_identity_password_port() -> anyhow::Result<()> {
        let env = timeout(APPLY_DEADLINE, async {
            Org::create().await?.apply(FIXTURE).await
        })
        .await
        .context("apply an org environment for the identity probe")??;
        let org = &env.org;
        let cluster = &org.cluster;
        let client = &cluster.client;
        let host = ready_host_pod(&env, OBSERVE_DEADLINE).await?;
        let host_name = host.metadata.name.context("host pod name")?;
        let hosts = Api::namespaced(client.clone(), org.name.as_str());
        let (identity, identity_host) = ready_identity(client).await?;
        let identity_ip = pod_ip(&identity)?;
        let password = SocketAddr::new(identity_ip, identity_port(&identity, "password")?);
        let general = SocketAddr::new(identity_ip, identity_port(&identity, "https")?);

        let web_host = format!("{}.{}", org.name, cluster.outputs.base_domain);
        let reply = HttpClient::builder()
            .timeout(Duration::from_secs(10))
            .build()?
            .post(format!("http://{}/password/recover", cluster.outputs.web_address))
            .header("host", &web_host)
            .header("origin", format!("https://{web_host}"))
            .header("sec-fetch-site", "same-origin")
            .json(&json!({}))
            .send()
            .await?;
        ensure!(reply.status() == hyper::StatusCode::BAD_REQUEST,
            "the web proxy did not reach identity's password route: {}", reply.status());

        let result = exec(&hosts, &host_name, &AttachParams::default(), [
            "bash", "-c", PROBE, "probe", &password.ip().to_string(), &password.port().to_string(),
        ]).await?;
        let result = probe_result(result.lines().next(), password)?;
        ensure!(matches!(result, "timeout" | "refused"),
            "host probe of identity's password listener at {password}: {result}");

        // Only admitted control requests reach the general listener's source counter.
        let control = hosts.list(&ListParams::default().labels("app.kubernetes.io/component=wamn-control"))
            .await?.items.into_iter().find(ready).context("ready control pod")?;
        let control_ip = pod_ip(&control)?;
        let control_name = control.metadata.name.context("control pod name")?;
        let forwarded = Ipv6Addr::from(
            0x2001_0db8_0000_0000_0000_0000_0000_0000u128 | (uuid::Uuid::new_v4().as_u128() >> 64),
        );
        let reply = exec(&hosts, &control_name, &AttachParams::default(), [
            "bash", "-ec", IDENTITY_POST, "probe",
            &general.to_string(), &identity_host, "/invitations", &forwarded.to_string(),
            "/var/run/secrets/wamn-identity/token",
        ]).await?;
        ensure!(reply.lines().next().and_then(|line| line.split_whitespace().nth(1)) == Some("400"),
            "identity did not process control's invitation request: {reply}");

        let mut system = cluster.observe_system().await?;
        let socket_bucket = format!("source:{control_ip}");
        let recorded: Vec<String> = sqlx::query_scalar(
            "SELECT bucket FROM identity.password_attempt WHERE bucket IN ($1, $2)",
        )
        .bind(&socket_bucket)
        .bind(format!("source:{forwarded}"))
        .fetch_all(&mut system.connection)
        .await?;
        ensure!(recorded == [socket_bucket], "identity recorded source buckets {recorded:?}");
        let event = timeout(Duration::from_secs(5), async {
            loop {
                let event = sqlx::query_as::<_, (String, String, Option<String>)>(
                    "SELECT host(ip_address), ip_source, host(peer_ip_address)
                     FROM identity.authentication_event
                     WHERE event_type = 'invitation_request' AND outcome = 'request_invalid'
                       AND org = $1 AND workload = $2",
                )
                .bind(org.name.as_str())
                .bind(name::control_service_account(&org.name))
                .fetch_optional(&mut system.connection)
                .await?;
                if let Some(event) = event {
                    break anyhow::Ok(event);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }).await.context("identity did not record control's malformed invitation request")??;
        ensure!(event == (control_ip.to_string(), "socket".into(), None),
            "identity recorded unexpected invitation address provenance: {event:?}");
        Ok(())
    }
}

common::journey! {
    async fn host_password_sign_in_on_identity_general_port_returns_404() -> anyhow::Result<()> {
        let env = timeout(APPLY_DEADLINE, async {
            Org::create().await?.apply(FIXTURE).await
        }).await.context("apply an org environment for the identity probe")??;
        let client = &env.org.cluster.client;
        let host = ready_host_pod(&env, OBSERVE_DEADLINE).await?;
        let host_name = host.metadata.name.context("host pod name")?;
        let hosts = Api::namespaced(client.clone(), env.org.name.as_str());
        let (identity, identity_host) = ready_identity(client).await?;
        let general = SocketAddr::new(pod_ip(&identity)?, identity_port(&identity, "https")?);
        let reply = exec(&hosts, &host_name, &AttachParams::default(), [
            "bash", "-ec", IDENTITY_POST, "probe",
            &general.to_string(), &identity_host, "/password/session", "192.0.2.42",
        ]).await?;
        ensure!(reply.lines().next().and_then(|line| line.split_whitespace().nth(1)) == Some("404"),
            "identity's general listener answered the host password sign-in: {reply}");
        Ok(())
    }
}

common::journey! {
    async fn envs_in_two_orgs_are_isolated() -> anyhow::Result<()> {
        let b = timeout(DEPLOY_DEADLINE, App::prepare(&fixture()))
            .await
            .context("deploy the application in B")??;
        let a = timeout(APPLY_DEADLINE, async {
            Org::create().await?.apply(FIXTURE).await
        })
        .await
        .context("create the second org and apply environment A")??;
        check_isolation(&a.org.cluster, &a.triple, &b).await
    }
}

async fn check_isolation(cluster: &Cluster, a: &Triple, b: &App) -> anyhow::Result<()> {
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
            matches!(result, "timeout" | "refused"),
            "{a} must be refused by B's {target} at {address}: {result}"
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
    let group = name::env_host_name(&a.org, &a.env);
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

async fn live_targets(b: &App) -> anyhow::Result<[(&'static str, SocketAddr); 5]> {
    let env = &b.env;
    let triple = &env.triple;
    let cluster = &env.org.cluster;
    let client = &cluster.client;
    let namespace = triple.org.as_str();
    let host = ready_host_pod(env, OBSERVE_DEADLINE).await?;
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
    let release = resource_names::app_release_name(
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
    let hosts = Api::namespaced(client.clone(), namespace);
    let host_name = host.metadata.name.context("B's host pod name")?;
    let primary_result = exec(
        &hosts,
        &host_name,
        &AttachParams::default(),
        [
            "bash",
            "-c",
            PROBE,
            "probe",
            &primary_ip.to_string(),
            &POSTGRES.to_string(),
        ],
    )
    .await
    .with_context(|| format!("probe {project}'s primary from B's host"))?;
    let result = probe_result(Some(primary_result.trim_end()), primary_postgres)?;
    ensure!(
        result == "connected",
        "B's host could not connect to its primary at {primary_postgres}: {result}"
    );

    // The Service endpoint above identifies the host that must serve this request.
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
            let ready_addresses = slices
                .list(&selector)
                .await?
                .items
                .into_iter()
                .filter(|slice| {
                    slice
                        .ports
                        .iter()
                        .flatten()
                        .any(|port| port.port == Some(i32::from(backend.port())))
                })
                .flat_map(|slice| slice.endpoints)
                .filter(|endpoint| {
                    endpoint
                        .conditions
                        .as_ref()
                        .and_then(|conditions| conditions.ready)
                        != Some(false)
                })
                .flat_map(|endpoint| endpoint.addresses)
                .collect::<BTreeSet<_>>();
            // These Services must serve through the exact pod probed below.
            Ok((ready_addresses.len() == 1 && ready_addresses.contains(&ip)).then_some(address))
        },
    )
    .await
}

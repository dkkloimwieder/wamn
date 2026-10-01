//! Receiving session setup and checks on two deployed native hosts.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::Command;
use wamn_control::print_release_env::ReleaseCarrier;
use wamn_gate_harness::session_issuer::{
    self, Env, IssuerCluster, Mount, Preserved, SecretVolume, Volume, ca_volume, image_loaded,
    mount,
};
use wamn_test_infrastructure::rendering::{HttpClaims, HttpWorkloadInput, render_http_workload};
use wamn_test_infrastructure::workload;

use super::super::{identity, sessions};
use super::resources::checked;
use super::{ReceivingCluster, Resources, apply, kubectl};

const DEPLOYMENTS: [&str; 2] = ["flow-http-session-a", "flow-http-session-b"];

/// The issuer cluster of this Receiving run.
fn issuer_cluster(cluster: &ReceivingCluster) -> IssuerCluster<'_> {
    let resources = &cluster.resources;
    IssuerCluster {
        name: &resources.name,
        work: &resources.work,
        evidence: &resources.evidence,
        repository: &resources.repository,
        lifecycle: &resources.lifecycle,
        source: &resources.source,
        system_database_url: &cluster.inputs.system_pg_url,
        identity_image: resources.identity_image.as_deref(),
        org: identity().org.as_str(),
        project: identity().project.as_str(),
        environment: identity().environment.as_str(),
        tenant: identity().tenant.as_str(),
    }
}

/// Supply the real issuer required by the published application's session routes.
pub(super) async fn prepare_application(
    cluster: &ReceivingCluster,
    carrier: &ReleaseCarrier,
) -> anyhow::Result<(String, String)> {
    load_gates(cluster).await?;
    session_issuer::prepare_application(
        &issuer_cluster(cluster),
        &carrier.manifest_digest.to_string(),
    )
    .await
}

pub(super) async fn prepare(
    cluster: &ReceivingCluster,
    carrier: &ReleaseCarrier,
    fixture: &Path,
) -> anyhow::Result<String> {
    load_gates(cluster).await?;
    session_issuer::prepare(
        &issuer_cluster(cluster),
        &carrier.manifest_digest.to_string(),
        fixture,
    )
    .await
}

/// Load the gates image of a case that uses it, beside the issuer.
async fn load_gates(cluster: &ReceivingCluster) -> anyhow::Result<()> {
    if let Some(gates_image) = cluster.resources.gates_image.as_deref() {
        image_loaded(&issuer_cluster(cluster), gates_image, "gates", true).await?;
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
struct Metadata {
    name: String,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum WorkloadDocument {
    Service {
        metadata: Metadata,
        #[serde(flatten)]
        rest: Preserved,
    },
    WorkloadDeployment {
        metadata: Metadata,
        spec: DeploymentSpec,
        #[serde(flatten)]
        rest: Preserved,
    },
}
#[derive(Clone, Serialize, Deserialize)]
struct DeploymentSpec {
    replicas: u32,
    template: WorkloadTemplate,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Clone, Serialize, Deserialize)]
struct WorkloadTemplate {
    spec: WorkloadSpec,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Clone, Serialize, Deserialize)]
struct WorkloadSpec {
    #[serde(rename = "hostId", skip_serializing_if = "Option::is_none")]
    host_id: Option<String>,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentList<T> {
    api_version: &'static str,
    kind: &'static str,
    items: Vec<T>,
}

fn pinned_workloads(source: &str, pins: &[Value]) -> anyhow::Result<String> {
    let documents = serde_yaml::Deserializer::from_str(source)
        .map(WorkloadDocument::deserialize)
        .collect::<Result<Vec<_>, _>>()?;
    ensure!(
        documents.len() == 2,
        "the session workload source has two documents"
    );
    let service = documents.iter().find(|document| matches!(document, WorkloadDocument::Service { metadata, .. } if metadata.name == "flow-http"))
        .context("the session workload source has its flow-http Service")?;
    let template = documents.iter().find(|document| matches!(document, WorkloadDocument::WorkloadDeployment { metadata, .. } if metadata.name == "flow-http"))
        .context("the session workload source has its flow-http WorkloadDeployment")?;
    let mut items = vec![service.clone()];
    for pin in pins {
        let mut document = template.clone();
        if let WorkloadDocument::WorkloadDeployment { metadata, spec, .. } = &mut document {
            metadata.name = text(pin, "/deployment")?.to_owned();
            spec.replicas = 1;
            spec.template.spec.host_id = Some(text(pin, "/host_id")?.to_owned());
        }
        items.push(document);
    }
    Ok(serde_yaml::to_string(&DocumentList {
        api_version: "v1",
        kind: "List",
        items,
    })?)
}

pub(super) async fn assert_session(
    cluster: &ReceivingCluster,
    fixture: &Path,
    issuer: &str,
    http_image: &str,
    fresh_only: bool,
    session_client: bool,
) -> anyhow::Result<()> {
    let resources = &cluster.resources;
    let fixture: Value = serde_json::from_slice(&fs::read(fixture)?)?;
    let gates_image = resources
        .gates_image
        .as_deref()
        .context("the session gates image is built")?;
    let gates = image_loaded(&issuer_cluster(cluster), gates_image, "gates", true).await?;
    let host_digest = workload::image_ready(
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
        runtime_digest: &host_digest,
        replicas: 2,
        evidence: &resources.evidence,
    })
    .await?;
    let deployment = read_object(
        resources,
        "session-host-deployment",
        &[
            "-n",
            &resources.name,
            "get",
            "deployment",
            "hostgroup-default",
        ],
    )
    .await?;
    let labels = deployment
        .pointer("/spec/selector/matchLabels")
        .and_then(Value::as_object)
        .context("the host deployment has selector labels")?;
    ensure!(!labels.is_empty(), "the host selector is not empty");
    let selector = labels
        .iter()
        .map(|(key, value)| {
            Ok(format!(
                "{key}={}",
                value.as_str().context("the host selector value is text")?
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .join(",");
    let source = render_http_workload(
        &fs::read_to_string(
            resources
                .repository
                .join("deploy/platform/http-route-workload.example.yaml"),
        )?,
        &HttpWorkloadInput {
            namespace: resources.name.clone(),
            image: http_image.to_owned(),
            route_host: cluster.inputs.route_host.clone(),
            claims: HttpClaims {
                tenant: identity().tenant.clone(),
                catalog: "default".to_owned(),
                environment: resources.name.clone(),
                project: identity().project.clone(),
                schema: "receiving".to_owned(),
            },
        },
    )?;
    fs::write(
        resources.evidence.join("session-workload-source.yaml"),
        &source,
    )?;
    let mut previous = Vec::new();
    for available in [true, false] {
        let stage = if session_client {
            "session-client"
        } else if available {
            "host-session-reachable"
        } else {
            "host-session-unreachable"
        };
        if !available {
            checked(kubectl(resources).args([
                "-n",
                &resources.name,
                "delete",
                "workloaddeployment",
                DEPLOYMENTS[0],
                DEPLOYMENTS[1],
                "--cascade=foreground",
                "--wait=true",
                "--timeout=120s",
            ]))
            .await?;
            checked(kubectl(resources).args([
                "-n",
                &resources.name,
                "rollout",
                "restart",
                "deployment/hostgroup-default",
            ]))
            .await?;
            checked(kubectl(resources).args([
                "-n",
                &resources.name,
                "rollout",
                "status",
                "deployment/hostgroup-default",
                "--timeout=240s",
            ]))
            .await?;
        }
        let published = key_command(resources, &format!("{stage}-published"), &["publish"]).await?;
        let kid = text(&published, "/kid")?;
        canonical_uuid(kid)?;
        let activated = key_command(
            resources,
            &format!("{stage}-activated"),
            &["activate", "--kid", kid],
        )
        .await?;
        ensure!(
            activated["activated"] == true && activated["kid"] == kid,
            "the published session key became active"
        );
        if available {
            Box::pin(nested_session(cluster, issuer, fresh_only, session_client)).await?;
        }
        if session_client {
            break;
        }
        let pods = read_object(
            resources,
            &format!("{stage}-pin-pods"),
            &["-n", &resources.name, "get", "pods", "-l", &selector],
        )
        .await?;
        let hosts = read_object(
            resources,
            &format!("{stage}-pin-hosts"),
            &[
                "-n",
                "wamn-system",
                "get",
                "hosts",
                "-l",
                "hostgroup=default",
            ],
        )
        .await?;
        let pins = select_hosts(
            &pods,
            &hosts,
            &resources.name,
            &resources.host_image,
            &host_digest,
        )?;
        save(resources, &format!("{stage}-pins"), &json!(pins))?;
        let path = resources.evidence.join(format!("{stage}-workload.yaml"));
        fs::write(&path, pinned_workloads(&source, &pins)?)?;
        apply(resources, &path).await?;
        checked(kubectl(resources).args([
            "-n",
            &resources.name,
            "wait",
            "--for=condition=Ready",
            "workloaddeployment/flow-http-session-a",
            "workloaddeployment/flow-http-session-b",
            "--timeout=240s",
        ]))
        .await?;
        let deployments = read_object(
            resources,
            &format!("{stage}-workload-deployments"),
            &[
                "-n",
                &resources.name,
                "get",
                "workloaddeployment",
                DEPLOYMENTS[0],
                DEPLOYMENTS[1],
            ],
        )
        .await?;
        let replicas = array(&deployments, "/items")?
            .iter()
            .map(|value| {
                ready_deployment(value)?;
                text(value, "/status/currentReplicaSet/name")
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut args = vec!["-n", &resources.name, "get", "workloadreplicaset"];
        args.extend(replicas);
        let replicasets =
            read_object(resources, &format!("{stage}-workload-replicasets"), &args).await?;
        let workloads = read_object(
            resources,
            &format!("{stage}-workloads"),
            &["-n", &resources.name, "get", "workloads"],
        )
        .await?;
        let before_pods = read_object(
            resources,
            &format!("{stage}-host-pods"),
            &["-n", &resources.name, "get", "pods", "-l", &selector],
        )
        .await?;
        let before_hosts = read_object(
            resources,
            &format!("{stage}-hosts"),
            &[
                "-n",
                "wamn-system",
                "get",
                "hosts",
                "-l",
                "hostgroup=default",
            ],
        )
        .await?;
        let service = read_object(
            resources,
            &format!("{stage}-service"),
            &["-n", &resources.name, "get", "service", "flow-http"],
        )
        .await?;
        let slices = read_object(
            resources,
            &format!("{stage}-endpointslices"),
            &[
                "-n",
                &resources.name,
                "get",
                "endpointslices",
                "-l",
                "kubernetes.io/service-name=flow-http,wasmcloud.dev/route-manager=true",
            ],
        )
        .await?;
        let endpoints = host_endpoints(
            &ObservedObjects {
                pins: &pins,
                deployments: &deployments,
                replicasets: &replicasets,
                workloads: &workloads,
                pods: &before_pods,
                hosts: &before_hosts,
                service: &service,
                slices: &slices,
            },
            &resources.name,
            &resources.host_image,
            &host_digest,
            http_image,
            text(&fixture, "/route_path")?,
        )?;
        save(
            resources,
            &format!("{stage}-host-endpoints"),
            &json!(endpoints),
        )?;
        if !available {
            for field in [
                "deployment_uid",
                "replicaset_uid",
                "workload_uid",
                "pod_uid",
                "host_id",
            ] {
                ensure!(
                    endpoints
                        .iter()
                        .all(|new| previous.iter().all(|old: &Value| old[field] != new[field])),
                    "the second session case must use new {field} values"
                );
            }
        }
        run_job(cluster, stage, issuer, kid, &endpoints, available, &gates).await?;
        let after_pods = read_object(
            resources,
            &format!("{stage}-post-host-pods"),
            &["-n", &resources.name, "get", "pods", "-l", &selector],
        )
        .await?;
        let after_hosts = read_object(
            resources,
            &format!("{stage}-post-hosts"),
            &[
                "-n",
                "wamn-system",
                "get",
                "hosts",
                "-l",
                "hostgroup=default",
            ],
        )
        .await?;
        let before = processes(
            &endpoints,
            &before_pods,
            &before_hosts,
            &resources.name,
            &resources.host_image,
            &host_digest,
        )?;
        let after = processes(
            &endpoints,
            &after_pods,
            &after_hosts,
            &resources.name,
            &resources.host_image,
            &host_digest,
        )?;
        ensure!(
            before == after,
            "both warm production host processes must remain unchanged"
        );
        save(
            resources,
            &format!("{stage}-process-continuity"),
            &json!({"result":"pass","hosts":2,"processes":after}),
        )?;
        previous = endpoints;
    }
    let head = checked(
        Command::new("git")
            .current_dir(&resources.repository)
            .args(["rev-parse", "HEAD"]),
    )
    .await?;
    ensure!(
        String::from_utf8(head)?.trim() == resources.source,
        "the session source commit changed"
    );
    let status = checked(
        Command::new("git")
            .current_dir(&resources.repository)
            .args([
                "status",
                "--porcelain",
                "--untracked-files=normal",
                "--",
                ".",
                ":(exclude).beads/issues.jsonl",
                ":(exclude).beads/interactions.jsonl",
            ]),
    )
    .await?;
    ensure!(status.is_empty(), "the session source tree changed");
    Ok(())
}

async fn nested_session(
    cluster: &ReceivingCluster,
    issuer: &str,
    fresh_only: bool,
    session_client: bool,
) -> anyhow::Result<()> {
    let resources = &cluster.resources;
    let mut child = kubectl(resources)
        .args([
            "-n",
            &resources.name,
            "port-forward",
            "--address=127.0.0.1",
            "service/host-session-identity",
            ":443",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::from(fs::File::create(
            resources
                .evidence
                .join("session-nested-port-forward-errors.log"),
        )?))
        .kill_on_drop(true)
        .spawn()?;
    let result = async {
        let mut lines = BufReader::new(
            child
                .stdout
                .take()
                .context("the session port-forward has output")?,
        )
        .lines();
        let first = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
            .await??
            .context("the session port-forward reports its endpoint")?;
        fs::write(
            resources.evidence.join("session-nested-port-forward.log"),
            format!("{first}\n"),
        )?;
        let port = first
            .strip_prefix("Forwarding from 127.0.0.1:")
            .and_then(|line| line.strip_suffix(" -> 8443"))
            .context("the session port-forward reports the declared HTTPS target")?
            .parse::<u16>()?;
        ensure!(port != 0, "the forwarded HTTPS port is assigned");
        Box::pin(tokio::time::timeout(
            Duration::from_secs(240),
            sessions::assert_nested_session(
                cluster.inputs.clone(),
                issuer,
                &format!("https://localhost:{port}"),
                &fs::read(resources.work.join("session-ca.crt"))?,
                fresh_only,
                session_client,
            ),
        ))
        .await??;
        save(
            resources,
            if session_client {
                "session-client-result"
            } else {
                "session-nested-result"
            },
            &json!({"result":"pass","fresh_only":fresh_only,"session_client":session_client}),
        )?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let cleanup = async {
        if child.try_wait()?.is_none() {
            child.start_kill()?;
        }
        tokio::time::timeout(Duration::from_secs(5), child.wait()).await??;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    save(
        resources,
        "session-nested-port-forward-cleanup",
        &json!({"reaped":cleanup.is_ok()}),
    )?;
    result?;
    cleanup
}

fn select_hosts(
    pods: &Value,
    hosts: &Value,
    namespace: &str,
    image: &str,
    digest: &str,
) -> anyhow::Result<Vec<Value>> {
    let pods = array(pods, "/items")?
        .iter()
        .filter(|pod| pod["metadata"]["deletionTimestamp"].is_null())
        .collect::<Vec<_>>();
    ensure!(pods.len() == 2, "two production host pods are required");
    let mut pins = Vec::new();
    for pod in pods {
        ready_pod(pod, "host", image, digest)?;
        for host in array(hosts, "/items")?
            .iter()
            .filter(|host| host_matches(host, pod, namespace))
        {
            pins.push(
                json!({"host_id":text(host,"/hostId")?, "pod_uid":text(pod,"/metadata/uid")?,
                "pod_name":text(pod,"/metadata/name")?, "pod_ip":text(pod,"/status/podIP")?}),
            );
        }
    }
    distinct_pair(&pins, &["host_id", "pod_uid", "pod_ip"])?;
    pins.sort_by(|a, b| a["pod_name"].as_str().cmp(&b["pod_name"].as_str()));
    for (pin, name) in pins.iter_mut().zip(DEPLOYMENTS) {
        pin["deployment"] = json!(name);
    }
    Ok(pins)
}

fn host_matches(host: &Value, pod: &Value, namespace: &str) -> bool {
    host["metadata"]["deletionTimestamp"].is_null()
        && host["environment"] == namespace
        && host["httpPort"] == 80
        && host["hostId"].as_str().is_some_and(|id| !id.is_empty())
        && (host["hostname"] == pod["metadata"]["name"]
            || host["hostname"] == pod["status"]["podIP"])
        && condition(host, "Ready")
}

fn ready_deployment(deployment: &Value) -> anyhow::Result<()> {
    ensure!(
        deployment["spec"]["replicas"] == 1
            && deployment["status"]["currentReplicas"] == 1
            && deployment["status"]["replicas"]["expected"] == 1
            && deployment["status"]["replicas"]["current"] == 1
            && deployment["status"]["replicas"]["ready"] == 1
            && (deployment["status"]["replicas"]["unavailable"].is_null()
                || deployment["status"]["replicas"]["unavailable"] == 0)
            && condition(deployment, "Ready"),
        "each pinned deployment must have exactly one ready replica"
    );
    text(deployment, "/status/currentReplicaSet/name")?;
    Ok(())
}

fn owned_by(value: &Value, owner: &Value, kind: &str) -> bool {
    value
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .is_some_and(|owners| {
            owners.iter().any(|entry| {
                entry["kind"] == kind
                    && entry["controller"] == true
                    && entry["uid"] == owner["metadata"]["uid"]
                    && entry["name"] == owner["metadata"]["name"]
            })
        })
}

/// The live cluster objects one endpoint reconciliation reads.
struct ObservedObjects<'a> {
    pins: &'a [Value],
    deployments: &'a Value,
    replicasets: &'a Value,
    workloads: &'a Value,
    pods: &'a Value,
    hosts: &'a Value,
    service: &'a Value,
    slices: &'a Value,
}

fn host_endpoints(
    observed: &ObservedObjects<'_>,
    namespace: &str,
    image: &str,
    digest: &str,
    component: &str,
    route: &str,
) -> anyhow::Result<Vec<Value>> {
    let ObservedObjects {
        pins,
        deployments,
        replicasets,
        workloads,
        pods,
        hosts,
        service,
        slices,
    } = *observed;
    let mut addresses = Vec::new();
    for slice in array(slices, "/items")? {
        ensure!(
            slice["addressType"] == "IPv4"
                && slice
                    .pointer("/metadata/ownerReferences")
                    .and_then(Value::as_array)
                    .is_some_and(
                        |owners| owners.iter().any(|owner| owner["kind"] == "Service"
                            && owner["controller"] == true
                            && owner["uid"] == service["metadata"]["uid"])
                    ),
            "each endpoint slice belongs to the selected IPv4 Service"
        );
        let ports = array(slice, "/ports")?;
        ensure!(
            ports.len() == 1 && ports[0]["port"] == 80 && ports[0]["protocol"] == "TCP",
            "each endpoint slice has the declared HTTP port"
        );
        for endpoint in array(slice, "/endpoints")? {
            let values = array(endpoint, "/addresses")?;
            ensure!(
                endpoint["conditions"]["ready"] == true
                    && endpoint["conditions"]["serving"] == true
                    && values.len() == 1,
                "each endpoint is ready and serving one address"
            );
            addresses.push(values[0].as_str().context("the endpoint address is text")?);
        }
    }
    ensure!(
        addresses.len() == 2 && addresses.iter().collect::<BTreeSet<_>>().len() == 2,
        "the HTTP Service has exactly two distinct endpoint addresses"
    );
    ensure!(
        array(deployments, "/items")?.len() == 2,
        "exactly two pinned deployments are required"
    );
    let mut placements = Vec::new();
    for deployment in array(deployments, "/items")? {
        ready_deployment(deployment)?;
        for replicaset in array(replicasets, "/items")?.iter().filter(|replicaset| {
            replicaset["metadata"]["name"] == deployment["status"]["currentReplicaSet"]["name"]
                && owned_by(replicaset, deployment, "WorkloadDeployment")
        }) {
            for workload in array(workloads, "/items")?
                .iter()
                .filter(|workload| owned_by(workload, replicaset, "WorkloadReplicaSet"))
            {
                ensure!(
                    [deployment, replicaset, workload]
                        .into_iter()
                        .all(|value| value["metadata"]["namespace"] == namespace)
                        && workload["status"]["environment"] == namespace
                        && replicaset["spec"]["replicas"] == 1,
                    "the session placement stays in its environment with one replica"
                );
                for (value, path) in [
                    (deployment, "/spec/template/spec/components"),
                    (replicaset, "/spec/template/spec/components"),
                    (workload, "/spec/components"),
                ] {
                    let components = array(value, path)?;
                    ensure!(
                        components.len() == 1 && components[0]["image"] == component,
                        "the session placement runs only its exact HTTP component"
                    );
                }
                ensure!(
                    ["Config", "HostSelection", "Placement", "Sync", "Ready"]
                        .into_iter()
                        .all(|name| condition(workload, name)),
                    "the session workload has all five ready conditions"
                );
                placements.push((deployment, replicaset, workload));
            }
        }
    }
    ensure!(
        placements.len() == 2,
        "the two deployments each own one native workload"
    );
    let ids = placements.iter().map(|(deployment, replicaset, workload)| json!({
        "deployment_uid":deployment["metadata"]["uid"], "replicaset_uid":replicaset["metadata"]["uid"],
        "workload_uid":workload["metadata"]["uid"], "host_id":workload["status"]["hostId"],
    })).collect::<Vec<_>>();
    distinct_pair(
        &ids,
        &[
            "deployment_uid",
            "replicaset_uid",
            "workload_uid",
            "host_id",
        ],
    )?;
    let mut output = Vec::new();
    for (deployment, replicaset, workload) in placements {
        let host_id = text(workload, "/status/hostId")?;
        for pin in pins.iter().filter(|pin| {
            pin["deployment"] == deployment["metadata"]["name"] && pin["host_id"] == host_id
        }) {
            ensure!(
                deployment["spec"]["template"]["spec"]["hostId"] == host_id
                    && replicaset["spec"]["template"]["spec"]["hostId"] == host_id
                    && workload["spec"]["hostId"] == host_id
                    && workload["spec"]["kubernetes"]["service"]["name"] == "flow-http",
                "all native workload levels retain the selected host and Service"
            );
            for host in array(hosts, "/items")?
                .iter()
                .filter(|host| host["hostId"] == host_id)
            {
                for pod in array(pods, "/items")?.iter().filter(|pod| {
                    pod["metadata"]["uid"] == pin["pod_uid"] && host_matches(host, pod, namespace)
                }) {
                    ensure!(
                        pod["metadata"]["deletionTimestamp"].is_null(),
                        "the selected host pod is not being deleted"
                    );
                    ready_pod(pod, "host", image, digest)?;
                    let ip = text(pod, "/status/podIP")?;
                    ensure!(
                        addresses.contains(&ip),
                        "the selected host pod serves an HTTP endpoint"
                    );
                    output.push(json!({"deployment":pin["deployment"], "deployment_uid":deployment["metadata"]["uid"],
                        "replicaset_uid":replicaset["metadata"]["uid"], "workload_uid":workload["metadata"]["uid"],
                        "host_id":host_id, "pod_uid":pod["metadata"]["uid"], "pod_name":pod["metadata"]["name"],
                        "pod_ip":ip, "endpoint":format!("http://{ip}:80{route}")}));
                }
            }
        }
    }
    distinct_pair(&output, &["pod_uid", "pod_ip"])?;
    output.sort_by(|a, b| a["host_id"].as_str().cmp(&b["host_id"].as_str()));
    Ok(output)
}

fn processes(
    placements: &[Value],
    pods: &Value,
    hosts: &Value,
    namespace: &str,
    image: &str,
    digest: &str,
) -> anyhow::Result<Vec<Value>> {
    let mut output = Vec::new();
    for placement in placements {
        for pod in array(pods, "/items")?.iter().filter(|pod| {
            pod["metadata"]["uid"] == placement["pod_uid"]
                && pod["metadata"]["name"] == placement["pod_name"]
                && pod["metadata"]["namespace"] == namespace
                && pod["status"]["podIP"] == placement["pod_ip"]
                && pod["metadata"]["deletionTimestamp"].is_null()
        }) {
            ready_pod(pod, "host", image, digest)?;
            ensure!(condition(pod, "Ready"), "the warm host pod stays Ready");
            for container in array(pod, "/status/containerStatuses")?
                .iter()
                .filter(|container| container["name"] == "host")
            {
                let container_id = text(container, "/containerID")?;
                let id = container_id
                    .strip_prefix("containerd://")
                    .context("the host uses its native containerd process")?;
                ensure!(
                    id.len() == 64
                        && id
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                    "the host container id is canonical"
                );
                let started = text(container, "/state/running/startedAt")?;
                chrono::DateTime::parse_from_rfc3339(started)
                    .context("the host process has a valid start time")?;
                let restarts = container["restartCount"]
                    .as_u64()
                    .context("the host restart count is a nonnegative integer")?;
                for host in array(hosts, "/items")?.iter().filter(|host| {
                    host["hostId"] == placement["host_id"] && host_matches(host, pod, namespace)
                }) {
                    canonical_uuid(text(pod, "/metadata/uid")?)?;
                    canonical_uuid(text(host, "/hostId")?)?;
                    output.push(json!({"pod_uid":pod["metadata"]["uid"], "host_id":host["hostId"],
                        "container_id":container_id, "started_at":started, "restart_count":restarts}));
                }
            }
        }
    }
    distinct_pair(&output, &["pod_uid", "host_id", "container_id"])?;
    output.sort_by(|a, b| a["pod_uid"].as_str().cmp(&b["pod_uid"].as_str()));
    Ok(output)
}

fn canonical_uuid(value: &str) -> anyhow::Result<()> {
    ensure!(
        uuid::Uuid::parse_str(value)?.hyphenated().to_string() == value,
        "the session identifier is a canonical UUID"
    );
    Ok(())
}
fn distinct_pair(values: &[Value], fields: &[&str]) -> anyhow::Result<()> {
    ensure!(
        values.len() == 2,
        "exactly two session placements are required"
    );
    for field in fields {
        let first = values[0][field]
            .as_str()
            .context("the first placement has a text field")?;
        let second = values[1][field]
            .as_str()
            .context("the second placement has a text field")?;
        ensure!(
            !first.is_empty() && !second.is_empty() && first != second,
            "the session placements have distinct {field} values"
        );
    }
    Ok(())
}
fn condition(value: &Value, name: &str) -> bool {
    value
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition["type"] == name && condition["status"] == "True")
        })
}
fn ready_pod(pod: &Value, container: &str, image: &str, digest: &str) -> anyhow::Result<()> {
    ensure!(
        pod["status"]["phase"] == "Running"
            && array(pod, "/spec/containers")?
                .iter()
                .any(|entry| entry["name"] == container && entry["image"] == image)
            && array(pod, "/status/containerStatuses")?
                .iter()
                .any(|entry| entry["name"] == container
                    && entry["ready"] == true
                    && entry["imageID"]
                        .as_str()
                        .is_some_and(|id| id.ends_with(digest))),
        "the {container} pod runs the selected ready image"
    );
    Ok(())
}
fn text<'a>(value: &'a Value, path: &str) -> anyhow::Result<&'a str> {
    value
        .pointer(path)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("the native observation has text at {path}"))
}
fn array<'a>(value: &'a Value, path: &str) -> anyhow::Result<&'a Vec<Value>> {
    value
        .pointer(path)
        .and_then(Value::as_array)
        .with_context(|| format!("the native observation has an array at {path}"))
}
fn save(resources: &Resources, name: &str, value: &Value) -> anyhow::Result<()> {
    fs::write(
        resources.evidence.join(format!("{name}.json")),
        serde_json::to_vec_pretty(value)?,
    )?;
    Ok(())
}
async fn read_object(resources: &Resources, name: &str, args: &[&str]) -> anyhow::Result<Value> {
    let bytes = checked(
        kubectl(resources)
            .arg("--request-timeout=30s")
            .args(args)
            .args(["-o", "json"]),
    )
    .await?;
    let value = serde_json::from_slice(&bytes)?;
    save(resources, name, &value)?;
    Ok(value)
}
async fn key_command(resources: &Resources, name: &str, args: &[&str]) -> anyhow::Result<Value> {
    let bytes = tokio::time::timeout(
        Duration::from_secs(30),
        checked(
            kubectl(resources)
                .args([
                    "-n",
                    &resources.name,
                    "exec",
                    "deployment/host-session-identity",
                    "-c",
                    "identity",
                    "--",
                    "/usr/local/bin/wamn-identity",
                ])
                .args(args),
        ),
    )
    .await??;
    let value = serde_json::from_slice(&bytes)?;
    save(resources, name, &value)?;
    Ok(value)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionJob {
    api_version: &'static str,
    kind: &'static str,
    metadata: JobMetadata,
    spec: JobSpec,
}
#[derive(Serialize)]
struct JobMetadata {
    name: String,
    namespace: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JobSpec {
    backoff_limit: u32,
    active_deadline_seconds: u32,
    template: PodTemplate,
}
#[derive(Serialize)]
struct PodTemplate {
    spec: PodSpec,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PodSpec {
    restart_policy: &'static str,
    automount_service_account_token: bool,
    containers: Vec<JobContainer>,
    volumes: Vec<Volume>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JobContainer {
    name: &'static str,
    image: String,
    image_pull_policy: &'static str,
    command: [&'static str; 2],
    env: Vec<Env>,
    volume_mounts: Vec<Mount>,
}

fn session_job(
    namespace: &str,
    name: &str,
    image: &str,
    issuer: &str,
    endpoints: &str,
    available: bool,
) -> SessionJob {
    let env = [
        ("WAMN_IDENTITY_ISSUER", issuer.to_owned()),
        (
            "WAMN_IDENTITY_CA_FILE",
            "/etc/host-session-ca/ca.crt".to_owned(),
        ),
        (
            "WAMN_HOST_SESSION_FIXTURE_FILE",
            "/etc/host-session-fixture/fixture.json".to_owned(),
        ),
        ("WAMN_HOST_SESSION_ENDPOINTS", endpoints.to_owned()),
        ("WAMN_HOST_SESSION_JWKS_AVAILABLE", available.to_string()),
    ]
    .into_iter()
    .map(|(name, value)| Env {
        name: name.to_owned(),
        value: Some(value),
        rest: Preserved::new(),
    })
    .collect();
    SessionJob {
        api_version: "batch/v1",
        kind: "Job",
        metadata: JobMetadata {
            name: name.to_owned(),
            namespace: namespace.to_owned(),
        },
        spec: JobSpec {
            backoff_limit: 0,
            active_deadline_seconds: 430,
            template: PodTemplate {
                spec: PodSpec {
                    restart_policy: "Never",
                    automount_service_account_token: false,
                    containers: vec![JobContainer {
                        name: "host-session",
                        image: image.to_owned(),
                        image_pull_policy: "Never",
                        command: ["/usr/local/bin/wamn-gates", "host-session-test"],
                        env,
                        volume_mounts: vec![
                            mount("ca", "/etc/host-session-ca"),
                            mount("fixture", "/etc/host-session-fixture"),
                        ],
                    }],
                    volumes: vec![
                        ca_volume("ca"),
                        Volume {
                            name: "fixture".to_owned(),
                            config_map: None,
                            secret: Some(SecretVolume {
                                secret_name: "host-session-fixture".to_owned(),
                                default_mode: Some(256),
                                rest: Preserved::new(),
                            }),
                            rest: Preserved::new(),
                        },
                    ],
                },
            },
        },
    }
}

async fn run_job(
    cluster: &ReceivingCluster,
    stage: &str,
    issuer: &str,
    kid: &str,
    endpoints: &[Value],
    available: bool,
    gates: &(String, String),
) -> anyhow::Result<()> {
    let resources = &cluster.resources;
    let image = resources
        .gates_image
        .as_deref()
        .context("the session gates image is built")?;
    let addresses = endpoints
        .iter()
        .map(|endpoint| text(endpoint, "/endpoint"))
        .collect::<anyhow::Result<Vec<_>>>()?
        .join(",");
    let job_path = resources.evidence.join(format!("{stage}-job.json"));
    fs::write(
        &job_path,
        serde_json::to_vec_pretty(&session_job(
            &resources.name,
            stage,
            image,
            issuer,
            &addresses,
            available,
        ))?,
    )?;
    let started = chrono::Utc::now().timestamp();
    apply(resources, &job_path).await?;
    let result = async {
        let mut child = kubectl(resources)
            .args([
                "-n",
                &resources.name,
                "logs",
                "-f",
                &format!("job/{stage}"),
                "-c",
                "host-session",
                "--pod-running-timeout=120s",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::from(fs::File::create(
                resources
                    .evidence
                    .join(format!("{stage}-stream-errors.log")),
            )?))
            .kill_on_drop(true)
            .spawn()?;
        let streaming = tokio::time::timeout(Duration::from_secs(480), async {
            let mut lines = BufReader::new(
                child
                    .stdout
                    .take()
                    .context("the session Job logs have output")?,
            )
            .lines();
            let mut log = fs::File::create(resources.evidence.join(format!("{stage}-stream.log")))?;
            let mut warm = false;
            while let Some(line) = lines.next_line().await? {
                use std::io::Write as _;
                writeln!(log, "{line}")?;
                if line == "HOST_SESSION_WARM hosts=2" {
                    ensure!(!warm, "the session Job warmed exactly once");
                    warm = true;
                    let removed = key_command(
                        resources,
                        &format!("{stage}-removed"),
                        &["remove", "--kid", kid],
                    )
                    .await?;
                    ensure!(
                        removed["removed"] == true && removed["kid"] == kid,
                        "the active session key was removed"
                    );
                    if !available {
                        checked(kubectl(resources).args([
                            "-n",
                            &resources.name,
                            "scale",
                            "deployment/host-session-identity",
                            "--replicas=0",
                        ]))
                        .await?;
                    }
                }
            }
            ensure!(
                child.wait().await?.success(),
                "the session Job log process failed"
            );
            ensure!(
                warm,
                "the session Job must warm both hosts before key removal"
            );
            Ok::<(), anyhow::Error>(())
        })
        .await;
        let child_cleanup = async {
            if child.try_wait()?.is_none() {
                child.start_kill()?;
            }
            tokio::time::timeout(Duration::from_secs(5), child.wait()).await??;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        save(
            resources,
            &format!("{stage}-log-cleanup"),
            &json!({"reaped":child_cleanup.is_ok()}),
        )?;
        streaming??;
        child_cleanup?;
        checked(kubectl(resources).args([
            "-n",
            &resources.name,
            "wait",
            "--for=condition=Complete",
            &format!("job/{stage}"),
            "--timeout=60s",
        ]))
        .await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let observed = async {
        let job = read_object(
            resources,
            &format!("{stage}-state"),
            &["-n", &resources.name, "get", "job", stage],
        )
        .await?;
        let pods = read_object(
            resources,
            &format!("{stage}-pods"),
            &[
                "-n",
                &resources.name,
                "get",
                "pods",
                "-l",
                &format!("job-name={stage}"),
            ],
        )
        .await?;
        let bytes = checked(kubectl(resources).args([
            "-n",
            &resources.name,
            "logs",
            &format!("job/{stage}"),
            "-c",
            "host-session",
        ]))
        .await?;
        fs::write(resources.evidence.join(format!("{stage}.log")), &bytes)?;
        for endpoint in endpoints {
            let pod = text(endpoint, "/pod_name")?;
            let log = checked(kubectl(resources).args([
                "--request-timeout=30s",
                "-n",
                &resources.name,
                "logs",
                pod,
                "--all-containers",
            ]))
            .await?;
            fs::write(resources.evidence.join(format!("{stage}-{pod}.log")), log)?;
        }
        assert_job(
            &job,
            &pods,
            image,
            gates,
            std::str::from_utf8(&bytes)?,
            available,
            started,
        )?;
        save(
            resources,
            &format!("{stage}-result"),
            &json!({"result":"pass","hosts":2,"jwks_available":available}),
        )?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let cleanup = checked(kubectl(resources).args([
        "-n",
        &resources.name,
        "delete",
        "job",
        stage,
        "--wait=true",
        "--timeout=120s",
    ]))
    .await;
    save(
        resources,
        &format!("{stage}-job-cleanup"),
        &json!({"deleted":cleanup.is_ok()}),
    )?;
    result?;
    observed?;
    cleanup?;
    Ok(())
}

fn assert_job(
    job: &Value,
    pods: &Value,
    image: &str,
    gates: &(String, String),
    log: &str,
    available: bool,
    started: i64,
) -> anyhow::Result<()> {
    ensure!(
        condition(job, "Complete") && job["status"]["succeeded"] == 1 && !condition(job, "Failed"),
        "the session Job must complete successfully once"
    );
    text(job, "/metadata/uid")?;
    occurred_after(text(job, "/metadata/creationTimestamp")?, started)?;
    let transition = array(job, "/status/conditions")?
        .iter()
        .rfind(|condition| condition["type"] == "Complete" && condition["status"] == "True")
        .context("the Job has its Complete condition")?;
    occurred_after(text(transition, "/lastTransitionTime")?, started)?;
    let pods = array(pods, "/items")?;
    ensure!(
        pods.len() == 1 && owned_by(&pods[0], job, "Job"),
        "the session Job owns one pod"
    );
    let pod = &pods[0];
    ensure!(
        pod["status"]["phase"] == "Succeeded",
        "the session Job pod succeeded"
    );
    occurred_after(text(pod, "/metadata/creationTimestamp")?, started)?;
    occurred_after(text(pod, "/status/startTime")?, started)?;
    let declared_init = pod
        .pointer("/spec/initContainers")
        .and_then(Value::as_array);
    let observed_init = pod
        .pointer("/status/initContainerStatuses")
        .and_then(Value::as_array);
    ensure!(
        declared_init.map_or(0, Vec::len) == observed_init.map_or(0, Vec::len),
        "all session Job init containers ran"
    );
    for container in observed_init.into_iter().flatten() {
        ensure!(
            container["state"]["terminated"]["exitCode"] == 0,
            "the session Job init container succeeded"
        );
        occurred_after(text(container, "/state/terminated/finishedAt")?, started)?;
    }
    let container = array(pod, "/status/containerStatuses")?
        .iter()
        .find(|container| container["name"] == "host-session")
        .context("the session container ran")?;
    occurred_after(text(container, "/state/terminated/finishedAt")?, started)?;

    ensure!(
        array(pod, "/spec/containers")?
            .iter()
            .any(|container| container["name"] == "host-session" && container["image"] == image)
            && array(pod, "/status/containerStatuses")?
                .iter()
                .any(|container| container["name"] == "host-session"
                    && container["state"]["terminated"]["exitCode"] == 0
                    && container["imageID"]
                        .as_str()
                        .is_some_and(|id| id.ends_with(&gates.0) || id == gates.1)),
        "the session Job exited zero in the exact loaded gates image"
    );
    let final_line = format!("HOST_SESSION_TEST result=pass hosts=2 jwks_available={available}");
    ensure!(
        log.lines().filter(|line| *line == final_line).count() == 1
            && log
                .lines()
                .filter(|line| *line == "HOST_SESSION_WARM hosts=2")
                .count()
                == 1,
        "the session Job reports one warm result and one successful final result"
    );
    Ok(())
}

fn occurred_after(value: &str, started: i64) -> anyhow::Result<()> {
    ensure!(
        chrono::DateTime::parse_from_rfc3339(value)?.timestamp() >= started,
        "the session Job observation must belong to this run"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinning_changes_only_the_two_declared_host_placements() -> anyhow::Result<()> {
        let source = "apiVersion: v1\nkind: Service\nmetadata: {name: flow-http, namespace: receiving}\nspec: {ports: [{port: 80}]}\n---\napiVersion: runtime.wasmcloud.dev/v1alpha1\nkind: WorkloadDeployment\nmetadata: {name: flow-http, namespace: receiving}\nspec:\n  replicas: 1\n  template:\n    spec:\n      environment: receiving\n      components: [{name: flow-http, image: http-image}]\n";
        let pins = vec![
            json!({"deployment":DEPLOYMENTS[0],"host_id":"host-a"}),
            json!({"deployment":DEPLOYMENTS[1],"host_id":"host-b"}),
        ];
        let actual: Value = serde_yaml::from_str(&pinned_workloads(source, &pins)?)?;
        let original = serde_yaml::Deserializer::from_str(source)
            .map(Value::deserialize)
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(actual["items"][0], original[0]);
        for (index, pin) in pins.iter().enumerate() {
            let mut expected = original[1].clone();
            expected["metadata"]["name"] = pin["deployment"].clone();
            expected["spec"]["template"]["spec"]["hostId"] = pin["host_id"].clone();
            assert_eq!(actual["items"][index + 1], expected);
        }
        Ok(())
    }
}

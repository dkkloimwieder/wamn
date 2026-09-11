//! Native image, placement, and authority checks shared by application tests.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::rendering::MaterializerInput;

/// Host objects and pods observed after the requested release becomes ready.
#[derive(Debug)]
pub struct HostObservation {
    pub hosts: Value,
    pub pods: Value,
    pub runtime_digest: String,
}

/// Check the built host image and the same image loaded on all three nodes.
pub async fn image_ready(
    lifecycle: &Path,
    cluster: &str,
    work: &Path,
    image: &str,
    source_head: &str,
    build_profile: &str,
    evidence: &Path,
) -> anyhow::Result<String> {
    let labels = command_json(Command::new(lifecycle).args(["image-labels", image])).await?;
    save(evidence, "host-image-labels.json", &labels)?;
    ensure!(
        labels["wamn.dev/source-head"] == source_head
            && labels["wamn.dev/build-profile"] == build_profile,
        "the host image does not carry the requested source and build profile"
    );
    checked(kubectl(cluster, work).args([
        "wait",
        "--for=condition=Ready",
        "nodes",
        "--all",
        "--timeout=180s",
    ]))
    .await?;
    let nodes = command_json(kubectl(cluster, work).args(["get", "nodes", "-o", "json"])).await?;
    save(evidence, "nodes.json", &nodes)?;
    validate_nodes(&nodes)?;
    let names = checked(Command::new(lifecycle).args(["nodes", cluster])).await?;
    let names = String::from_utf8(names)?
        .lines()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    ensure!(
        names.len() == 3,
        "kind must report exactly three distinct nodes"
    );
    let mut rows = Vec::new();
    let mut expected = None;
    for node in names {
        let observed =
            command_json(Command::new(lifecycle).args(["node-image", &node, image])).await?;
        let tuple = image_tuple(&observed)?;
        if let Some(expected) = &expected {
            ensure!(
                expected == &tuple,
                "the loaded host image differs across kind nodes"
            );
        } else {
            expected = Some(tuple.clone());
        }
        rows.push(json!({"node":node,"runtime_digest":tuple.0,"config_id":tuple.1}));
    }
    save(evidence, "host-image-nodes.json", &json!(rows))?;
    Ok(expected.context("the host image was observed on a node")?.0)
}

/// Check the deployed chart, ready host pods, and native Host objects.
pub async fn hosts_ready(
    lifecycle: &Path,
    cluster: &str,
    work: &Path,
    namespace: &str,
    image: &str,
    runtime_digest: &str,
    replicas: u32,
    evidence: &Path,
) -> anyhow::Result<HostObservation> {
    let releases = command_json(
        Command::new(lifecycle)
            .arg("helm-releases")
            .arg(cluster)
            .arg(work),
    )
    .await?;
    save(evidence, "helm-releases.json", &releases)?;
    validate_releases(&releases, namespace)?;
    let deployment = command_json(kubectl(cluster, work).args([
        "-n",
        namespace,
        "get",
        "deployment",
        "hostgroup-default",
        "-o",
        "json",
    ]))
    .await?;
    save(evidence, "host-deployment.json", &deployment)?;
    validate_host_deployment(&deployment, image, replicas)?;
    let labels = deployment
        .pointer("/spec/selector/matchLabels")
        .and_then(Value::as_object)
        .context("host deployment has selector labels")?;
    ensure!(!labels.is_empty(), "host deployment selector is empty");
    let selector = labels
        .iter()
        .map(|(key, value)| {
            Ok(format!(
                "{key}={}",
                value.as_str().context("host selector value is text")?
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .join(",");
    let pods = command_json(kubectl(cluster, work).args([
        "-n", namespace, "get", "pods", "-l", &selector, "-o", "json",
    ]))
    .await?;
    save(evidence, "host-pods.json", &pods)?;
    validate_host_pods(&pods, image, runtime_digest, replicas)?;
    let mut observed = Value::Null;
    for _ in 0..120 {
        if let Ok(hosts) = command_json(kubectl(cluster, work).args([
            "-n",
            "wamn-system",
            "get",
            "hosts",
            "-l",
            "hostgroup=default",
            "-o",
            "json",
        ]))
        .await
        {
            observed = hosts;
            if validate_hosts(&observed, namespace, replicas).is_ok() {
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    save(evidence, "hosts.json", &observed)?;
    validate_hosts(&observed, namespace, replicas)?;
    Ok(HostObservation {
        hosts: observed,
        pods,
        runtime_digest: runtime_digest.to_owned(),
    })
}

/// Check the HTTP workload and its Service-owned endpoint placement.
pub async fn http_ready(
    cluster: &str,
    work: &Path,
    namespace: &str,
    hosts: &HostObservation,
    evidence: &Path,
) -> anyhow::Result<Value> {
    let workload = placed_workload(
        cluster,
        work,
        namespace,
        "flow-http",
        "flow-http",
        hosts,
        evidence,
    )
    .await?;
    let mut slices = Value::Null;
    for _ in 0..120 {
        slices = command_json(kubectl(cluster, work).args([
            "-n",
            namespace,
            "get",
            "endpointslices",
            "-l",
            "kubernetes.io/service-name=flow-http,wasmcloud.dev/route-manager=true",
            "-o",
            "json",
        ]))
        .await?;
        if slices["items"].as_array().is_some_and(|items| {
            items.len() == 1
                && items[0]["endpoints"]
                    .as_array()
                    .is_some_and(|endpoints| endpoints.len() == 1)
        }) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let service = command_json(kubectl(cluster, work).args([
        "-n",
        namespace,
        "get",
        "service",
        "flow-http",
        "-o",
        "json",
    ]))
    .await?;
    save(evidence, "flow-http-endpointslices.json", &slices)?;
    save(evidence, "flow-http-service.json", &service)?;
    validate_endpoints(&slices, &service, &workload, hosts)?;
    Ok(workload)
}

/// Check the accepted materializer declaration and its own native placement.
pub async fn materializer_ready(
    cluster: &str,
    work: &Path,
    expected: &MaterializerInput,
    hosts: &HostObservation,
    evidence: &Path,
) -> anyhow::Result<Value> {
    let workload = placed_workload(
        cluster,
        work,
        &expected.namespace,
        &expected.workload,
        "materializer",
        hosts,
        evidence,
    )
    .await?;
    let deployment: Value =
        serde_json::from_slice(&fs::read(evidence.join("materializer-deployment.json"))?)?;
    validate_materializer(&deployment, expected)?;
    Ok(workload)
}

/// Check the native refusal and exact Event for a foreign environment.
pub async fn cross_environment_refused(
    cluster: &str,
    work: &Path,
    namespace: &str,
    http_workload: &Value,
    evidence: &Path,
) -> anyhow::Result<()> {
    let target = format!("{namespace}-denied");
    let mut negative = http_workload.clone();
    let metadata = negative["metadata"]
        .as_object_mut()
        .context("HTTP workload has metadata")?;
    for key in [
        "annotations",
        "creationTimestamp",
        "generateName",
        "finalizers",
        "generation",
        "managedFields",
        "ownerReferences",
        "resourceVersion",
        "uid",
    ] {
        metadata.remove(key);
    }
    metadata.insert("name".into(), json!("flow-http-cross-environment"));
    metadata.insert(
        "labels".into(),
        json!({"app":"flow-http-cross-environment"}),
    );
    negative
        .as_object_mut()
        .context("HTTP workload is an object")?
        .remove("status");
    negative["spec"]["environment"] = json!(target);
    negative["spec"]
        .as_object_mut()
        .context("HTTP workload has a spec")?
        .remove("hostId");
    let path = work.join("cross-environment-workload.json");
    fs::write(&path, serde_json::to_vec_pretty(&negative)?)?;
    checked(kubectl(cluster, work).args(["apply", "-f"]).arg(&path)).await?;
    checked(kubectl(cluster, work).args([
        "-n",
        namespace,
        "wait",
        "--for=condition=HostSelection=False",
        "workload/flow-http-cross-environment",
        "--timeout=240s",
    ]))
    .await?;
    let observed = command_json(kubectl(cluster, work).args([
        "-n",
        namespace,
        "get",
        "workload",
        "flow-http-cross-environment",
        "-o",
        "json",
    ]))
    .await?;
    save(evidence, "cross-environment-workload.json", &observed)?;
    validate_cross_environment(&observed, &target)?;
    let uid = text_at(&observed, "/metadata/uid")?;
    let selector = format!("regarding.uid={uid}");
    let mut accepted = None;
    for _ in 0..240 {
        let events = command_json(kubectl(cluster, work).args([
            "-n",
            namespace,
            "get",
            "events.events.k8s.io",
            "--field-selector",
            &selector,
            "-o",
            "json",
        ]))
        .await?;
        if let Some(event) = items(&events)?
            .iter()
            .find(|event| validate_denied_event(event, namespace, &target, uid).is_ok())
        {
            accepted = Some(event.clone());
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    save(
        evidence,
        "cross-environment-event.json",
        &accepted.context("native CrossEnvironmentSchedulingDenied Event did not arrive")?,
    )?;
    let mut permissions = Vec::new();
    for (verb, expected) in [
        ("create", true),
        ("patch", true),
        ("get", false),
        ("list", false),
        ("watch", false),
        ("update", false),
        ("delete", false),
        ("deletecollection", false),
    ] {
        let output = kubectl(cluster, work)
            .args([
                "-n",
                namespace,
                "auth",
                "can-i",
                verb,
                "events.events.k8s.io",
                "--as=system:serviceaccount:wamn-system:wamn-runtime-operator",
            ])
            .kill_on_drop(true)
            .output()
            .await
            .context("read the operator Event authorization")?;
        let answer = String::from_utf8(output.stdout)?.trim().to_owned();
        ensure!(
            answer == if expected { "yes" } else { "no" },
            "operator Event permission {verb} differs from its declared scope"
        );
        ensure!(
            output.status.code() == Some(if expected { 0 } else { 1 }),
            "operator Event authorization command failed for {verb}"
        );
        permissions.push(json!({"verb":verb,"allowed":expected}));
    }
    save(
        evidence,
        "operator-events-authorization.json",
        &json!(permissions),
    )
}

/// Check the retained refusal signals after an idle materializer becomes ready.
pub async fn materializer_idle(
    cluster: &str,
    work: &Path,
    namespace: &str,
    hosts: &HostObservation,
    evidence: &Path,
) -> anyhow::Result<()> {
    let mut bytes = Vec::new();
    for pod in items(&hosts.pods)? {
        let name = text_at(pod, "/metadata/name")?;
        bytes.extend(
            checked(kubectl(cluster, work).args(["-n", namespace, "logs", name, "-c", "host"]))
                .await?,
        );
    }
    fs::write(evidence.join("materializer-host.log"), &bytes)?;
    let output = String::from_utf8(bytes).context("host logs are UTF-8")?;
    ensure!(
        !output.lines().any(|line| {
            line.contains("router delivery did not settle")
                || line
                    .match_indices("wamn::materializer ")
                    .any(|(index, marker)| {
                        let tail = &line[index + marker.len()..];
                        tail.starts_with("REFUSED") || tail.contains("failed")
                    })
        }),
        "the idle materializer reported a refusal, failure, or unsettled delivery"
    );
    save(
        evidence,
        "materializer-idle.json",
        &json!({"host_logs_checked":items(&hosts.pods)?.len(),"failure_lines":0}),
    )
}

/// Check the service through in-cluster DNS and compare the exact refusal body.
pub async fn unknown_route(
    cluster: &str,
    work: &Path,
    namespace: &str,
    image: &str,
    runtime_digest: &str,
    route_host: &str,
    evidence: &Path,
) -> anyhow::Result<()> {
    let script = r#"transport=$(curl --silent --show-error --connect-timeout 5 --max-time 15 --output /tmp/body --write-out '{"status":%{http_code},"content_type":"%{content_type}"}' --header "Host: $ROUTE_HOST" "http://flow-http.$NAMESPACE.svc.cluster.local/no-such-route")
body_hex=$(od -An -v -tx1 /tmp/body | tr -d ' \n')
printf '{"transport":%s,"body_hex":"%s"}\n' "$transport" "$body_hex" >/dev/termination-log
"#;
    let job = json!({"apiVersion":"batch/v1","kind":"Job",
    "metadata":{"name":"flow-http-reachability","namespace":namespace},
    "spec":{"activeDeadlineSeconds":60,"backoffLimit":0,"template":{"spec":{"restartPolicy":"Never","containers":[{
        "name":"probe","image":image,"imagePullPolicy":"Never","command":["/bin/sh","-ec"],"args":[script],
        "env":[{"name":"ROUTE_HOST","value":route_host},{"name":"NAMESPACE","value":namespace}]
    }]}}}});
    let path = work.join("flow-http-reachability.json");
    fs::write(&path, serde_json::to_vec_pretty(&job)?)?;
    checked(kubectl(cluster, work).args(["apply", "-f"]).arg(&path)).await?;
    checked(kubectl(cluster, work).args([
        "-n",
        namespace,
        "wait",
        "--for=condition=Complete",
        "job/flow-http-reachability",
        "--timeout=90s",
    ]))
    .await?;
    let job = command_json(kubectl(cluster, work).args([
        "-n",
        namespace,
        "get",
        "job",
        "flow-http-reachability",
        "-o",
        "json",
    ]))
    .await?;
    let pods = command_json(kubectl(cluster, work).args([
        "-n",
        namespace,
        "get",
        "pods",
        "-l",
        "job-name=flow-http-reachability",
        "-o",
        "json",
    ]))
    .await?;
    save(evidence, "flow-http-probe-job.json", &job)?;
    save(evidence, "flow-http-probe-pod.json", &pods)?;
    ensure!(
        job["status"]["succeeded"] == 1 && condition(&job["status"], "Complete", "True"),
        "the in-cluster HTTP Job did not complete exactly once"
    );
    let pods = items(&pods)?;
    ensure!(pods.len() == 1, "the in-cluster HTTP Job must have one pod");
    let pod = &pods[0];
    let containers = array_at(pod, "/spec/containers")?;
    let statuses = array_at(pod, "/status/containerStatuses")?;
    ensure!(
        pod["status"]["phase"] == "Succeeded"
            && containers.len() == 1
            && containers[0]["image"] == image
            && statuses.len() == 1
            && statuses[0]["ready"] == false
            && statuses[0]["imageID"]
                .as_str()
                .is_some_and(|id| id.ends_with(runtime_digest)),
        "the in-cluster HTTP Job did not run the retained host image successfully"
    );
    ensure!(
        statuses[0]["state"]["terminated"]["exitCode"] == 0,
        "the in-cluster HTTP client failed"
    );
    let response: Value = serde_json::from_str(text_at(&statuses[0], "/state/terminated/message")?)
        .context("the in-cluster HTTP client returned structured response data")?;
    validate_unknown_route(&response)?;
    save(
        evidence,
        "flow-http-response.json",
        &json!({"status":404,"content_type":"application/json", "host":route_host,
        "body":{"error":{"code":"route-not-found"}}}),
    )
}

fn validate_unknown_route(response: &Value) -> anyhow::Result<()> {
    let expected_hex = b"{\"error\":{\"code\":\"route-not-found\"}}"
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    ensure!(
        response["transport"]["status"] == 404
            && response["transport"]["content_type"] == "application/json"
            && response["body_hex"] == expected_hex,
        "the in-cluster route refusal differs from the exact HTTP contract"
    );
    Ok(())
}

async fn placed_workload(
    cluster: &str,
    work: &Path,
    namespace: &str,
    name: &str,
    prefix: &str,
    hosts: &HostObservation,
    evidence: &Path,
) -> anyhow::Result<Value> {
    checked(kubectl(cluster, work).args([
        "-n",
        namespace,
        "wait",
        "--for=condition=Ready",
        &format!("workloaddeployment/{name}"),
        "--timeout=240s",
    ]))
    .await?;
    let deployment = command_json(kubectl(cluster, work).args([
        "-n",
        namespace,
        "get",
        "workloaddeployment",
        name,
        "-o",
        "json",
    ]))
    .await?;
    save(evidence, &format!("{prefix}-deployment.json"), &deployment)?;
    validate_deployment(&deployment)?;
    let replica_name = text_at(&deployment, "/status/currentReplicaSet/name")?;
    let replica = command_json(kubectl(cluster, work).args([
        "-n",
        namespace,
        "get",
        "workloadreplicaset",
        replica_name,
        "-o",
        "json",
    ]))
    .await?;
    save(evidence, &format!("{prefix}-replicaset.json"), &replica)?;
    let uid = text_at(&replica, "/metadata/uid")?;
    let mut selected = Vec::new();
    for _ in 0..120 {
        let workloads = command_json(kubectl(cluster, work).args([
            "-n",
            namespace,
            "get",
            "workloads",
            "-o",
            "json",
        ]))
        .await?;
        selected = items(&workloads)?
            .iter()
            .filter(|workload| {
                workload["metadata"]["ownerReferences"]
                    .as_array()
                    .is_some_and(|owners| owners.iter().any(|owner| owner["uid"] == uid))
            })
            .cloned()
            .collect();
        if selected.len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    ensure!(
        selected.len() == 1,
        "the positive workload is missing or ambiguous"
    );
    let workload = selected.remove(0);
    save(evidence, &format!("{prefix}-workload.json"), &workload)?;
    validate_placement(&workload, namespace, &hosts.hosts)?;
    Ok(workload)
}

fn validate_nodes(nodes: &Value) -> anyhow::Result<()> {
    let nodes = items(nodes)?;
    let names = nodes
        .iter()
        .map(|node| text_at(node, "/metadata/name"))
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    ensure!(
        nodes.len() == 3
            && names.len() == 3
            && nodes
                .iter()
                .all(|node| node["status"]["nodeInfo"]["architecture"] == "amd64"
                    && condition(&node["status"], "Ready", "True")),
        "the cluster must have exactly three distinct ready amd64 nodes"
    );
    Ok(())
}

fn image_tuple(image: &Value) -> anyhow::Result<(String, String)> {
    let references = array_at(image, "/status/repoDigests")?
        .iter()
        .map(|value| value.as_str().context("image repository digest is text"))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let digests = references
        .into_iter()
        .filter_map(|reference| reference.rsplit_once('@').map(|(_, digest)| digest))
        .filter(|digest| is_digest(digest))
        .collect::<BTreeSet<_>>();
    ensure!(
        digests.len() == 1,
        "the loaded image has no unique runtime digest"
    );
    let config = text_at(image, "/status/id")?;
    ensure!(is_digest(config), "the loaded image config id is invalid");
    Ok((
        digests
            .into_iter()
            .next()
            .context("runtime digest exists")?
            .to_owned(),
        config.to_owned(),
    ))
}

fn validate_releases(releases: &Value, namespace: &str) -> anyhow::Result<()> {
    let releases = releases
        .as_array()
        .context("Helm returns a release array")?;
    for (name, namespace) in [("wamn", "wamn-system"), ("wamn-host", namespace)] {
        ensure!(
            releases
                .iter()
                .filter(|release| release["name"] == name
                    && release["namespace"] == namespace
                    && release["status"] == "deployed"
                    && release["chart"] == "runtime-operator-2.9.0"
                    && release["app_version"] == "2.9.0")
                .count()
                == 1,
            "the declared runtime-operator release is missing or ambiguous"
        );
    }
    Ok(())
}

fn validate_host_deployment(deployment: &Value, image: &str, replicas: u32) -> anyhow::Result<()> {
    ensure!(
        deployment["status"]["observedGeneration"] == deployment["metadata"]["generation"]
            && deployment["spec"]["replicas"] == replicas
            && deployment["status"]["updatedReplicas"] == replicas
            && deployment["status"]["readyReplicas"] == replicas
            && deployment["status"]["availableReplicas"] == replicas
            && array_at(deployment, "/spec/template/spec/containers")?
                .iter()
                .any(|container| container["image"] == image),
        "the host deployment does not match its declared generation, replica count, and image"
    );
    Ok(())
}

fn validate_host_pods(
    pods: &Value,
    image: &str,
    digest: &str,
    replicas: u32,
) -> anyhow::Result<()> {
    let pods = items(pods)?;
    ensure!(
        pods.len() == replicas as usize,
        "the host pod count differs from the declared replicas"
    );
    for pod in pods {
        let names = array_at(pod, "/spec/containers")?
            .iter()
            .filter(|container| container["image"] == image)
            .map(|container| text_at(container, "/name"))
            .collect::<anyhow::Result<Vec<_>>>()?;
        ensure!(
            names.len() == 1
                && array_at(pod, "/status/containerStatuses")?
                    .iter()
                    .any(|status| status["name"] == names[0]
                        && status["ready"] == true
                        && status["imageID"]
                            .as_str()
                            .is_some_and(|id| id.ends_with(digest))),
            "a host pod is not ready on the exact loaded image"
        );
    }
    Ok(())
}

fn validate_hosts(hosts: &Value, namespace: &str, replicas: u32) -> anyhow::Result<()> {
    let hosts = items(hosts)?;
    let ids = hosts
        .iter()
        .map(|host| text_at(host, "/hostId"))
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    ensure!(
        hosts.len() == replicas as usize && ids.len() == replicas as usize,
        "native Host count or identity differs from the declared replicas"
    );
    for host in hosts {
        ensure!(
            host["metadata"]["labels"]["hostgroup"] == "default"
                && host["environment"] == namespace
                && !text_at(host, "/hostId")?.is_empty()
                && !text_at(host, "/hostname")?.is_empty()
                && host["httpPort"] == 80
                && condition(&host["status"], "Ready", "True"),
            "a native Host has the wrong environment, identity, port, or readiness"
        );
    }
    Ok(())
}

fn validate_deployment(deployment: &Value) -> anyhow::Result<()> {
    ensure!(
        deployment["spec"]["replicas"] == 1
            && deployment["status"]["currentReplicas"] == 1
            && deployment["status"]["replicas"]["expected"] == 1
            && deployment["status"]["replicas"]["current"] == 1
            && deployment["status"]["replicas"]["ready"] == 1
            && (deployment["status"]["replicas"]["unavailable"].is_null()
                || deployment["status"]["replicas"]["unavailable"] == false
                || deployment["status"]["replicas"]["unavailable"] == 0)
            && !text_at(deployment, "/status/currentReplicaSet/name")?.is_empty()
            && condition(&deployment["status"], "Ready", "True"),
        "the workload deployment does not have exactly one ready declared replica"
    );
    Ok(())
}

fn validate_placement(workload: &Value, namespace: &str, hosts: &Value) -> anyhow::Result<()> {
    let host_id = text_at(workload, "/status/hostId")?;
    ensure!(
        workload["status"]["environment"] == namespace
            && !host_id.is_empty()
            && items(hosts)?.iter().any(|host| host["hostId"] == host_id)
            && ["Config", "HostSelection", "Placement", "Sync", "Ready"]
                .iter()
                .all(|name| condition(&workload["status"], name, "True")),
        "the workload is not ready on one of this environment's native Hosts"
    );
    Ok(())
}

fn validate_endpoints(
    slices: &Value,
    service: &Value,
    workload: &Value,
    hosts: &HostObservation,
) -> anyhow::Result<()> {
    let slices = items(slices)?;
    ensure!(
        slices.len() == 1,
        "the HTTP route must have one EndpointSlice"
    );
    let slice = &slices[0];
    let host_id = text_at(workload, "/status/hostId")?;
    let selected = items(&hosts.hosts)?
        .iter()
        .filter(|host| host["hostId"] == host_id)
        .collect::<Vec<_>>();
    ensure!(selected.len() == 1, "the route selects one native Host");
    let endpoints = array_at(slice, "/endpoints")?;
    let ports = array_at(slice, "/ports")?;
    ensure!(
        endpoints.len() == 1 && array_at(&endpoints[0], "/addresses")?.len() == 1,
        "the HTTP route must have one endpoint address"
    );
    let address = text_at(&endpoints[0], "/addresses/0")?;
    let hostname = text_at(selected[0], "/hostname")?;
    ensure!(
        slice["addressType"] == "IPv4"
            && slice["metadata"]["labels"]["kubernetes.io/service-name"] == "flow-http"
            && slice["metadata"]["labels"]["wasmcloud.dev/route-manager"] == "true"
            && array_at(slice, "/metadata/ownerReferences")?
                .iter()
                .any(|owner| owner["uid"] == service["metadata"]["uid"]
                    && owner["kind"] == "Service"
                    && owner["controller"] == true)
            && ports.len() == 1
            && ports[0]["name"] == "http"
            && ports[0]["protocol"] == "TCP"
            && ports[0]["port"] == selected[0]["httpPort"]
            && endpoints[0]["conditions"]["ready"] == true
            && endpoints[0]["conditions"]["serving"] == true
            && items(&hosts.pods)?
                .iter()
                .any(|pod| pod["status"]["podIP"] == address)
            && (hostname == address
                || items(&hosts.pods)?
                    .iter()
                    .any(|pod| pod["metadata"]["name"] == hostname
                        && pod["status"]["podIP"] == address)),
        "the Service-owned HTTP EndpointSlice differs from its selected native Host and pod"
    );
    Ok(())
}

fn validate_materializer(deployment: &Value, expected: &MaterializerInput) -> anyhow::Result<()> {
    let spec = &deployment["spec"]["template"]["spec"];
    ensure!(
        deployment["metadata"]["name"] == expected.workload
            && deployment["metadata"]["namespace"] == expected.namespace
            && deployment["spec"]["replicas"] == 1
            && spec["environment"] == expected.namespace
            && spec["hostSelector"] == json!({"hostgroup":"default"})
            && spec["service"]["image"] == expected.image
            && spec["service"]["localResources"]["config"]
                == json!({
                    "wamn.tenant":expected.tenant,"wamn.project":expected.event.project,
                    "wamn.environment":expected.event.environment,"wamn.postgres.authority":"event-materializer"
                }),
        "the accepted materializer declaration has the wrong identity, image, selector, or authority"
    );
    let environment = &spec["service"]["localResources"]["environment"]["config"];
    for (name, value) in [
        ("WAMN_MAT_STREAM", expected.event_stream.as_str()),
        ("WAMN_MAT_ORG", expected.event.org.as_str()),
        ("WAMN_MAT_PROJECT", expected.event.project.as_str()),
        ("WAMN_MAT_ENV", expected.event.environment.as_str()),
        ("WAMN_MAT_TENANT", expected.tenant.as_str()),
    ] {
        ensure!(
            environment[name] == value,
            "accepted materializer input {name} differs from its declaration"
        );
    }
    ensure!(
        environment["WAMN_MAT_FETCH_MS"] == expected.fetch_ms.to_string()
            && environment["WAMN_MAT_SWEEP_MS"] == expected.sweep_ms.to_string(),
        "accepted materializer intervals differ from their declaration"
    );
    let mut interfaces = array_at(spec, "/hostInterfaces")?.clone();
    interfaces.sort_by(|left, right| left["package"].as_str().cmp(&right["package"].as_str()));
    ensure!(
        json!(interfaces)
            == json!([
                {"namespace":"wamn","package":"flow-http-routing","version":"0.1.0","interfaces":["routing"]},
                {"namespace":"wamn","package":"jetstream","version":"0.1.0","interfaces":["types","registration"]},
                {"namespace":"wasmcloud","package":"nats","version":"0.1.0","name":"events","interfaces":["types","jetstream"]},
                {"namespace":"wamn","package":"postgres","version":"0.1.0","interfaces":["types","client"]},
                {"namespace":"wamn","package":"router-delivery","version":"0.1.0","interfaces":["delivery"]}
            ]),
        "accepted materializer interfaces differ from the native declaration"
    );
    validate_deployment(deployment)
}

fn validate_cross_environment(workload: &Value, target: &str) -> anyhow::Result<()> {
    ensure!(
        workload["spec"]["environment"] == target
            && (workload["status"]["hostId"].is_null()
                || workload["status"]["hostId"] == false
                || workload["status"]["hostId"] == "")
            && (workload["status"]["environment"].is_null()
                || workload["status"]["environment"] == false
                || workload["status"]["environment"] == "")
            && array_at(workload, "/status/conditions")?
                .iter()
                .any(|condition| condition["type"] == "HostSelection"
                    && condition["status"] == "False"
                    && condition["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("allowSharedHosts")))
            && !condition(&workload["status"], "Ready", "True"),
        "the foreign environment was not refused before host selection"
    );
    Ok(())
}

fn validate_denied_event(
    event: &Value,
    namespace: &str,
    target: &str,
    uid: &str,
) -> anyhow::Result<()> {
    ensure!(
        event["apiVersion"] == "events.k8s.io/v1"
            && event["kind"] == "Event"
            && event["regarding"]["apiVersion"] == "runtime.wasmcloud.dev/v1alpha1"
            && event["regarding"]["kind"] == "Workload"
            && event["regarding"]["namespace"] == namespace
            && event["regarding"]["name"] == "flow-http-cross-environment"
            && event["regarding"]["uid"] == uid
            && event["type"] == "Warning"
            && event["reason"] == "CrossEnvironmentSchedulingDenied"
            && event["action"] == "Reject"
            && event["reportingController"] == "workload-controller"
            && event["note"]
                == format!(
                    "Environment \"{target}\" is outside \"{namespace}\" and operator has allowSharedHosts=false"
                ),
        "the native Event does not describe the exact cross-environment refusal"
    );
    Ok(())
}

fn condition(status: &Value, name: &str, value: &str) -> bool {
    status["conditions"].as_array().is_some_and(|conditions| {
        conditions
            .iter()
            .any(|condition| condition["type"] == name && condition["status"] == value)
    })
}

fn is_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn items(value: &Value) -> anyhow::Result<&Vec<Value>> {
    array_at(value, "/items")
}
fn array_at<'a>(value: &'a Value, pointer: &str) -> anyhow::Result<&'a Vec<Value>> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .with_context(|| format!("observed object requires array {pointer}"))
}
fn text_at<'a>(value: &'a Value, pointer: &str) -> anyhow::Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .with_context(|| format!("observed object requires text {pointer}"))
}
fn save(directory: &Path, name: &str, value: &Value) -> anyhow::Result<()> {
    fs::write(directory.join(name), serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("write observed {name}"))
}
fn kubectl(cluster: &str, work: &Path) -> Command {
    let mut command = Command::new("kubectl");
    command
        .arg("--kubeconfig")
        .arg(work.join("kubeconfig"))
        .arg("--context")
        .arg(format!("kind-{cluster}"));
    command
}
async fn command_json(command: &mut Command) -> anyhow::Result<Value> {
    serde_json::from_slice(&checked(command).await?)
        .context("read the observed Kubernetes or image JSON")
}
async fn checked(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    let output = command
        .kill_on_drop(true)
        .output()
        .await
        .context("run the native platform check")?;
    ensure!(
        output.status.success(),
        "native platform command failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    // These are observed native objects from the recorded three-host run.
    const DENIED: &str = include_str!(
        "../../docs/perf/2026.09/1-component-cache/journey3/cross-environment-workload.json"
    );
    const EVENT: &str = include_str!(
        "../../docs/perf/2026.09/1-component-cache/journey3/cross-environment-event.json"
    );
    const DEPLOYMENT: &str = include_str!(
        "../../docs/perf/2026.09/1-component-cache/journey3/flow-http-deployment.json"
    );
    const SLICES: &str = include_str!(
        "../../docs/perf/2026.09/1-component-cache/journey3/flow-http-endpointslices.json"
    );
    const SERVICE: &str =
        include_str!("../../docs/perf/2026.09/1-component-cache/journey3/flow-http-service.json");
    const WORKLOAD: &str =
        include_str!("../../docs/perf/2026.09/1-component-cache/journey3/flow-http-workload.json");
    const HOSTS: &str =
        include_str!("../../docs/perf/2026.09/1-component-cache/journey3/hosts.json");
    const PODS: &str =
        include_str!("../../docs/perf/2026.09/1-component-cache/journey3/host-pods.json");

    #[test]
    fn refused_environment_cannot_carry_a_host_or_ready_condition() {
        let original: Value = serde_json::from_str(DENIED).unwrap();
        let target = original["spec"]["environment"].as_str().unwrap();
        validate_cross_environment(&original, target).unwrap();
        for host in [json!("foreign-host"), json!(1), json!(true)] {
            let mut changed = original.clone();
            changed["status"]["hostId"] = host;
            assert!(validate_cross_environment(&changed, target).is_err());
        }
        let mut changed = original.clone();
        changed["status"]["conditions"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"Ready","status":"True"}));
        assert!(validate_cross_environment(&changed, target).is_err());
    }

    #[test]
    fn denial_event_must_name_the_exact_object_and_environment() {
        let original: Value = serde_json::from_str(EVENT).unwrap();
        let namespace = original["regarding"]["namespace"].as_str().unwrap();
        let target = format!("{namespace}-denied");
        let uid = original["regarding"]["uid"].as_str().unwrap();
        validate_denied_event(&original, namespace, &target, uid).unwrap();
        for pointer in [
            "/regarding/uid",
            "/regarding/namespace",
            "/reason",
            "/action",
            "/note",
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = json!("another-object-or-action");
            assert!(
                validate_denied_event(&changed, namespace, &target, uid).is_err(),
                "{pointer}"
            );
        }
    }

    #[test]
    fn endpoint_must_match_the_service_selected_host_and_ready_pod() {
        let original: Value = serde_json::from_str(SLICES).unwrap();
        let service = serde_json::from_str(SERVICE).unwrap();
        let workload = serde_json::from_str(WORKLOAD).unwrap();
        let hosts = HostObservation {
            hosts: serde_json::from_str(HOSTS).unwrap(),
            pods: serde_json::from_str(PODS).unwrap(),
            runtime_digest: String::new(),
        };
        validate_endpoints(&original, &service, &workload, &hosts).unwrap();
        for (pointer, replacement) in [
            ("/items/0/ports/0/port", json!(81)),
            (
                "/items/0/metadata/ownerReferences/0/uid",
                json!("foreign-service"),
            ),
            ("/items/0/endpoints/0/addresses/0", json!("203.0.113.99")),
            ("/items/0/endpoints/0/conditions/ready", json!(false)),
            ("/items/0/endpoints/0/conditions/serving", json!(false)),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = replacement;
            assert!(
                validate_endpoints(&changed, &service, &workload, &hosts).is_err(),
                "{pointer}"
            );
        }
    }

    #[test]
    fn workload_count_remains_separate_from_stream_settings() {
        let original: Value = serde_json::from_str(DEPLOYMENT).unwrap();
        validate_deployment(&original).unwrap();
        for pointer in [
            "/spec/replicas",
            "/status/currentReplicas",
            "/status/replicas/expected",
            "/status/replicas/current",
            "/status/replicas/ready",
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = json!(2);
            assert!(validate_deployment(&changed).is_err(), "{pointer}");
        }
        let mut changed = original;
        changed["status"]["replicas"]["unavailable"] = json!("invalid");
        assert!(validate_deployment(&changed).is_err());
    }

    #[test]
    fn in_cluster_response_requires_the_exact_status_type_and_body_bytes() {
        let hex = b"{\"error\":{\"code\":\"route-not-found\"}}"
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let response =
            json!({"transport":{"status":404,"content_type":"application/json"},"body_hex":hex});
        validate_unknown_route(&response).unwrap();
        for (pointer, value) in [
            ("/transport/status", json!(200)),
            ("/transport/content_type", json!("text/html")),
            ("/body_hex", json!(format!("{hex}0a"))),
        ] {
            let mut changed = response.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(validate_unknown_route(&changed).is_err());
        }
    }
}

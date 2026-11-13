//! Native image, placement, and authority tests shared by application tests.

use std::collections::BTreeSet;
use std::fs;
use std::net::Ipv4Addr;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;

/// Pinned curl 8.12.1 client used by in-cluster HTTP probes (linux/amd64).
pub const HTTP_PROBE_IMAGE: &str =
    "curlimages/curl@sha256:88a9abad9d958340e48564f9bdcdaa29916a2984be59314da709f8bbc0eef6f7";

/// Read the owned container's IPv4 address from restricted Docker output.
pub fn kind_address(settings: &Value) -> anyhow::Result<Ipv4Addr> {
    let address: Ipv4Addr = settings["Networks"]["kind"]["IPAddress"]
        .as_str()
        .context("the owned container has a kind-network address")?
        .parse()
        .context("the owned container has an IPv4 kind-network address")?;
    ensure!(
        !address.is_unspecified(),
        "the kind-network address is empty"
    );
    Ok(address)
}

/// Read the sole PostgreSQL port published on the loopback interface.
pub fn postgres_host_port(settings: &Value) -> anyhow::Result<u16> {
    let ports = settings["Ports"]["5432/tcp"]
        .as_array()
        .context("the owned PostgreSQL container publishes port 5432")?;
    ensure!(
        ports.len() == 1 && ports[0]["HostIp"] == "127.0.0.1",
        "PostgreSQL must publish one loopback port"
    );
    let port = ports[0]["HostPort"]
        .as_str()
        .context("the PostgreSQL mapping names its host port")?
        .parse::<u16>()
        .context("the PostgreSQL host port is valid")?;
    ensure!(port != 0, "the PostgreSQL host port must be allocated");
    Ok(port)
}

/// Record a build command and its private output without recording environment values.
pub async fn record_build(
    evidence: &Path,
    name: &str,
    command: &mut Command,
) -> anyhow::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::Stdio;
    use std::time::Instant;

    fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)
            .with_context(|| format!("write private file {}", path.display()))
    }

    let standard = command.as_std();
    let argv = std::iter::once(standard.get_program())
        .chain(standard.get_args())
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let cwd = standard
        .get_current_dir()
        .map(Path::to_path_buf)
        .map_or_else(std::env::current_dir, Ok)?;
    let stdout_path = evidence.join(format!("{name}.stdout.log"));
    let stderr_path = evidence.join(format!("{name}.stderr.log"));
    write_private(
        &evidence.join(format!("{name}-command.json")),
        &serde_json::to_vec_pretty(&json!({
            "argv": argv, "cwd": cwd,
            "stdout": stdout_path, "stderr": stderr_path,
        }))?,
    )?;
    let stdout = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&stdout_path)?;
    let stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&stderr_path)?;
    let started = Instant::now();
    let status = command
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .kill_on_drop(true)
        .status()
        .await;
    let result = match &status {
        Ok(status) => json!({
            "exit_code": status.code(), "signal": status.signal(),
            "status": status.to_string(), "passed": status.success(),
            "elapsed_seconds": started.elapsed().as_secs_f64(),
        }),
        Err(error) => json!({
            "exit_code": null, "signal": null, "passed": false,
            "failure": error.to_string(),
            "elapsed_seconds": started.elapsed().as_secs_f64(),
        }),
    };
    write_private(
        &evidence.join(format!("{name}-result.json")),
        &serde_json::to_vec_pretty(&result)?,
    )?;
    let status = status.with_context(|| format!("start {name}"))?;
    ensure!(
        status.success(),
        "{name} failed with {status}; see {} and {}",
        stdout_path.display(),
        stderr_path.display(),
    );
    Ok(())
}

/// Host objects and pods observed after the requested release becomes ready.
#[derive(Debug)]
pub struct HostObservation {
    pub hosts: Value,
    pub pods: Value,
    pub runtime_digest: String,
}

/// Make sure of the built host image and the same image loaded on all three nodes.
pub async fn image_ready(
    cluster: &str,
    work: &Path,
    image: &str,
    build_profile: &str,
    evidence: &Path,
) -> anyhow::Result<String> {
    let names =
        require_success(Command::new("kind").args(["get", "nodes", "--name", cluster])).await?;
    let node = std::str::from_utf8(&names)?
        .lines()
        .next()
        .context("type reported no node")?;
    let observed = command_json(&mut node_image(node, image)).await?;
    let config = observed
        .pointer("/info/imageSpec")
        .context("CRI returned no image configuration")?;
    let labels = config["config"]["Labels"].clone();
    save(evidence, "host-image-labels.json", &labels)?;
    ensure!(
        labels["wamn.dev/build-profile"] == build_profile,
        "the host image does not carry the requested build profile"
    );
    require_success(kubectl(cluster, work).args([
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
    let names =
        require_success(Command::new("kind").args(["get", "nodes", "--name", cluster])).await?;
    let names = String::from_utf8(names)?
        .lines()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    ensure!(
        names.len() == 3,
        "type must report exactly three distinct nodes"
    );
    let loaded = loaded_image_digest(image, &names, evidence).await;
    if loaded.is_err() {
        capture_node_images(&names, evidence).await;
    }
    loaded
}

/// The one runtime digest that every node reports for this image.
async fn loaded_image_digest(
    image: &str,
    names: &BTreeSet<String>,
    evidence: &Path,
) -> anyhow::Result<String> {
    let mut inspections = Vec::new();
    for node in names {
        let bytes = require_success(&mut node_image(node, image)).await?;
        fs::write(evidence.join(format!("node-image-{node}.json")), &bytes)?;
        inspections.push((node, bytes));
    }
    let mut rows = Vec::new();
    let mut expected = None;
    for (node, bytes) in inspections {
        let observed = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse image {image} on {node}"))?;
        let tuple =
            image_tuple(&observed).with_context(|| format!("test image {image} on {node}"))?;
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

/// Write what each node actually holds, after an image test failed.
///
/// A kind load reported success and the first node then reported no such
/// image. The test aborts at that node, so it writes nothing for the node
/// that failed and nothing at all for the nodes after it, and the cause stays
/// unproven. This runs only on the failure path and keeps its own failures out
/// of the way, because it is evidence and not a test (wamn-y2ml).
pub async fn capture_node_images(names: &BTreeSet<String>, evidence: &Path) {
    for node in names {
        for (mut command, file) in [
            (
                docker(&["exec", node, "crictl", "images", "-o", "json"]),
                "cri-images.json",
            ),
            (
                docker(&["exec", node, "ctr", "--namespace", "k8s.io", "images", "ls"]),
                "containerd-images.txt",
            ),
        ] {
            let Ok(Ok(output)) =
                tokio::time::timeout(Duration::from_secs(30), command.kill_on_drop(true).output())
                    .await
            else {
                continue;
            };
            let path = evidence.join(format!("failure-{node}-{file}"));
            let _ = fs::write(path, [output.stdout, output.stderr].concat());
        }
    }
}

/// Make sure of the native refusal and exact Event for a foreign environment.
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
    require_success(kubectl(cluster, work).args(["apply", "-f"]).arg(&path)).await?;
    require_success(kubectl(cluster, work).args([
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
            "operator Event permission {verb} differs from its declared target"
        );
        ensure!(
            output.status.code() == Some(i32::from(!expected)),
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

/// Make sure of the retained refusal signals after an idle materializer becomes ready.
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
            require_success(
                kubectl(cluster, work).args(["-n", namespace, "logs", name, "-c", "host"]),
            )
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
        &json!({"host_logs_tested":items(&hosts.pods)?.len(),"failure_lines":0}),
    )
}

/// Make sure of the service through in-cluster DNS and compare the exact refusal body.
pub async fn unknown_route(
    cluster: &str,
    work: &Path,
    namespace: &str,
    service: &str,
    route_host: &str,
    evidence: &Path,
) -> anyhow::Result<()> {
    // curlimages/curl 8.12.1, linux/amd64 manifest and its exact config identity.
    let (_, image_digest) = HTTP_PROBE_IMAGE
        .split_once('@')
        .expect("pinned probe digest");
    let config_digest = "sha256:5dce198cca467ce79994ed65e01d03882238f9efdd16a8c6f4bc55151c8a4a54";
    let image = HTTP_PROBE_IMAGE;
    let script = r#"transport=$(curl --silent --show-error --connect-timeout 5 --max-time 15 --output /tmp/body --write-out '{"status":%{http_code},"content_type":"%{content_type}"}' --header "Host: $ROUTE_HOST" "http://$SERVICE.$NAMESPACE.svc.cluster.local/no-such-route")
body_hex=$(od -An -v -tx1 /tmp/body | tr -d ' \n')
printf '{"transport":%s,"body_hex":"%s"}\n' "$transport" "$body_hex" >/dev/termination-log
"#;
    let job = json!({"apiVersion":"batch/v1","kind":"Job",
    "metadata":{"name":"flow-http-reachability","namespace":namespace},
    "spec":{"activeDeadlineSeconds":60,"backoffLimit":0,"template":{"spec":{"restartPolicy":"Never","containers":[{
        "name":"probe","image":image,"imagePullPolicy":"IfNotPresent","command":["/bin/sh","-ec"],"args":[script],
        "env":[{"name":"ROUTE_HOST","value":route_host},{"name":"NAMESPACE","value":namespace},{"name":"SERVICE","value":service}]
    }]}}}});
    let path = work.join("flow-http-reachability.json");
    fs::write(&path, serde_json::to_vec_pretty(&job)?)?;
    require_success(kubectl(cluster, work).args(["apply", "-f"]).arg(&path)).await?;
    let completed = require_success(kubectl(cluster, work).args([
        "-n",
        namespace,
        "wait",
        "--for=condition=Complete",
        "job/flow-http-reachability",
        "--timeout=90s",
    ]))
    .await;
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
    if let Err(error) = completed {
        let logs = require_success(kubectl(cluster, work).args([
            "-n",
            namespace,
            "logs",
            "job/flow-http-reachability",
            "--tail=40",
        ]))
        .await
        .unwrap_or_else(|error| format!("probe logs unavailable: {error:#}").into_bytes());
        fs::write(evidence.join("flow-http-probe.log"), &logs)?;
        return Err(error.context(format!(
            "HTTP probe job status {}; pods {}; logs: {}",
            job["status"],
            pods,
            String::from_utf8_lossy(&logs)
        )));
    }
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
                .is_some_and(|id| id.ends_with(image_digest) || id.ends_with(config_digest)),
        "the in-cluster HTTP Job did not run the pinned client image successfully"
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
    let expected_hex = b"{\"error\":{\"code\":\"route-not-found\"}}".iter().fold(
        String::new(),
        |mut out, byte| {
            use std::fmt::Write as _;
            write!(out, "{byte:02x}").expect("writing to a string is infallible");
            out
        },
    );
    ensure!(
        response["transport"]["status"] == 404
            && response["transport"]["content_type"] == "application/json"
            && response["body_hex"] == expected_hex,
        "the in-cluster route refusal differs from the exact HTTP contract"
    );
    Ok(())
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
fn docker(args: &[&str]) -> Command {
    let mut command = Command::new("docker");
    command.args(args);
    command
}
fn node_image(node: &str, image: &str) -> Command {
    docker(&["exec", node, "crictl", "inspecti", "-o", "json", image])
}
async fn command_json(command: &mut Command) -> anyhow::Result<Value> {
    serde_json::from_slice(&require_success(command).await?)
        .context("read the observed Kubernetes or image JSON")
}
async fn require_success(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    let output = tokio::time::timeout(
        Duration::from_secs(300),
        command.kill_on_drop(true).output(),
    )
    .await
    .context("workload observation deadline")??;
    ensure!(
        output.status.success(),
        "workload observation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::ScratchRoot;
    use std::os::unix::fs::PermissionsExt as _;

    #[tokio::test]
    async fn failed_image_build_retains_command_status_and_output_bytes() {
        let root = ScratchRoot::create().unwrap();
        let script = "printf 'stdout\\000data\\n'; printf 'stderr\\377data\\n' >&2; exit 7";
        let failure = record_build(
            root.path(),
            "build-images",
            Command::new("/bin/sh")
                .current_dir(root.path())
                .env("WAMN_TEST_PRIVATE_VALUE", "not-for-evidence")
                .args(["-c", script]),
        )
        .await
        .unwrap_err();
        assert!(failure.to_string().contains("build-images.stderr.log"));
        assert_eq!(
            fs::read(root.path().join("build-images.stdout.log")).unwrap(),
            b"stdout\0data\n"
        );
        assert_eq!(
            fs::read(root.path().join("build-images.stderr.log")).unwrap(),
            b"stderr\xffdata\n"
        );
        let command = fs::read_to_string(root.path().join("build-images-command.json")).unwrap();
        assert!(!command.contains("not-for-evidence"));
        let command: Value = serde_json::from_str(&command).unwrap();
        assert_eq!(command["argv"], json!(["/bin/sh", "-c", script]));
        assert_eq!(command["cwd"], json!(root.path()));
        let result: Value = serde_json::from_slice(
            &fs::read(root.path().join("build-images-result.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(result["exit_code"], 7);
        assert_eq!(result["passed"], false);
        assert!(result["signal"].is_null());
        for file in fs::read_dir(root.path()).unwrap() {
            assert_eq!(
                file.unwrap().metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn image_build_spawn_failure_is_retained_before_returning() {
        let root = ScratchRoot::create().unwrap();
        assert!(
            record_build(
                root.path(),
                "build-identity",
                Command::new(root.path().join("absent-build-command")).current_dir(root.path()),
            )
            .await
            .is_err()
        );
        let result: Value = serde_json::from_slice(
            &fs::read(root.path().join("build-identity-result.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(result["passed"], false);
        assert!(result["exit_code"].is_null());
        assert!(!result["failure"].as_str().unwrap().is_empty());
        assert!(
            fs::read(root.path().join("build-identity.stdout.log"))
                .unwrap()
                .is_empty()
        );
        assert!(
            fs::read(root.path().join("build-identity.stderr.log"))
                .unwrap()
                .is_empty()
        );
        assert!(root.path().join("build-identity-command.json").is_file());
    }

    #[test]
    fn docker_network_output_requires_private_allocated_addresses() {
        let settings = json!({
            "Networks":{"kind":{"IPAddress":"172.18.0.5"}},
            "Ports":{"5432/tcp":[{"HostIp":"127.0.0.1","HostPort":"41327"}]},
        });
        assert_eq!(kind_address(&settings).unwrap().to_string(), "172.18.0.5");
        assert_eq!(postgres_host_port(&settings).unwrap(), 41327);
        for ports in [
            json!([{"HostIp":"0.0.0.0","HostPort":"41327"}]),
            json!([{"HostIp":"127.0.0.1","HostPort":"0"}]),
            json!([{"HostIp":"127.0.0.1","HostPort":"41327"},{"HostIp":"::1","HostPort":"41327"}]),
            json!([{"HostIp":"127.0.0.1","HostPort":"65536"}]),
            json!([]),
            json!(null),
        ] {
            assert!(postgres_host_port(&json!({"Ports":{"5432/tcp":ports}})).is_err());
        }
        for networks in [
            json!({"bridge":{"IPAddress":"172.18.0.4"}}),
            json!({"kind":{"IPAddress":"0.0.0.0"}}),
            json!({"kind":{"IPAddress":"::1"}}),
            json!({"kind":{"IPAddress":""}}),
        ] {
            assert!(kind_address(&json!({"Networks":networks})).is_err());
        }
    }

    // These are observed native objects from the recorded three-host run.
    const DENIED: &str = include_str!("fixtures/workload/cross-environment-workload.json");
    const EVENT: &str = include_str!("fixtures/workload/cross-environment-event.json");

    #[test]
    fn recorded_images_require_one_runtime_digest_and_a_config_id() {
        // Both inputs are historical native output; neither is the failed RC inspection.
        let identity: Value =
            serde_json::from_str(include_str!("fixtures/workload/identity-node-image.json"))
                .unwrap();
        let expected = (
            "sha256:a3dafaa1a09101a5a4eaaeedd12fc21542a8bb4b3c66fb8dd3fa01850ec16021".to_owned(),
            "sha256:6d7e01c8efdb85c004da7c5924b7b83a8489f3ab97b55a0aaeed45601e795e77".to_owned(),
        );
        assert_eq!(image_tuple(&identity).unwrap(), expected);

        let mut alias = identity.clone();
        alias["status"]["repoDigests"]
            .as_array_mut()
            .unwrap()
            .push(json!(format!("another-repository@{}", expected.0)));
        assert_eq!(image_tuple(&alias).unwrap(), expected);

        let mut ambiguous = identity.clone();
        ambiguous["status"]["repoDigests"]
            .as_array_mut()
            .unwrap()
            .push(json!(format!("another-repository@{}", expected.1)));
        assert!(image_tuple(&ambiguous).is_err());

        let mut missing = identity.clone();
        missing["status"]["repoDigests"] = json!(["repository-without-a-digest"]);
        assert!(image_tuple(&missing).is_err());
        let mut invalid_config = identity;
        invalid_config["status"]["id"] = json!("invalid");
        assert!(image_tuple(&invalid_config).is_err());
    }

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
    fn in_cluster_response_requires_the_exact_status_type_and_body_bytes() {
        let hex = b"{\"error\":{\"code\":\"route-not-found\"}}".iter().fold(
            String::new(),
            |mut out, byte| {
                use std::fmt::Write as _;
                write!(out, "{byte:02x}").expect("writing to a string is infallible");
                out
            },
        );
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

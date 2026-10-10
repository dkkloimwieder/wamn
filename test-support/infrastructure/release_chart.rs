//! Checks of the release chart that `wamn-ctl env apply` writes, on a kind
//! cluster (docs/plan/platform-deploy.md §9.2, assumptions A1 and A12).
//!
//! The environment release is `deploy/platform/release`, in the operator
//! namespace. Its host group is the release name, and its hosts carry the
//! `wamn.release` label of the release manifest digest.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::workload::HostObservation;

/// The chart name and version that `helm list` reports for an environment release.
pub const CHART: &str = "release-0.1.0";

/// How long the A1 probe waits for a host it must not get.
const PROBE_WAIT: Duration = Duration::from_secs(120);

/// The namespaced kinds whose objects an environment owns, or its owners create.
const OWNED_KINDS: [&str; 7] = [
    "deployments",
    "services",
    "workloaddeployments",
    "workloadreplicasets",
    "workloads",
    "secrets",
    "configmaps",
];

/// One environment release on one cluster, and where its checks write.
#[derive(Debug, Clone, Copy)]
pub struct Release<'a> {
    pub cluster: &'a str,
    /// The directory that holds `kubeconfig`.
    pub work: &'a Path,
    pub namespace: &'a str,
    /// The Helm release name, which is also the host group.
    pub name: &'a str,
    /// The `wamn.release` label value of the manifest digest.
    pub label: &'a str,
    /// The manifest digest, `sha256:<hex>`.
    pub digest: &'a str,
    pub evidence: &'a Path,
}

/// Every Helm release in every namespace, in every state. Helm 4 has no
/// `--all`, so each state is named.
pub async fn helm_releases(cluster: &str, work: &Path) -> anyhow::Result<Vec<Value>> {
    let releases = command_json(
        Command::new("helm")
            .arg("list")
            .arg("--kubeconfig")
            .arg(work.join("kubeconfig"))
            .args([
                "--kube-context",
                &format!("kind-{cluster}"),
                "--all-namespaces",
                "--deployed",
                "--failed",
                "--pending",
                "--superseded",
                "--uninstalling",
                "--uninstalled",
                "-o",
                "json",
            ]),
    )
    .await?;
    Ok(releases
        .as_array()
        .context("Helm returns a release array")?
        .clone())
}

/// The one Helm release named `release.name`, in any namespace and state.
pub async fn helm_release(release: &Release<'_>) -> anyhow::Result<Option<Value>> {
    let named: Vec<Value> = helm_releases(release.cluster, release.work)
        .await?
        .into_iter()
        .filter(|entry| entry["name"] == release.name)
        .collect();
    ensure!(
        named.len() <= 1,
        "Helm lists release {} more than once",
        release.name
    );
    Ok(named.into_iter().next())
}

/// Check the release, its host Deployment and pods, and the Host objects of its group.
pub async fn hosts_ready(
    release: &Release<'_>,
    image: &str,
    runtime_digest: &str,
    replicas: u32,
) -> anyhow::Result<HostObservation> {
    let Release {
        cluster,
        work,
        namespace,
        name,
        label,
        digest,
        evidence,
    } = *release;
    let entry = helm_release(release)
        .await?
        .with_context(|| format!("Helm lists no release {name}"))?;
    save(evidence, "helm-release.json", &entry)?;
    ensure!(
        entry["namespace"] == namespace && entry["chart"] == CHART && entry["status"] == "deployed",
        "release {name} is not one deployed {CHART} release in {namespace}"
    );
    let deployment = get(release, "deployment", &format!("hostgroup-{name}")).await?;
    save(evidence, "host-deployment.json", &deployment)?;
    let containers = array_at(&deployment, "/spec/template/spec/containers")?;
    ensure!(
        deployment["status"]["observedGeneration"] == deployment["metadata"]["generation"]
            && deployment["spec"]["replicas"] == replicas
            && deployment["status"]["readyReplicas"] == replicas
            && deployment["status"]["updatedReplicas"] == replicas
            && containers.len() == 1
            && containers[0]["image"] == image,
        "host Deployment hostgroup-{name} is not ready with {replicas} replicas of {image}"
    );
    // R1: the release is the manifest digest in the host's arguments.
    ensure!(
        array_at(&containers[0], "/args")?
            .iter()
            .any(|argument| argument == &json!(format!("--release-manifest-digest={digest}"))),
        "the host arguments do not carry the release manifest digest"
    );
    let pods = command_json(kubectl(cluster, work).args([
        "-n",
        namespace,
        "get",
        "pods",
        "-l",
        &format!("wasmcloud.com/hostgroup={name}"),
        "-o",
        "json",
    ]))
    .await?;
    save(evidence, "host-pods.json", &pods)?;
    let running: Vec<&Value> = items(&pods)?
        .iter()
        .filter(|pod| pod["metadata"]["deletionTimestamp"].is_null())
        .collect();
    ensure!(
        running.len() == replicas as usize,
        "host group {name} has {} pods, not {replicas}",
        running.len()
    );
    for pod in running {
        ensure!(
            pod["metadata"]["labels"]["wamn.release"] == label
                && pod["metadata"]["annotations"]["wamn.release-digest"] == digest
                && array_at(pod, "/status/containerStatuses")?
                    .iter()
                    .any(|status| status["ready"] == true
                        && status["imageID"]
                            .as_str()
                            .is_some_and(|id| id.ends_with(runtime_digest))),
            "host pod {} lacks the release label, the digest annotation or the ready host image",
            text_at(pod, "/metadata/name")?
        );
    }
    let mut hosts = Value::Null;
    for _ in 0..120 {
        if let Ok(observed) = command_json(kubectl(cluster, work).args([
            "-n",
            namespace,
            "get",
            "hosts",
            "-l",
            &format!("hostgroup={name}"),
            "-o",
            "json",
        ]))
        .await
        {
            hosts = observed;
            if validate_hosts(&hosts, release, replicas).is_ok() {
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    save(evidence, "hosts.json", &hosts)?;
    validate_hosts(&hosts, release, replicas)?;
    Ok(HostObservation {
        hosts,
        pods,
        runtime_digest: runtime_digest.to_owned(),
    })
}

/// A1, the heartbeat half: each Host of the group carries both selector labels.
fn validate_hosts(hosts: &Value, release: &Release<'_>, replicas: u32) -> anyhow::Result<()> {
    let hosts = items(hosts)?;
    let ids = hosts
        .iter()
        .map(|host| text_at(host, "/hostId"))
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    ensure!(
        hosts.len() == replicas as usize && ids.len() == replicas as usize,
        "host group {} has {} Host objects, not {replicas}",
        release.name,
        hosts.len()
    );
    for host in hosts {
        ensure!(
            host["metadata"]["labels"]["hostgroup"] == release.name
                && host["metadata"]["labels"]["wamn.release"] == release.label
                && condition(&host["status"], "Ready", "True"),
            "Host {} lacks hostgroup={} and wamn.release={}, or is not ready",
            text_at(host, "/metadata/name")?,
            release.name,
            release.label
        );
    }
    Ok(())
}

/// Check that role `role` is placed, and only on hosts of this release.
///
/// For `http`, the role Service's route EndpointSlices point only at this
/// group's pods.
pub async fn role_placed(
    release: &Release<'_>,
    role: &str,
    hosts: &HostObservation,
) -> anyhow::Result<Value> {
    let Release {
        cluster,
        work,
        namespace,
        name,
        label,
        evidence,
        ..
    } = *release;
    let deployment_name = format!("{name}-{role}");
    checked(kubectl(cluster, work).args([
        "-n",
        namespace,
        "wait",
        "--for=condition=Ready",
        &format!("workloaddeployment/{deployment_name}"),
        "--timeout=240s",
    ]))
    .await?;
    let deployment = get(release, "workloaddeployment", &deployment_name).await?;
    save(evidence, &format!("{role}-deployment.json"), &deployment)?;
    ensure!(
        deployment["spec"]["template"]["spec"]["hostSelector"]
            == json!({"hostgroup": name, "wamn.release": label})
            && condition(&deployment["status"], "Ready", "True"),
        "WorkloadDeployment {deployment_name} is not ready with the release hostSelector"
    );
    let replica_name = text_at(&deployment, "/status/currentReplicaSet/name")?;
    let replica = get(release, "workloadreplicaset", replica_name).await?;
    save(evidence, &format!("{role}-replicaset.json"), &replica)?;
    let uid = text_at(&replica, "/metadata/uid")?;
    let mut selected = Vec::new();
    for _ in 0..120 {
        selected = owned(release, "workloads", uid).await?;
        if selected.len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    ensure!(
        selected.len() == 1,
        "WorkloadDeployment {deployment_name} owns {} current Workloads, not one",
        selected.len()
    );
    let workload = selected.remove(0);
    save(evidence, &format!("{role}-workload.json"), &workload)?;
    let host_id = text_at(&workload, "/status/hostId")?;
    ensure!(
        items(&hosts.hosts)?.iter().any(
            |host| host["hostId"] == host_id && host["metadata"]["labels"]["hostgroup"] == name
        ) && condition(&workload["status"], "Ready", "True"),
        "Workload of {deployment_name} is not ready on a Host of group {name}"
    );
    if role == "http" {
        http_endpoints(release, hosts).await?;
    }
    Ok(workload)
}

async fn http_endpoints(release: &Release<'_>, hosts: &HostObservation) -> anyhow::Result<()> {
    let Release {
        cluster,
        work,
        namespace,
        name,
        evidence,
        ..
    } = *release;
    let addresses = |slices: &Value| -> anyhow::Result<Vec<String>> {
        let mut addresses = Vec::new();
        for slice in items(slices)? {
            for endpoint in slice["endpoints"].as_array().into_iter().flatten() {
                for address in array_at(endpoint, "/addresses")? {
                    addresses.push(address.as_str().context("address is text")?.to_owned());
                }
            }
        }
        Ok(addresses)
    };
    let mut slices = Value::Null;
    for _ in 0..120 {
        slices = command_json(kubectl(cluster, work).args([
            "-n",
            namespace,
            "get",
            "endpointslices",
            "-l",
            &format!("kubernetes.io/service-name={name}-http,wasmcloud.dev/route-manager=true"),
            "-o",
            "json",
        ]))
        .await?;
        if !addresses(&slices)?.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    save(evidence, "http-endpointslices.json", &slices)?;
    let pods = items(&hosts.pods)?
        .iter()
        .filter_map(|pod| pod["status"]["podIP"].as_str())
        .collect::<BTreeSet<_>>();
    let addresses = addresses(&slices)?;
    ensure!(
        !addresses.is_empty()
            && addresses
                .iter()
                .all(|address| pods.contains(address.as_str())),
        "Service {name}-http has endpoints {addresses:?} outside host group {name} ({pods:?})"
    );
    Ok(())
}

/// A1, the selector half: a copy of the release's http WorkloadDeployment
/// whose `hostSelector` names another `wamn.release` value is placed on no
/// host. The probe is removed before this returns.
pub async fn a1_probe(release: &Release<'_>) -> anyhow::Result<Value> {
    // Another lowercase base32 value of the same length: the last character changed.
    let mut wrong = release.label.to_owned();
    let last = wrong.pop().context("the release label is not empty")?;
    wrong.push(if last == 'a' { 'b' } else { 'a' });
    let http = get(
        release,
        "workloaddeployment",
        &format!("{}-http", release.name),
    )
    .await?;
    let mut spec = http["spec"].clone();
    spec["template"]["spec"]["hostSelector"] =
        json!({"hostgroup": release.name, "wamn.release": wrong});
    spec["template"]["spec"]
        .as_object_mut()
        .context("the http template has a spec")?
        .remove("kubernetes");
    let name = format!("{}-a1-probe", release.name);
    let probe = json!({"apiVersion":"runtime.wasmcloud.dev/v1alpha1","kind":"WorkloadDeployment",
        "metadata":{"name":name,"namespace":release.namespace},"spec":spec});
    let path = release.work.join("a1-probe.json");
    fs::write(&path, serde_json::to_vec_pretty(&probe)?)?;
    checked(
        kubectl(release.cluster, release.work)
            .args(["apply", "-f"])
            .arg(&path),
    )
    .await?;
    tokio::time::sleep(PROBE_WAIT).await;
    let deployment = get(release, "workloaddeployment", &name).await?;
    let uid = text_at(&deployment, "/metadata/uid")?.to_owned();
    let sets = owned(release, "workloadreplicasets", &uid).await?;
    let mut workloads = Vec::new();
    for set in &sets {
        workloads.extend(owned(release, "workloads", text_at(set, "/metadata/uid")?).await?);
    }
    let observed = json!({"selector":spec["template"]["spec"]["hostSelector"],
        "deployment":deployment,"replicasets":sets,"workloads":workloads});
    save(release.evidence, "a1-probe.json", &observed)?;
    checked(kubectl(release.cluster, release.work).args([
        "-n",
        release.namespace,
        "delete",
        "workloaddeployment",
        &name,
        "--wait",
    ]))
    .await?;
    ensure!(
        !sets.is_empty(),
        "the operator made no WorkloadReplicaSet for the A1 probe in {} s",
        PROBE_WAIT.as_secs()
    );
    let placed: Vec<&Value> = workloads
        .iter()
        .filter(|workload| {
            workload["status"]["hostId"]
                .as_str()
                .is_some_and(|host| !host.is_empty())
        })
        .collect();
    ensure!(
        placed.is_empty(),
        "A1 is false: the probe with a wrong wamn.release was placed: {placed:?}"
    );
    Ok(
        json!({"selector":observed["selector"],"replicasets":sets.len(),
        "workloads":workloads.len(),"placed":0}),
    )
}

/// One environment of the shared namespace: its release name and its
/// coordinate in the Secret form `<org>--<project>--<env>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Environment {
    pub name: String,
    pub coordinate: String,
}

/// A12: the environments of one namespace share no object name, and the
/// host pods of each reference no Secret or ConfigMap of another.
///
/// Every object whose name holds an environment's release name or its
/// coordinate belongs to that environment. Shared objects, such as the
/// operator release's TLS Secrets and the platform registry Secret, hold
/// neither and are listed apart.
pub async fn environments_isolated(
    cluster: &str,
    work: &Path,
    namespace: &str,
    environments: &[Environment],
    evidence: &Path,
) -> anyhow::Result<Value> {
    ensure!(
        environments.len() >= 2,
        "A12 needs two environments in {namespace}, found {}",
        environments.len()
    );
    let mut owned_names = BTreeMap::<&str, Vec<String>>::new();
    let mut shared = Vec::new();
    for kind in OWNED_KINDS {
        let names =
            checked(kubectl(cluster, work).args(["-n", namespace, "get", kind, "-o", "name"]))
                .await?;
        for object in String::from_utf8(names)?.lines() {
            let owners: Vec<&Environment> = environments
                .iter()
                .filter(|environment| {
                    object.contains(&environment.name) || object.contains(&environment.coordinate)
                })
                .collect();
            match owners.as_slice() {
                [] => shared.push(object.to_owned()),
                [owner] => owned_names
                    .entry(owner.name.as_str())
                    .or_default()
                    .push(object.to_owned()),
                several => anyhow::bail!(
                    "A12 is false: {object} carries the names of {:?}",
                    several.iter().map(|owner| &owner.name).collect::<Vec<_>>()
                ),
            }
        }
    }
    let mut references = serde_json::Map::new();
    for environment in environments {
        ensure!(
            owned_names.contains_key(environment.name.as_str()),
            "environment {} owns no object in {namespace}",
            environment.name
        );
        let pods = command_json(kubectl(cluster, work).args([
            "-n",
            namespace,
            "get",
            "pods",
            "-l",
            &format!("wasmcloud.com/hostgroup={}", environment.name),
            "-o",
            "json",
        ]))
        .await?;
        let mut foreign = Vec::new();
        for pod in items(&pods)? {
            let spec = pod["spec"].to_string();
            for other in environments.iter().filter(|other| *other != environment) {
                if spec.contains(&other.coordinate) || spec.contains(&other.name) {
                    foreign.push(format!(
                        "{} references {}",
                        text_at(pod, "/metadata/name")?,
                        other.name
                    ));
                }
            }
        }
        references.insert(environment.name.clone(), json!(foreign));
    }
    let observed = json!({"environments":environments.iter().map(|environment| json!({
            "name":environment.name,"coordinate":environment.coordinate})).collect::<Vec<_>>(),
        "owned":owned_names,"shared":shared,"foreign_references":references});
    save(evidence, "a12-environments.json", &observed)?;
    ensure!(
        references
            .values()
            .all(|foreign| foreign.as_array().is_some_and(Vec::is_empty)),
        "A12 is false: a host pod references another environment: {references:?}"
    );
    Ok(observed)
}

/// The objects of `kind` in the release namespace whose owner has `uid`.
pub async fn owned(release: &Release<'_>, kind: &str, uid: &str) -> anyhow::Result<Vec<Value>> {
    let objects = command_json(kubectl(release.cluster, release.work).args([
        "-n",
        release.namespace,
        "get",
        kind,
        "-o",
        "json",
    ]))
    .await?;
    Ok(items(&objects)?
        .iter()
        .filter(|object| {
            object["metadata"]["ownerReferences"]
                .as_array()
                .is_some_and(|owners| owners.iter().any(|owner| owner["uid"] == uid))
        })
        .cloned()
        .collect())
}

/// One object of `kind` named `name` in the release namespace.
pub async fn get(release: &Release<'_>, kind: &str, name: &str) -> anyhow::Result<Value> {
    command_json(kubectl(release.cluster, release.work).args([
        "-n",
        release.namespace,
        "get",
        kind,
        name,
        "-o",
        "json",
    ]))
    .await
}

fn condition(status: &Value, name: &str, value: &str) -> bool {
    status["conditions"].as_array().is_some_and(|conditions| {
        conditions
            .iter()
            .any(|condition| condition["type"] == name && condition["status"] == value)
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
    serde_json::from_slice(&checked(command).await?).context("read the observed JSON")
}
async fn checked(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    let output = command
        .kill_on_drop(true)
        .output()
        .await
        .context("run the release chart check")?;
    ensure!(
        output.status.success(),
        "release chart check command failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

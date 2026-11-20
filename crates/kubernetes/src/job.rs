//! Bounded platform Job execution and observation.
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use std::time::Duration;

use crate::{Target, kubectl};

// Bound execution even if the caller exits. Kubernetes never retries these Jobs.
const INSTALL_DEADLINE: Duration = Duration::from_mins(15);

pub fn manifest(name: &str, image: &str, command: &[&str], arguments: &[&str]) -> Value {
    json!({"apiVersion":"batch/v1", "kind":"Job", "metadata":{"name":name},
        "spec":{"suspend":true, "backoffLimit":0, "activeDeadlineSeconds":INSTALL_DEADLINE.as_secs(),
            "template":{"metadata":{"labels":{"app.kubernetes.io/component":"wamn-worker"}},
                "spec":{"restartPolicy":"Never", "automountServiceAccountToken":false,
                "containers":[{"name":"operation", "image":image, "command":command, "args":arguments, "env":[], "volumeMounts":[]}],
                "volumes":[]}}}})
}

fn name(job: &Value) -> &str {
    job["metadata"]["name"].as_str().expect("platform Job name")
}

pub fn container(job: &mut Value) -> &mut Value {
    &mut job["spec"]["template"]["spec"]["containers"][0]
}

pub fn environment(job: &mut Value, values: &[(&str, &str)]) {
    let env = container(job)["env"].as_array_mut().unwrap();
    env.extend(
        values
            .iter()
            .map(|(name, value)| json!({"name":name,"value":value})),
    );
}

pub fn mount(job: &mut Value, name: &str, secret: &str, path: &str, keys: &[&str]) {
    job["spec"]["template"]["spec"]["volumes"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name":name, "secret":{"secretName":secret, "defaultMode":256,
                "items":keys.iter().map(|key| json!({"key":key,"path":key})).collect::<Vec<_>>()}
        }));
    container(job)["volumeMounts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":name, "mountPath":path, "readOnly":true}));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum State {
    Pending,
    Running,
    Succeeded,
    Failed,
}

fn state(job: &Value) -> State {
    let conditions = job["status"]["conditions"].as_array();
    let has = |type_| {
        conditions.is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition["type"] == type_ && condition["status"] == "True")
        })
    };
    if has("Failed") {
        State::Failed
    } else if has("Complete") {
        State::Succeeded
    } else if job["status"]["active"].as_u64().unwrap_or(0) > 0 {
        State::Running
    } else {
        State::Pending
    }
}

/// Wait for a named Job to reach a terminal condition within the caller's deadline.
pub async fn wait(target: &crate::Target, name: &str, timeout: Duration) -> anyhow::Result<()> {
    let failed = std::cell::Cell::new(false);
    crate::kubectl::wait_named(target, "job", name, timeout, |job| match state(job) {
        State::Succeeded => true,
        State::Failed => {
            failed.set(true);
            true
        }
        State::Pending | State::Running => false,
    })
    .await?;
    ensure!(!failed.get(), "Job {}/{} failed", target.namespace, name);
    Ok(())
}

/// Wait for the previous Job to finish and remove it and its Pods.
pub async fn clear_finished(
    target: &Target,
    name: &str,
    deadline: tokio::time::Instant,
) -> anyhow::Result<()> {
    tokio::time::timeout_at(deadline, clear(target, name))
        .await
        .map_err(anyhow::Error::from)
        .flatten()
        .with_context(|| format!("wait to replace Job {}/{name}", target.namespace))
}

async fn clear(target: &Target, name: &str) -> anyhow::Result<()> {
    let namespace = &target.namespace;
    let path = format!("/apis/batch/v1/namespaces/{namespace}/jobs/{name}");
    loop {
        let Some(job) =
            kubectl::get::<Value>(&target.kubeconfig, &target.context, namespace, "job", name)
                .await?
        else {
            return Ok(());
        };
        if matches!(state(&job), State::Succeeded | State::Failed)
            && job["metadata"]["deletionTimestamp"].is_null()
        {
            let uid = job["metadata"]["uid"]
                .as_str()
                .context("Job UID is absent")?;
            // A concurrent caller can already have replaced this Job by name.
            let options = serde_json::to_vec(&json!({
                "apiVersion": "v1",
                "kind": "DeleteOptions",
                "propagationPolicy": "Foreground",
                "preconditions": {"uid": uid},
            }))?;
            match kubectl::kubectl(
                &target.kubeconfig,
                &target.context,
                namespace,
                &["delete", "--raw", &path, "-f", "-"],
                Some(&options),
            )
            .await
            {
                Ok(_) => {}
                Err(error)
                    if error.is::<kubectl::Conflict>() || error.is::<kubectl::NotFound>() => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("remove Job {namespace}/{name}"));
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Run one Job under its fixed name and return the UID of the completed run.
pub async fn run_exclusive(
    target: &Target,
    manifest: &Value,
    timeout: Duration,
) -> anyhow::Result<String> {
    let name = name(manifest);
    let namespace = &target.namespace;
    let input = serde_json::to_vec(manifest)?;
    let created = tokio::time::timeout(timeout, async {
        loop {
            clear(target, name).await?;
            match kubectl::kubectl(
                &target.kubeconfig,
                &target.context,
                namespace,
                &["create", "-f", "-", "-o", "json"],
                Some(&input),
            )
            .await
            {
                Ok(output) => return Ok(serde_json::from_str::<Value>(&output)?),
                Err(error) if error.is::<kubectl::Conflict>() => {}
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .map_err(anyhow::Error::from)
    .flatten()
    .with_context(|| format!("start Job {namespace}/{name}"))?;
    let uid = created["metadata"]["uid"]
        .as_str()
        .context("Job UID is absent")?;
    let version = created["metadata"]["resourceVersion"]
        .as_str()
        .context("Job resource version is absent")?;
    wait_instance(target, name, uid, version, timeout).await?;
    Ok(uid.to_owned())
}

async fn wait_instance(
    target: &Target,
    name: &str,
    uid: &str,
    version: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    let namespace = &target.namespace;
    let deadline = tokio::time::Instant::now() + timeout;
    let path =
        format!("/apis/batch/v1/namespaces/{namespace}/jobs?fieldSelector=metadata.name%3D{name}");
    let mut version = version.to_owned();
    let inspect = |job: &Value| match state(job) {
        State::Succeeded => Ok(true),
        State::Failed => anyhow::bail!("Job {namespace}/{name} failed"),
        State::Pending | State::Running => Ok(false),
    };
    loop {
        let output = kubectl::with_timeout(
            deadline,
            &target.kubeconfig,
            &target.context,
            namespace,
            &["get", "--raw", &path],
        )
        .await
        .with_context(|| format!("read Job {namespace}/{name}"))?;
        let jobs: Value = serde_json::from_str(&output).context("decode Job list")?;
        let job = &jobs["items"][0];
        if job["metadata"]["uid"] != uid {
            break;
        }
        if inspect(job)? {
            return Ok(());
        }
        jobs["metadata"]["resourceVersion"]
            .as_str()
            .context("Job list resource version is absent")?
            .clone_into(&mut version);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // A concurrent caller may already have removed the finished Job. Replay only
    // the events since our last observation to recover this run's terminal state.
    ensure!(
        !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid resource version for Job {namespace}/{name}"
    );
    let watch = format!("{path}&watch=true&resourceVersion={version}&timeoutSeconds=1");
    let events = kubectl::with_timeout(
        deadline,
        &target.kubeconfig,
        &target.context,
        namespace,
        &["get", "--raw", &watch],
    )
    .await
    .with_context(|| format!("watch Job {namespace}/{name}"))?;
    for event in serde_json::Deserializer::from_str(&events).into_iter::<Value>() {
        let event = event.context("decode Job watch event")?;
        let job = &event["object"];
        ensure!(
            event["type"] != "ERROR",
            "watch Job {namespace}/{name}: {job}"
        );
        if job["metadata"]["uid"] == uid && inspect(job)? {
            return Ok(());
        }
    }
    anyhow::bail!("Job {namespace}/{name} disappeared before completion could be verified")
}

/// Installed worker image and its public component configuration.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    pub image: String,
    pub configuration: String,
    pub service_account: String,
}

/// Submit one bounded operation Job. The caller has already authorized its target.
pub async fn submit(
    api: &crate::kubernetes_api::KubernetesApi,
    job: &mut Value,
    request: &str,
) -> anyhow::Result<Value> {
    let input = format!("{}-input", name(job));
    job["spec"]["suspend"] = false.into();
    job["spec"]["template"]["spec"]["volumes"]
        .as_array_mut()
        .expect("Job volumes")
        .push(json!({"name":"request","configMap":{"name":input}}));
    container(job)["volumeMounts"]
        .as_array_mut()
        .expect("operation volume mounts")
        .push(json!({"name":"request","mountPath":"/etc/wamn/request","readOnly":true}));
    // The pod waits for the volume while the Job deadline runs. The Job's UID
    // is available for the owner reference before we create the input.
    let created = api.create_job(&api.namespace, job).await?;
    ensure!(
        created["metadata"]["name"] == job["metadata"]["name"],
        "Kubernetes returned a different Job"
    );
    let uid = created["metadata"]["uid"]
        .as_str()
        .context("Job UID is absent")?;
    api.create_operation_input(
        &api.namespace,
        &json!({
            "apiVersion":"v1","kind":"ConfigMap","immutable":true,
            "metadata":{"name":input,"ownerReferences":[{
                "apiVersion":"batch/v1","kind":"Job","name":name(job),"uid":uid
            }]},
            "data":{"request.json":request}
        }),
    )
    .await
    .with_context(|| format!("create input for Job {}", name(job)))?;
    Ok(json!({"job":name(job)}))
}

/// Read the Job that controls the current worker Pod.
pub async fn current(api: &crate::kubernetes_api::KubernetesApi) -> anyhow::Result<Value> {
    // Operation Pods use the default hostname, which Kubernetes sets to the Pod name.
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .context("read operation Pod hostname")?;
    let pod = hostname.trim();
    ensure!(kubernetes_name(pod), "invalid operation Pod name");
    let namespace = &api.namespace;
    let pod = api
        .get(format!("/api/v1/namespaces/{namespace}/pods/{pod}"))
        .await?;
    let owner = pod["metadata"]["ownerReferences"]
        .as_array()
        .and_then(|owners| {
            owners.iter().find(|owner| {
                owner["controller"] == true
                    && owner["kind"] == "Job"
                    && owner["apiVersion"] == "batch/v1"
            })
        })
        .context("operation Pod has no controlling Job")?;
    let name = owner["name"]
        .as_str()
        .filter(|name| kubernetes_name(name))
        .context("invalid operation Job name")?;
    let job = api
        .get_job(namespace, name)
        .await?
        .context("operation Job is absent")?;
    ensure!(
        owner["uid"]
            .as_str()
            .is_some_and(|uid| job["metadata"]["uid"] == uid),
        "operation Job {namespace}/{name} UID does not match its Pod's owner"
    );
    Ok(job)
}

/// A refused observation never reveals a Job outside the authorized target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationErrorType {
    InvalidName,
    NotFound,
}

#[derive(Debug)]
pub struct ObservationError {
    pub type_: ObservationErrorType,
}
impl std::fmt::Display for ObservationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "operation observation refused: {:?}", self.type_)
    }
}
impl std::error::Error for ObservationError {}

/// Observe only Jobs that belong to the authorized environment and operation set.
pub async fn observe(
    api: &crate::kubernetes_api::KubernetesApi,
    name: &str,
    target: &str,
    operations: &[&str],
) -> anyhow::Result<Value> {
    let namespace = &api.namespace;
    if !kubernetes_name(name)
        || name
            .strip_prefix("op-")
            .is_none_or(|id| uuid::Uuid::parse_str(id).is_err())
    {
        return Err(ObservationError {
            type_: ObservationErrorType::InvalidName,
        }
        .into());
    }
    let job = api
        .get_job(&api.namespace, name)
        .await
        .map_err(anyhow::Error::from)?
        .ok_or(ObservationError {
            type_: ObservationErrorType::NotFound,
        })?;
    if job["metadata"]["annotations"]["wamn.dev/operation-target"] != target
        || job["metadata"]["annotations"]["wamn.dev/operation"]
            .as_str()
            .is_none_or(|operation| !operations.contains(&operation))
    {
        return Err(ObservationError {
            type_: ObservationErrorType::NotFound,
        }
        .into());
    }
    let uid = job["metadata"]["uid"]
        .as_str()
        .context("Job UID is absent")?;
    let uid = uuid::Uuid::parse_str(uid).context("Job UID is invalid")?;
    let pods = api.get(format!("/api/v1/namespaces/{namespace}/pods?labelSelector=batch.kubernetes.io%2Fcontroller-uid%3D{uid}&limit=1")).await.map_err(anyhow::Error::from)?;
    let mut log = String::new();
    if let Some(pod) = pods["items"].as_array().and_then(|items| items.first()) {
        let owned = pod["metadata"]["ownerReferences"]
            .as_array()
            .is_some_and(|owners| {
                owners
                    .iter()
                    .any(|owner| owner["uid"] == uid.to_string() && owner["kind"] == "Job")
            });
        if owned
            && pod["status"]["containerStatuses"]
                .as_array()
                .is_some_and(|states| {
                    states.iter().any(|state| {
                        state["name"] == job["spec"]["template"]["spec"]["containers"][0]["name"]
                            && state["state"].get("waiting").is_none()
                    })
                })
        {
            let pod = pod["metadata"]["name"]
                .as_str()
                .filter(|name| kubernetes_name(name))
                .context("pod name is invalid")?;
            log = api
                .pod_log(&api.namespace, pod)
                .await
                .map_err(anyhow::Error::from)?;
        }
    }
    let state = state(&job);
    Ok(json!({"job":name,"state":state,"log":log}))
}

fn kubernetes_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
}

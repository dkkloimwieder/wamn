//! Bounded platform Job execution and observation.
use anyhow::{Context as _, ensure};
use serde::Deserialize as _;
use serde_json::{Value, json};
use std::time::Duration;

use crate::{
    Target,
    client::{KubernetesApi, REQUEST_TIMEOUT, status_matches, wait_named},
};
use futures_util::{AsyncReadExt as _, TryStreamExt as _};
use k8s_openapi::api::{
    batch::v1::Job,
    core::v1::{ConfigMap, Pod},
};
use kube::{
    Api,
    api::{
        DeleteParams, ListParams, LogParams, PostParams, Preconditions, PropagationPolicy,
        WatchEvent, WatchParams,
    },
};

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

fn state(job: &Job) -> State {
    let Some(status) = &job.status else {
        return State::Pending;
    };
    let has = |type_: &str| {
        status.conditions.as_ref().is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == type_ && condition.status == "True")
        })
    };
    if has("Failed") {
        State::Failed
    } else if has("Complete") {
        State::Succeeded
    } else if status.active.unwrap_or(0) > 0 {
        State::Running
    } else {
        State::Pending
    }
}

/// Wait for a named Job to reach a terminal condition within the caller's deadline.
pub async fn wait(jobs: &Api<Job>, name: &str, timeout: Duration) -> anyhow::Result<()> {
    let result = wait_named(jobs, name, timeout, |job| {
        Ok(match job.as_ref().map(state) {
            Some(result @ (State::Succeeded | State::Failed)) => Some(result),
            _ => None,
        })
    })
    .await?;
    ensure!(
        result == State::Succeeded,
        "Job {}/{name} failed",
        jobs.resource_url()
    );
    Ok(())
}

/// Wait for the previous Job to terminate, then delete it and wait for the name to disappear.
pub async fn remove_previous(jobs: &Api<Job>, name: &str, timeout: Duration) -> anyhow::Result<()> {
    tokio::time::timeout(timeout, clear(jobs, name))
        .await
        .map_err(anyhow::Error::from)
        .flatten()
        .with_context(|| format!("wait to replace Job {}/{name}", jobs.resource_url()))
}

async fn clear(jobs: &Api<Job>, name: &str) -> anyhow::Result<()> {
    loop {
        let Some(job) = jobs.get_opt(name).await? else {
            return Ok(());
        };
        if matches!(state(&job), State::Succeeded | State::Failed)
            && job.metadata.deletion_timestamp.is_none()
        {
            let uid = job.metadata.uid.context("Job UID is absent")?;
            let options = DeleteParams {
                propagation_policy: Some(PropagationPolicy::Foreground),
                preconditions: Some(Preconditions {
                    uid: Some(uid),
                    resource_version: None,
                }),
                ..DeleteParams::default()
            };
            match jobs.delete(name, &options).await {
                Ok(_) => {}
                Err(error)
                    if status_matches(&error, 409, "Conflict")
                        || status_matches(&error, 404, "NotFound") => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("remove Job {}/{name}", jobs.resource_url()));
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
    let jobs = Api::<Job>::namespaced(target.client.client(), namespace);
    let job = Job::deserialize(manifest).context("decode Job manifest")?;
    let created = tokio::time::timeout(timeout, async {
        loop {
            clear(&jobs, name).await?;
            match jobs.create(&PostParams::default(), &job).await {
                Ok(job) => return Ok(job),
                Err(error) if status_matches(&error, 409, "AlreadyExists") => {}
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await
    .map_err(anyhow::Error::from)
    .flatten()
    .with_context(|| format!("start Job {namespace}/{name}"))?;
    let uid = created.metadata.uid.context("Job UID is absent")?;
    let version = created
        .metadata
        .resource_version
        .context("Job resource version is absent")?;
    tokio::time::timeout(timeout, wait_instance(&jobs, name, &uid, version))
        .await
        .with_context(|| format!("wait for Job {namespace}/{name}"))??;
    Ok(uid)
}

async fn wait_instance(
    jobs: &Api<Job>,
    name: &str,
    uid: &str,
    mut version: String,
) -> anyhow::Result<()> {
    let path = jobs.resource_url();
    let selector = format!("metadata.name={name}");
    let list = ListParams::default().fields(&selector);
    let inspect = |job: &Job| match state(job) {
        State::Succeeded => Ok(true),
        State::Failed => anyhow::bail!("Job {path}/{name} failed"),
        State::Pending | State::Running => Ok(false),
    };
    loop {
        let found = jobs
            .list(&list)
            .await
            .with_context(|| format!("read Job {path}/{name}"))?;
        let Some(job) = found
            .items
            .first()
            .filter(|job| job.metadata.uid.as_deref() == Some(uid))
        else {
            break;
        };
        if inspect(job)? {
            return Ok(());
        }
        version = found
            .metadata
            .resource_version
            .context("Job list resource version is absent")?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // Replay events for the previous UID if another caller already replaced the Job.
    ensure!(
        !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid resource version for Job {path}/{name}"
    );
    let watch = WatchParams::default()
        .fields(&selector)
        .timeout(1)
        .disable_bookmarks();
    let events = jobs
        .watch(&watch, &version)
        .await
        .with_context(|| format!("watch Job {path}/{name}"))?;
    let mut events = std::pin::pin!(events);
    while let Some(event) = events
        .try_next()
        .await
        .with_context(|| format!("watch Job {path}/{name}"))?
    {
        match event {
            WatchEvent::Added(job) | WatchEvent::Modified(job) | WatchEvent::Deleted(job) => {
                if job.metadata.uid.as_deref() == Some(uid) && inspect(&job)? {
                    return Ok(());
                }
            }
            WatchEvent::Error(status) => {
                return Err(kube::Error::Api(status.boxed()))
                    .with_context(|| format!("watch Job {path}/{name}"));
            }
            WatchEvent::Bookmark(_) => {}
        }
    }
    anyhow::bail!("Job {path}/{name} disappeared before completion could be verified")
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
pub async fn submit(api: &KubernetesApi, job: &mut Value, request: &str) -> anyhow::Result<Value> {
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
    let jobs = Api::<Job>::namespaced(api.client(), &api.namespace);
    let manifest = Job::deserialize(&*job).context("decode operation Job")?;
    let name = manifest
        .metadata
        .name
        .as_deref()
        .context("operation Job name is absent")?;
    let created = tokio::time::timeout(
        REQUEST_TIMEOUT,
        jobs.create(&PostParams::default(), &manifest),
    )
    .await
    .with_context(|| format!("create Job {name}"))??;
    ensure!(
        created.metadata.name.as_deref() == Some(name),
        "Kubernetes returned a different Job"
    );
    let uid = created.metadata.uid.context("Job UID is absent")?;
    let inputs = Api::<ConfigMap>::namespaced(api.client(), &api.namespace);
    let manifest = serde_json::from_value::<ConfigMap>(json!({
        "metadata":{"name":input,"ownerReferences":[{
            "apiVersion":"batch/v1","kind":"Job","name":name,"uid":uid
        }]},
        "immutable":true,"data":{"request.json":request}
    }))?;
    tokio::time::timeout(
        REQUEST_TIMEOUT,
        inputs.create(&PostParams::default(), &manifest),
    )
    .await
    .map_err(anyhow::Error::from)
    .and_then(|result| result.map_err(anyhow::Error::from))
    .with_context(|| format!("create input for Job {name}"))?;
    Ok(json!({"job":name}))
}

/// Read the Job that controls the current worker Pod.
pub async fn current(api: &KubernetesApi) -> anyhow::Result<Value> {
    // Operation Pods use the default hostname, which Kubernetes sets to the Pod name.
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .context("read operation Pod hostname")?;
    let pod = hostname.trim();
    ensure!(kubernetes_name(pod), "invalid operation Pod name");
    let namespace = &api.namespace;
    let pods = Api::<Pod>::namespaced(api.client(), namespace);
    let pod = tokio::time::timeout(REQUEST_TIMEOUT, pods.get(pod)).await??;
    let owner = pod
        .metadata
        .owner_references
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|owner| {
            owner.controller == Some(true) && owner.api_version == "batch/v1" && owner.kind == "Job"
        })
        .context("operation Pod has no controlling Job")?;
    let name = &owner.name;
    ensure!(kubernetes_name(name), "invalid operation Job name");
    let jobs = Api::<Job>::namespaced(api.client(), namespace);
    let job = tokio::time::timeout(REQUEST_TIMEOUT, jobs.get_opt(name))
        .await??
        .context("operation Job is absent")?;
    ensure!(
        job.metadata.uid.as_deref() == Some(&owner.uid),
        "operation Job {namespace}/{name} UID does not match its Pod's owner"
    );
    Ok(serde_json::to_value(job)?)
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
    api: &KubernetesApi,
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
    let jobs = Api::<Job>::namespaced(api.client(), namespace);
    let job = tokio::time::timeout(REQUEST_TIMEOUT, jobs.get_opt(name))
        .await??
        .ok_or(ObservationError {
            type_: ObservationErrorType::NotFound,
        })?;
    let annotations = job.metadata.annotations.as_ref();
    let annotation = |key| {
        annotations
            .and_then(|values| values.get(key))
            .map(String::as_str)
    };
    if annotation("wamn.dev/operation-target") != Some(target)
        || annotation("wamn.dev/operation").is_none_or(|operation| !operations.contains(&operation))
    {
        return Err(ObservationError {
            type_: ObservationErrorType::NotFound,
        }
        .into());
    }
    let uid = job.metadata.uid.as_deref().context("Job UID is absent")?;
    let uid = uuid::Uuid::parse_str(uid)
        .context("Job UID is invalid")?
        .to_string();
    let pods = Api::<Pod>::namespaced(api.client(), namespace);
    let params = ListParams::default()
        .labels(&format!("batch.kubernetes.io/controller-uid={uid}"))
        .limit(1);
    let result = tokio::time::timeout(REQUEST_TIMEOUT, pods.list(&params)).await??;
    let mut log = String::new();
    if let Some(pod) = result.items.first() {
        let owned = pod
            .metadata
            .owner_references
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|owner| owner.uid == uid && owner.kind == "Job");
        let container = job
            .spec
            .as_ref()
            .and_then(|spec| spec.template.spec.as_ref())
            .and_then(|spec| spec.containers.first())
            .map(|container| container.name.as_str());
        let started = pod
            .status
            .as_ref()
            .and_then(|status| status.container_statuses.as_deref())
            .unwrap_or_default()
            .iter()
            .any(|status| {
                Some(status.name.as_str()) == container
                    && status
                        .state
                        .as_ref()
                        .is_none_or(|state| state.waiting.is_none())
            });
        if owned && started {
            let pod = pod
                .metadata
                .name
                .as_deref()
                .filter(|name| kubernetes_name(name))
                .context("pod name is invalid")?;
            log = tokio::time::timeout(REQUEST_TIMEOUT, pod_log(&pods, pod)).await??;
        }
    }
    Ok(json!({"job":name,"state":state(&job),"log":log}))
}

async fn pod_log(pods: &Api<Pod>, name: &str) -> anyhow::Result<String> {
    const LIMIT: usize = 32 * 1024;
    let params = LogParams {
        container: Some("operation".into()),
        tail_lines: Some(100),
        limit_bytes: Some(LIMIT as i64),
        ..LogParams::default()
    };
    let mut log = Vec::new();
    pods.log_stream(name, &params)
        .await?
        .take(LIMIT as u64)
        .read_to_end(&mut log)
        .await?;
    let mut text = String::from_utf8_lossy(&log).into_owned();
    while text.len() > LIMIT {
        text.pop();
    }
    Ok(text)
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

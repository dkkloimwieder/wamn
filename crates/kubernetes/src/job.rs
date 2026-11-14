//! Bounded platform Job execution and observation.
use crate::{Target, kubectl};
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

// Bound execution even if the caller exits. Kubernetes never retries these Jobs.
const INSTALL_DEADLINE: Duration = Duration::from_mins(15);

#[derive(Debug)]
struct JobError {
    name: String,
}
impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "platform Job {} failed", self.name)
    }
}
impl std::error::Error for JobError {}

/// A temporary Kubernetes Secret. Debug output excludes its values.
pub struct Secret(pub Value);
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secret")
            .field("name", &self.0["metadata"]["name"])
            .finish_non_exhaustive()
    }
}

pub fn manifest(name: &str, image: &str, command: &[&str], arguments: &[&str]) -> Value {
    json!({"apiVersion":"batch/v1", "kind":"Job", "metadata":{"name":name},
        "spec":{"suspend":true, "backoffLimit":0, "activeDeadlineSeconds":INSTALL_DEADLINE.as_secs(),
            "template":{"spec":{"restartPolicy":"Never", "automountServiceAccountToken":false,
                "containers":[{"name":"operation", "image":image, "command":command, "args":arguments, "env":[], "volumeMounts":[]}],
                "volumes":[]}}}})
}

pub fn name(job: &Value) -> &str {
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

pub fn credential(
    job: &mut Value,
    name: &str,
    environment: &str,
    key: &str,
    value: &str,
) -> Secret {
    container(job)["env"].as_array_mut().unwrap().push(json!({
        "name":environment, "valueFrom":{"secretKeyRef":{"name":name,"key":key}}
    }));
    Secret(
        json!({"apiVersion":"v1", "kind":"Secret", "metadata":{"name":name},
        "type":"Opaque", "stringData":{key:value}}),
    )
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

pub fn files(job: &mut Value, files: BTreeMap<String, String>) -> Secret {
    let secret = format!("{}-files", name(job));
    mount(
        job,
        "inputs",
        &secret,
        "/inputs",
        &files.keys().map(String::as_str).collect::<Vec<_>>(),
    );
    let mut object =
        json!({"apiVersion":"v1", "kind":"Secret", "metadata":{"name":secret}, "type":"Opaque"});
    object["stringData"] = Value::Object(
        files
            .into_iter()
            .map(|(key, value)| (key, Value::String(value)))
            .collect(),
    );
    Secret(object)
}

fn owner_reference(metadata: &Value) -> anyhow::Result<Value> {
    let uid = metadata["uid"].as_str().context("created Job has no uid")?;
    Ok(json!([{"apiVersion":"batch/v1", "kind":"Job", "name":metadata["name"], "uid":uid}]))
}

/// Run a Job and return its public output after cleanup.
///
/// Existing Jobs are collected before a new request runs.
///
/// # Errors
/// Returns execution, log, or cleanup failures.
pub async fn run(
    target: &Target,
    directory: &Path,
    job: &mut Value,
    secrets: &mut [Secret],
) -> anyhow::Result<String> {
    let existing: Option<Value> = kubectl::get(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        "job",
        name(job),
    )
    .await?;
    if let Some(existing) = existing {
        // A suspended Job never started. Its support objects can be incomplete.
        if existing["spec"]["suspend"] == true {
            cleanup(target, &existing).await?;
        } else {
            collect(target, directory, &existing).await?;
        }
    }
    job["metadata"]["namespace"] = target.namespace.clone().into();
    let input = serde_json::to_vec(job)?;
    let created = kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &["create", "-f", "-", "-o", "json"],
        Some(&input),
    )
    .await?;
    let created: Value = serde_json::from_str(&created).context("decode the created Job")?;
    let prepared = async {
        for secret in secrets {
            let object = &mut secret.0;
            object["metadata"]["namespace"] = target.namespace.clone().into();
            object["metadata"]["ownerReferences"] = owner_reference(&created["metadata"])?;
            kubectl::apply(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                object,
            )
            .await?;
        }
        anyhow::Ok(())
    }
    .await;
    if let Err(error) = prepared {
        return match cleanup(target, job).await {
            Ok(()) => Err(error),
            Err(cleanup) => Err(error.context(format!("Job cleanup failed: {cleanup:#}"))),
        };
    }
    kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &[
            "patch",
            "job",
            name(job),
            "--type=merge",
            "--patch",
            r#"{"spec":{"suspend":false}}"#,
        ],
        None,
    )
    .await?;
    collect(target, directory, job).await
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

fn completed(job: &Value, name: &str) -> Result<bool, JobError> {
    match state(job) {
        State::Failed => Err(JobError { name: name.into() }),
        State::Succeeded => Ok(true),
        State::Pending | State::Running => Ok(false),
    }
}

async fn wait(target: &Target, job: &Value) -> anyhow::Result<()> {
    let deadline = Instant::now() + INSTALL_DEADLINE;
    loop {
        let status: Value = match kubectl::get(
            &target.kubeconfig,
            &target.context,
            &target.namespace,
            "job",
            name(job),
        )
        .await
        {
            Err(error) if error.is::<kubectl::Unreachable>() && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            status => status?.context("platform Job disappeared")?,
        };
        if completed(&status, name(job))? {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "platform Job {} did not complete in {}s",
            name(job),
            INSTALL_DEADLINE.as_secs()
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn collect(target: &Target, directory: &Path, job: &Value) -> anyhow::Result<String> {
    let result = wait(target, job).await;
    let output = logs(target, directory, job).await;
    // An API failure says nothing about execution. Preserve the Job
    // and its owned credentials so the next invocation can recover its result.
    let cleanup = if result.is_ok() || result.as_ref().is_err_and(anyhow::Error::is::<JobError>) {
        cleanup(target, job).await
    } else {
        Ok(())
    };
    result.with_context(|| {
        let path = directory.join("jobs").join(format!("{}.log", name(job)));
        let log = std::fs::read_to_string(&path).unwrap_or_default();
        let cleanup_error = cleanup
            .as_ref()
            .err()
            .map(|error| format!("\nJob cleanup failed: {error:#}"))
            .unwrap_or_default();
        format!(
            "platform Job log saved at {}:\n{log}{cleanup_error}",
            path.display()
        )
    })?;
    cleanup?;
    output
}

async fn logs(target: &Target, directory: &Path, job: &Value) -> anyhow::Result<String> {
    let name = name(job);
    let log = kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &[
            "logs",
            &format!("job/{name}"),
            "--all-containers=true",
            "--pod-running-timeout=10s",
        ],
        None,
    )
    .await;
    let directory = directory.join("jobs");
    std::fs::create_dir_all(&directory).context("create the Job log directory")?;
    let saved = std::fs::write(
        directory.join(format!("{name}.log")),
        match &log {
            Ok(log) => std::borrow::Cow::Borrowed(log.as_str()),
            Err(error) => std::borrow::Cow::Owned(format!("Job log unavailable: {error:#}\n")),
        }
        .as_bytes(),
    )
    .context("save the Job log");
    saved?;
    log.context("read the public Job result")
}

async fn cleanup(target: &Target, job: &Value) -> anyhow::Result<()> {
    let name = name(job);
    // After its log is saved, even a failed Job must release its credentials.
    let deleted = kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &[
            "delete",
            "job",
            name,
            "--ignore-not-found",
            "--cascade=foreground",
            "--wait=true",
            "--timeout=120s",
        ],
        None,
    )
    .await;
    deleted?;
    Ok(())
}

/// Installed worker image and its public component configuration.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    pub image: String,
    pub configuration: String,
    pub service_account: String,
    #[serde(skip)]
    pub database_credential: String,
    #[serde(skip)]
    pub administration: std::path::PathBuf,
}

/// Submit one bounded operation Job. The caller has already authorized its target.
pub async fn submit(
    api: &crate::kubernetes_api::KubernetesApi,
    job: &Value,
) -> anyhow::Result<Value> {
    let created = api.create_job(&api.namespace, job).await?;
    ensure!(
        created["metadata"]["name"] == job["metadata"]["name"],
        "Kubernetes returned a different Job"
    );
    Ok(json!({"job":name(job)}))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_ownership_refuses_a_job_without_a_uid() {
        assert!(owner_reference(&json!({"name":"platform-system"})).is_err());
    }
}

//! Bounded platform Job execution and observation.
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use std::time::Duration;

// Bound execution even if the caller exits. Kubernetes never retries these Jobs.
const INSTALL_DEADLINE: Duration = Duration::from_mins(15);

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

/// Mount the installed OCI client configuration for the selected credential source.
pub fn registry(job: &mut Value, metadata: bool) {
    if !metadata {
        job["spec"]["template"]["spec"]["imagePullSecrets"] =
            json!([{"name":wamn_registry::name::REGISTRY_PULL}]);
    }
    let source = if metadata {
        json!({"configMap":{"name":"wamn-registry-helper"}})
    } else {
        json!({"secret":{"secretName":wamn_registry::name::REGISTRY_PULL,"defaultMode":256,
            "items":[{"key":".dockerconfigjson","path":"config.json"}]}})
    };
    let mut volume = source;
    volume["name"] = "registry".into();
    job["spec"]["template"]["spec"]["volumes"]
        .as_array_mut()
        .unwrap()
        .push(volume);
    container(job)["volumeMounts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"registry","mountPath":"/credentials/registry","readOnly":true}));
    environment(job, &[("DOCKER_CONFIG", "/credentials/registry")]);
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

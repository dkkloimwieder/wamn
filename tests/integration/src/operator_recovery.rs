//! Generic Kubernetes operator recovery predicates and retained native-object tests.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

/// Epoch microseconds as epoch seconds. `Duration` carries the conversion, so
/// the width change is not an `as` cast; a pre-epoch value reports zero.
pub fn epoch_seconds(micros: i64) -> f64 {
    Duration::from_micros(u64::try_from(micros).unwrap_or(0)).as_secs_f64()
}
pub fn now() -> f64 {
    epoch_seconds(chrono::Utc::now().timestamp_micros())
}

/// Report whether any contextual source in an error is a refused connection.
#[must_use]
pub fn connection_refused(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused)
    })
}

/// Require the measured host container to remain in the selected pod and reach
/// the exact ready restart generation.
pub fn assert_host_restart(pod: &Value, uid: Option<&str>, restarts: u64) -> anyhow::Result<()> {
    ensure!(
        uid.is_none_or(|uid| pod["metadata"]["uid"] == uid),
        "host restart replaced the measured pod"
    );
    ensure!(
        pod["status"]["containerStatuses"]
            .as_array()
            .is_some_and(|statuses| statuses.iter().any(|status| {
                status["name"] == "host"
                    && status["ready"] == true
                    && status["restartCount"].as_u64() == Some(restarts)
            })),
        "measured host must be ready with the exact restart count"
    );
    Ok(())
}

pub fn timestamp(value: &str) -> anyhow::Result<f64> {
    Ok(epoch_seconds(
        chrono::DateTime::parse_from_rfc3339(value)?.timestamp_micros(),
    ))
}
pub fn text<'a>(value: &'a Value, path: &str) -> anyhow::Result<&'a str> {
    value
        .pointer(path)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("the native object has text at {path}"))
}
pub fn array<'a>(value: &'a Value, path: &str) -> anyhow::Result<&'a Vec<Value>> {
    value
        .pointer(path)
        .and_then(Value::as_array)
        .with_context(|| format!("the native object has an array at {path}"))
}
pub fn ready(value: &Value) -> bool {
    value
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition["type"] == "Ready" && condition["status"] == "True")
        })
}
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), canonical(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        value => value.clone(),
    }
}
pub fn digest(value: &Value) -> anyhow::Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(
        &canonical(value),
    )?)))
}
pub fn normalized_crd(spec: &Value) -> anyhow::Result<Value> {
    let mut value = spec.clone();
    let fields = value
        .as_object_mut()
        .context("the CRD specification is an object")?;
    fields
        .entry("conversion")
        .or_insert_with(|| json!({"strategy":"None"}));
    fields
        .entry("preserveUnknownFields")
        .or_insert(Value::Bool(false));
    Ok(value)
}
pub fn host_ids(items: &[Value]) -> anyhow::Result<Vec<(String, String)>> {
    let mut ids = items
        .iter()
        .map(|host| {
            Ok((
                text(host, "/metadata/uid")?.to_owned(),
                text(host, "/hostId")?.to_owned(),
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    ids.sort();
    Ok(ids)
}
pub fn pod_ids(items: &[Value], name: &str) -> anyhow::Result<Vec<(String, String, u64, String)>> {
    let mut ids = Vec::new();
    for pod in items {
        ensure!(
            pod["metadata"]["deletionTimestamp"].is_null(),
            "the observed process is not terminating"
        );
        let statuses = array(pod, "/status/containerStatuses")?
            .iter()
            .filter(|status| status["name"] == name)
            .collect::<Vec<_>>();
        ensure!(
            statuses.len() == 1 && statuses[0]["ready"] == true,
            "the observed process has one ready container"
        );
        let status = statuses[0];
        ids.push((
            text(pod, "/metadata/uid")?.to_owned(),
            text(status, "/containerID")?.to_owned(),
            status["restartCount"]
                .as_u64()
                .context("the process restart count is an integer")?,
            text(status, "/imageID")?.to_owned(),
        ));
    }
    ids.sort();
    Ok(ids)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperatorState {
    pub pod_name: String,
    pub pod_uid: String,
    pub container_id: Option<String>,
    pub restart_count: u64,
    pub image_id: Option<String>,
    pub ready: bool,
}
pub fn operator_state(items: &[Value]) -> anyhow::Result<OperatorState> {
    ensure!(items.len() == 1, "one supervised operator pod is required");
    let pod = &items[0];
    ensure!(
        pod["metadata"]["deletionTimestamp"].is_null(),
        "the operator pod is not terminating"
    );
    let statuses = array(pod, "/status/containerStatuses")?
        .iter()
        .filter(|status| status["name"] == "runtime-operator")
        .collect::<Vec<_>>();
    ensure!(statuses.len() == 1, "the operator has one container status");
    let status = statuses[0];
    Ok(OperatorState {
        pod_name: text(pod, "/metadata/name")?.to_owned(),
        pod_uid: text(pod, "/metadata/uid")?.to_owned(),
        container_id: status["containerID"].as_str().map(str::to_owned),
        restart_count: status["restartCount"]
            .as_u64()
            .context("the operator restart count is an integer")?,
        image_id: status["imageID"].as_str().map(str::to_owned),
        ready: status["ready"].as_bool().unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_refused_connections_are_retryable() {
        let refused =
            anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::ConnectionRefused))
                .context("connect to the allocated NodePort");
        assert!(connection_refused(&refused));
        let denied = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert!(!connection_refused(&denied));
    }

    #[test]
    fn a_replaced_or_unready_measured_host_does_not_pass() {
        let mut pod = json!({"metadata":{"uid":"same"},"status":{"containerStatuses":[{"name":"host","ready":true,"restartCount":1}]}});
        assert!(assert_host_restart(&pod, Some("same"), 1).is_ok());
        assert!(assert_host_restart(&pod, Some("different"), 1).is_err());
        pod["status"]["containerStatuses"][0]["restartCount"] = json!(2);
        assert!(assert_host_restart(&pod, Some("same"), 1).is_err());
        pod["status"]["containerStatuses"][0]["restartCount"] = json!(1);
        pod["status"]["containerStatuses"][0]["ready"] = json!(false);
        assert!(assert_host_restart(&pod, Some("same"), 1).is_err());
    }
}

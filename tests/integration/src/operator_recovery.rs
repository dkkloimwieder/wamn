//! Generic Kubernetes operator recovery predicates and retained native-object tests.

use std::collections::BTreeMap;
use std::time::Duration;

#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::{Path, PathBuf};

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
    pub termination: Option<Value>,
    pub state: Value,
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
        termination: status.pointer("/lastState/terminated").cloned(),
        state: status["state"].clone(),
    })
}
pub fn supervised_restart(
    previous: &OperatorState,
    current: &OperatorState,
    previous_log: &str,
    events: &Value,
    since: f64,
    scheduler_address: &str,
) -> anyhow::Result<Value> {
    ensure!(
        current.pod_uid == previous.pod_uid && current.image_id == previous.image_id,
        "the operator pod and image must stay unchanged during the scheduler fault"
    );
    ensure!(
        current.restart_count == previous.restart_count + 1,
        "the observed operator restart history has a gap"
    );
    ensure!(
        current
            .container_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
            && current.container_id != previous.container_id,
        "the operator restart must identify a new container"
    );
    let termination = current
        .termination
        .as_ref()
        .context("the previous operator termination is recorded")?;
    ensure!(
        termination["containerID"].as_str() == previous.container_id.as_deref(),
        "the previous termination must identify the observed operator process"
    );
    if termination["exitCode"] == 1 && termination["reason"] == "Error" {
        ensure!(
            !previous.ready && !scheduler_address.is_empty(),
            "startup refusal requires an unready operator and the recorded scheduler address"
        );
        // A waiting snapshot retains the stopped container in its last termination.
        let observed_start = previous.state.pointer("/running/startedAt").or_else(|| {
            previous
                .state
                .get("waiting")
                .filter(|waiting| waiting.is_object())
                .and(previous.termination.as_ref())
                .filter(|stopped| {
                    previous.container_id.as_deref().is_some_and(|id| {
                        !id.is_empty() && stopped["containerID"].as_str() == Some(id)
                    })
                })
                .and_then(|stopped| stopped.get("startedAt"))
        });
        ensure!(
            observed_start.is_some_and(|started| started == &termination["startedAt"]),
            "startup refusal must identify the observed container start"
        );
        let started = timestamp(text(termination, "/startedAt")?)?;
        ensure!(
            started >= since,
            "the operator startup refusal predates the scheduler fault"
        );
        let expected = format!("transport error: dial tcp {scheduler_address}: i/o timeout");
        let mut setup_errors = Vec::new();
        for line in previous_log.lines() {
            let fields = line.splitn(5, '\t').collect::<Vec<_>>();
            if fields.len() != 5
                || fields[1..4] != ["ERROR", "setup", "unable to create runtime operator"]
            {
                continue;
            }
            let Some(stamp) = fields[0]
                .split_whitespace()
                .next()
                .and_then(|stamp| timestamp(stamp).ok())
            else {
                continue;
            };
            let Ok(detail) = serde_json::from_str::<Value>(fields[4]) else {
                continue;
            };
            // The kubelet keeps one stopped container. When the crash loop
            // stops the next container before this read, the recorded
            // termination still identifies the observed exit, and the cause
            // comes from a later container of the same pod in the same fault.
            if started <= stamp && detail["error"] == expected {
                setup_errors.push(line);
            }
        }
        ensure!(
            !setup_errors.is_empty(),
            "the operator startup exit lacks its scheduler TCP timeout evidence"
        );
        return Ok(
            json!({"previous":previous,"current":current,"termination":termination,
            "cause":"scheduler-nats-startup-timeout","scheduler_address":scheduler_address,"setup_errors":setup_errors}),
        );
    }
    ensure!(
        termination["exitCode"] == 0 && termination["reason"] == "Completed",
        "the operator restart was not a graceful supervised exit"
    );
    let mut witnesses = Vec::new();
    let mut timeouts = Vec::new();
    for event in events["items"].as_array().into_iter().flatten() {
        let object = &event["involvedObject"];
        let stamp = event
            .pointer("/series/lastObservedTime")
            .filter(|stamp| stamp.as_str().is_some_and(|value| !value.is_empty()))
            .or_else(|| {
                event
                    .get("lastTimestamp")
                    .filter(|stamp| stamp.as_str().is_some_and(|value| !value.is_empty()))
            })
            .or_else(|| {
                event
                    .get("eventTime")
                    .filter(|stamp| stamp.as_str().is_some_and(|value| !value.is_empty()))
            })
            .or_else(|| event.pointer("/metadata/creationTimestamp"));
        let Some(stamp) = stamp.and_then(Value::as_str) else {
            continue;
        };
        let reporter = event["reportingComponent"]
            .as_str()
            .filter(|value| !value.is_empty())
            .or_else(|| event.pointer("/source/component").and_then(Value::as_str));
        if object["uid"] != current.pod_uid
            || reporter != Some("kubelet")
            || timestamp(stamp)? < since
        {
            continue;
        }
        let message = event["message"].as_str().unwrap_or("");
        if event["reason"] == "Killing"
            && message.contains("runtime-operator")
            && message.contains("failed liveness probe")
        {
            witnesses.push(event);
        }
        if event["reason"] == "Unhealthy"
            && object["fieldPath"] == "spec.containers{runtime-operator}"
            && message.starts_with("Liveness probe failed:")
            && message.contains("/healthz")
            && message.contains("context deadline exceeded")
        {
            timeouts.push(event);
        }
    }
    ensure!(
        !witnesses.is_empty(),
        "the operator restart lacks a same-pod kubelet liveness event from this fault"
    );
    let cause = if previous_log.contains("nats connection closed") {
        "terminal-nats-closure-and-kubelet-liveness-restart"
    } else {
        ensure!(
            !timeouts.is_empty(),
            "the operator restart lacks a recorded terminal NATS closure or liveness HTTP timeout"
        );
        "kubelet-liveness-http-timeout"
    };
    Ok(
        json!({"previous":previous,"current":current,"termination":termination,"cause":cause,"events":witnesses,"probe_timeout_events":timeouts}),
    )
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

    fn retained_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/operator-recovery")
    }
    fn read(path: &Path) -> anyhow::Result<Value> {
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    #[test]
    fn retained_operator_transitions_require_the_recorded_cause() -> anyhow::Result<()> {
        let root = retained_root().join("live-receiving-008/journey/operator-recovery");
        let commands = fs::read_to_string(root.join("commands.jsonl"))?
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()?;
        let since = commands
            .iter()
            .find(|command| {
                command["output_stem"]
                    .as_str()
                    .is_some_and(|stem| stem.ends_with("-stop-scheduler"))
            })
            .context("the retained scheduler stop command exists")?["started"]
            .as_f64()
            .context("the fault start is recorded")?;
        for (number, log_name, event_name, cause) in [
            (
                3,
                "0236-operator-transition-3-previous-log.stdout",
                "0237-operator-transition-3-events.stdout",
                "kubelet-liveness-http-timeout",
            ),
            (
                4,
                "0308-operator-transition-4-previous-log.stdout",
                "0309-operator-transition-4-events.stdout",
                "scheduler-nats-startup-timeout",
            ),
        ] {
            let state = read(&root.join(format!("operator-transition-{number}-state.json")))?;
            let previous: OperatorState = serde_json::from_value(state["previous"].clone())?;
            let current: OperatorState = serde_json::from_value(state["current"].clone())?;
            let log = fs::read_to_string(root.join(log_name))?;
            let events = read(&root.join(event_name))?;
            // This is the endpoint in the retained startup error from the same run.
            let address = "10.96.167.147:4222";
            let classified =
                supervised_restart(&previous, &current, &log, &events, since, address)?;
            assert_eq!(classified["cause"], cause);
            let mut skipped = current.clone();
            skipped.restart_count += 1;
            assert!(
                supervised_restart(&previous, &skipped, &log, &events, since, address).is_err()
            );
            if number == 3 {
                assert!(
                    supervised_restart(
                        &previous,
                        &current,
                        &log,
                        &json!({"items":[]}),
                        since,
                        address
                    )
                    .is_err()
                );
            } else {
                assert!(
                    supervised_restart(&previous, &current, &log, &events, since, "127.0.0.1:4222")
                        .is_err()
                );
                assert!(
                    supervised_restart(&previous, &current, "", &events, since, address).is_err()
                );
            }
        }
        println!(
            "OPERATOR_TRANSITION_REPLAY liveness_timeout=1 startup_scheduler_timeout=1 missing_or_foreign_evidence=refused"
        );
        Ok(())
    }

    #[test]
    fn startup_refusal_retains_container_identity_during_restart_backoff() -> anyhow::Result<()> {
        let stopped = json!({
            "containerID":"containerd://stopped", "exitCode":1, "reason":"Error",
            "startedAt":"2026-09-12T00:32:36Z", "finishedAt":"2026-09-12T00:32:41Z",
        });
        let mut previous = OperatorState {
            pod_name: "operator".to_owned(),
            pod_uid: "pod".to_owned(),
            container_id: Some("containerd://stopped".to_owned()),
            restart_count: 2,
            image_id: Some("image".to_owned()),
            ready: false,
            termination: Some(stopped.clone()),
            state: json!({"waiting":{"reason":"CrashLoopBackOff"}}),
        };
        let current = OperatorState {
            container_id: Some("containerd://next".to_owned()),
            restart_count: 3,
            state: json!({"running":{"startedAt":"2026-09-12T00:32:52Z"}}),
            ..previous.clone()
        };
        let since = timestamp("2026-09-12T00:31:15Z")?;
        let address = "10.96.239.215:4222";
        let log = "2026-09-12T00:32:41Z\tERROR\tsetup\tunable to create runtime operator\t{\"error\":\"transport error: dial tcp 10.96.239.215:4222: i/o timeout\"}";
        let events = json!({"items":[]});
        assert_eq!(
            supervised_restart(&previous, &current, log, &events, since, address)?["cause"],
            "scheduler-nats-startup-timeout"
        );
        // wamn-203x: the observed container's log is gone, and the next
        // container of the same crash loop records the cause after it.
        let later = log.replace("00:32:41Z", "00:33:03Z");
        assert_eq!(
            supervised_restart(&previous, &current, &later, &events, since, address)?["cause"],
            "scheduler-nats-startup-timeout"
        );
        let earlier = log.replace("00:32:41Z", "00:32:30Z");
        assert!(
            supervised_restart(&previous, &current, &earlier, &events, since, address).is_err()
        );
        previous.termination.as_mut().unwrap()["containerID"] = json!("containerd://other");
        assert!(supervised_restart(&previous, &current, log, &events, since, address).is_err());
        previous.termination = Some(stopped.clone());
        previous.termination.as_mut().unwrap()["startedAt"] = json!("2026-09-12T00:32:35Z");
        assert!(supervised_restart(&previous, &current, log, &events, since, address).is_err());
        previous.termination = None;
        assert!(supervised_restart(&previous, &current, log, &events, since, address).is_err());
        previous.termination = Some(stopped);
        assert!(supervised_restart(&previous, &current, "", &events, since, address).is_err());
        previous.state = json!({"waiting":null});
        assert!(supervised_restart(&previous, &current, log, &events, since, address).is_err());
        previous.state = json!({"waiting":{"reason":"CrashLoopBackOff"}});
        previous.container_id = None;
        previous.termination.as_mut().unwrap()["containerID"] = Value::Null;
        let mut unidentified = current.clone();
        unidentified.termination.as_mut().unwrap()["containerID"] = Value::Null;
        assert!(
            supervised_restart(&previous, &unidentified, log, &events, since, address).is_err()
        );
        Ok(())
    }

    #[test]
    fn retained_crd_hashes_use_recursive_key_order_and_only_native_defaults() -> anyhow::Result<()>
    {
        let root = retained_root();
        let records = read(&root.join("deployment-crds-001/crd-inventory.json"))?;
        let observed = root.join("live-receiving-009/journey/operator-recovery");
        let bytes = fs::read(observed.join("0003-distributed-crds-client-decode.stdout"))?;
        let expected = serde_json::Deserializer::from_slice(&bytes)
            .into_iter::<Value>()
            .collect::<Result<Vec<_>, _>>()?;
        let actual = read(&observed.join("0004-installed-crds.stdout"))?;
        assert_eq!(expected.len(), 5);
        for crd in expected {
            let name = text(&crd, "/metadata/name")?;
            assert_eq!(
                records["versions"]["2.9.0"][name]["spec_sha256"],
                digest(&crd["spec"])?
            );
            let installed = array(&actual, "/items")?
                .iter()
                .find(|value| value["metadata"]["name"] == name)
                .context("the recorded CRD is installed")?;
            assert_eq!(
                normalized_crd(&installed["spec"])?,
                normalized_crd(&crd["spec"])?
            );
            let mut changed = crd["spec"].clone();
            changed["scope"] = json!("changed");
            assert_ne!(
                normalized_crd(&installed["spec"])?,
                normalized_crd(&changed)?
            );
        }
        assert_eq!(
            digest(&serde_json::from_str::<Value>(
                r#"{"b":{"z":1,"a":2},"a":3}"#
            )?)?,
            digest(&serde_json::from_str::<Value>(
                r#"{"a":3,"b":{"a":2,"z":1}}"#
            )?)?
        );
        println!("OPERATOR_CRD_REPLAY distributed_hashes=5 installed_schemas=5 defaults_only=true");
        Ok(())
    }
}

//! Receiving requests during a scheduler outage and operator restart.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};
use wamn_ctl::dev::environment::secret_value;

use super::{ReceivingCluster, kubectl, materializer_case};

const SYSTEM: &str = "wamn-system";
const REQUEST_ID: &str = "00000000-0000-4000-8000-000000000929";
const ORDER_ID: &str = "00000000-0000-0000-0000-000000000301";
const OUTAGE_SECONDS: u64 = 150;
const RECOVERY_SECONDS: u64 = 120;

fn now() -> f64 {
    chrono::Utc::now().timestamp_micros() as f64 / 1_000_000.0
}
fn timestamp(value: &str) -> anyhow::Result<f64> {
    Ok(chrono::DateTime::parse_from_rfc3339(value)?.timestamp_micros() as f64 / 1_000_000.0)
}
fn text<'a>(value: &'a Value, path: &str) -> anyhow::Result<&'a str> {
    value
        .pointer(path)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("the native object has text at {path}"))
}
fn array<'a>(value: &'a Value, path: &str) -> anyhow::Result<&'a Vec<Value>> {
    value
        .pointer(path)
        .and_then(Value::as_array)
        .with_context(|| format!("the native object has an array at {path}"))
}
fn ready(value: &Value) -> bool {
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
fn digest(value: &Value) -> anyhow::Result<String> {
    Ok(hex::encode(
        ring::digest::digest(
            &ring::digest::SHA256,
            &serde_json::to_vec(&canonical(value))?,
        )
        .as_ref(),
    ))
}
fn normalized_crd(spec: &Value) -> anyhow::Result<Value> {
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
fn host_ids(items: &[Value]) -> anyhow::Result<Vec<(String, String)>> {
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
fn pod_ids(items: &[Value], name: &str) -> anyhow::Result<Vec<(String, String, u64, String)>> {
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
struct OperatorState {
    pod_name: String,
    pod_uid: String,
    container_id: Option<String>,
    restart_count: u64,
    image_id: Option<String>,
    ready: bool,
    termination: Option<Value>,
    state: Value,
}
fn operator_state(items: &[Value]) -> anyhow::Result<OperatorState> {
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
fn supervised_restart(
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
        let finished = timestamp(text(termination, "/finishedAt")?)?;
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
            if started <= stamp && stamp < finished + 1.0 && detail["error"] == expected {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Sample {
    timestamp: i64,
    status: u16,
    transport_failure: Option<String>,
    content_type: String,
    seconds: f64,
    body: String,
    result: String,
    origin: String,
}
fn classify(sample: &Sample, expected_id: &str) -> anyhow::Result<&'static str> {
    if sample.status == 200 && sample.transport_failure.is_none() {
        let value: Value = serde_json::from_str(&sample.body)?;
        ensure!(
            sample.content_type.split(';').next() == Some("application/json"),
            "the HTTP 200 response must be JSON"
        );
        let values = value
            .as_array()
            .context("the Receiving response is an array")?;
        ensure!(
            values.len() == 1
                && values[0]["request_id"] == REQUEST_ID
                && values[0].get("error").is_none()
                && values[0]["value"]["id"] == expected_id,
            "HTTP 200 must return the selected Receiving purchase order"
        );
        Ok("serving")
    } else if sample.transport_failure.is_none()
        && matches!(sample.status, 404 | 503)
        && sample.body.is_empty()
    {
        Ok(if sample.status == 404 {
            "native_404"
        } else {
            "native_503"
        })
    } else if sample.status == 0
        && matches!(
            sample.transport_failure.as_deref(),
            Some("connect" | "timeout" | "empty_response" | "receive")
        )
    {
        Ok("transport_failure")
    } else {
        anyhow::bail!("the route returned an unexpected response: {sample:?}")
    }
}
fn transport(error: &reqwest::Error) -> anyhow::Result<String> {
    if error.is_timeout() {
        return Ok("timeout".to_owned());
    }
    if error.is_connect() {
        return Ok("connect".to_owned());
    }
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = cause {
        if let Some(error) = error.downcast_ref::<hyper::Error>() {
            if error.is_incomplete_message() {
                return Ok("empty_response".to_owned());
            }
            if error.is_closed() {
                return Ok("receive".to_owned());
            }
        }
        if let Some(error) = error.downcast_ref::<std::io::Error>() {
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
            ) {
                return Ok("receive".to_owned());
            }
        }
        cause = error.source();
    }
    anyhow::bail!("the route request failed outside the retained transport classes: {error}")
}
async fn sample(
    http: &reqwest::Client,
    endpoint: &str,
    route_host: &str,
    token: &str,
) -> anyhow::Result<Sample> {
    let started = Instant::now();
    let response = http
        .post(format!("{endpoint}/purchase_order/get"))
        .header("Host", route_host)
        .bearer_auth(token)
        .json(&json!([{"id":ORDER_ID,"request_id":REQUEST_ID}]))
        .send()
        .await;
    let mut sample = Sample {
        timestamp: chrono::Utc::now().timestamp(),
        status: 0,
        transport_failure: None,
        content_type: String::new(),
        seconds: 0.0,
        body: String::new(),
        result: String::new(),
        origin: endpoint.to_owned(),
    };
    match response {
        Ok(response) => {
            sample.status = response.status().as_u16();
            sample.content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned();
            match response.bytes().await {
                Ok(body) => {
                    sample.body = String::from_utf8(body.to_vec())
                        .context("the route response body is UTF-8")?
                }
                Err(error) => sample.transport_failure = Some(transport(&error)?),
            }
        }
        Err(error) => sample.transport_failure = Some(transport(&error)?),
    }
    sample.seconds = started.elapsed().as_secs_f64();
    sample.timestamp = chrono::Utc::now().timestamp();
    sample.result = classify(&sample, ORDER_ID)?.to_owned();
    Ok(sample)
}

type Samples = Result<Vec<Sample>, String>;
async fn sample_requests(
    http: reqwest::Client,
    endpoint: String,
    route_host: String,
    token: String,
    evidence: PathBuf,
    changed: watch::Sender<Samples>,
    mut stop: oneshot::Receiver<()>,
) -> anyhow::Result<Vec<Sample>> {
    let mut samples = Vec::new();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(840) {
        let value = match sample(&http, &endpoint, &route_host, &token).await {
            Ok(value) => value,
            Err(error) => {
                let _ = changed.send(Err(error.to_string()));
                return Err(error);
            }
        };
        samples.push(value);
        fs::write(
            evidence.join("route-samples.json"),
            serde_json::to_vec_pretty(&samples)?,
        )?;
        let _ = changed.send(Ok(samples.clone()));
        tokio::select! { _ = &mut stop => return Ok(samples), _ = tokio::time::sleep(Duration::from_secs(5)) => {} }
    }
    anyhow::bail!("the operator request sampler exceeded its retained 840-second bound")
}

#[derive(Serialize)]
struct Snapshot {
    hosts: Vec<Value>,
    pods: Vec<Value>,
    operator_pods: Vec<Value>,
    endpointslices: Vec<Value>,
    workloads: Vec<Value>,
}
#[derive(Clone, Copy)]
enum Continuity {
    Original,
    Supervised,
    Restarted,
}
struct Recovery<'a> {
    cluster: &'a ReceivingCluster,
    evidence: PathBuf,
    command_number: usize,
    host_selector: String,
    original_hosts: Vec<(String, String)>,
    original_pods: Vec<(String, String, u64, String)>,
    original_operator: Vec<(String, String, u64, String)>,
    operator_initial: Option<OperatorState>,
    operator_seen: Option<OperatorState>,
    operator_transitions: Vec<Value>,
    operator_fresh_after: f64,
    scheduler_fault_started: Option<f64>,
    scheduler_address: String,
    phases: BTreeMap<String, Value>,
}
impl Recovery<'_> {
    fn write(&self, name: &str, value: &impl Serialize) -> anyhow::Result<()> {
        fs::write(
            self.evidence.join(format!("{name}.json")),
            serde_json::to_vec_pretty(value)?,
        )?;
        Ok(())
    }
    async fn run(
        &mut self,
        label: &str,
        args: &[&str],
        timeout: u64,
        tolerate: bool,
    ) -> anyhow::Result<Vec<u8>> {
        self.command_number += 1;
        let stem = format!("{:04}-{label}", self.command_number);
        let mut command = kubectl(&self.cluster.resources);
        command
            .arg("--request-timeout=20s")
            .args(args)
            .kill_on_drop(true);
        let arguments = std::iter::once(command.as_std().get_program())
            .chain(command.as_std().get_args())
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let started = now();
        let (status, stdout, stderr) =
            match tokio::time::timeout(Duration::from_secs(timeout), command.output()).await {
                Ok(output) => {
                    let output = output?;
                    (
                        output.status.code().unwrap_or(-1),
                        output.stdout,
                        output.stderr,
                    )
                }
                Err(_) => (
                    124,
                    Vec::new(),
                    b"the Kubernetes command exceeded its time limit\n".to_vec(),
                ),
            };
        fs::write(self.evidence.join(format!("{stem}.stdout")), &stdout)?;
        fs::write(self.evidence.join(format!("{stem}.stderr")), stderr)?;
        let mut record = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.evidence.join("commands.jsonl"))?;
        writeln!(
            record,
            "{}",
            serde_json::to_string(
                &json!({"command":arguments,"exit_code":status,"started":started,"elapsed_seconds":now()-started,"output_stem":stem})
            )?
        )?;
        ensure!(
            tolerate || status == 0,
            "{label} failed with exit {status}, see {stem}.stderr"
        );
        Ok(stdout)
    }
    async fn get(
        &mut self,
        label: &str,
        namespace: &str,
        resource: &str,
        extra: &[&str],
    ) -> anyhow::Result<Value> {
        let mut args = vec!["-n", namespace, "get", resource];
        args.extend(extra);
        args.extend(["-o", "json"]);
        Ok(serde_json::from_slice(
            &self.run(label, &args, 30, false).await?,
        )?)
    }
    async fn snapshot(&mut self, label: &str) -> anyhow::Result<Snapshot> {
        let namespace = self.cluster.resources.name.clone();
        let selector = self.host_selector.clone();
        let value = Snapshot {
            hosts: array(
                &self
                    .get(
                        &format!("{label}-hosts"),
                        SYSTEM,
                        "hosts",
                        &["-l", "hostgroup=default"],
                    )
                    .await?,
                "/items",
            )?
            .clone(),
            pods: array(
                &self
                    .get(
                        &format!("{label}-host-pods"),
                        &namespace,
                        "pods",
                        &["-l", &selector],
                    )
                    .await?,
                "/items",
            )?
            .clone(),
            operator_pods: array(
                &self
                    .get(
                        &format!("{label}-operator-pods"),
                        SYSTEM,
                        "pods",
                        &["-l", "wasmcloud.com/name=runtime-operator"],
                    )
                    .await?,
                "/items",
            )?
            .clone(),
            endpointslices: array(
                &self
                    .get(
                        &format!("{label}-endpointslices"),
                        &namespace,
                        "endpointslices",
                        &["-l", "kubernetes.io/service-name=flow-http"],
                    )
                    .await?,
                "/items",
            )?
            .clone(),
            workloads: array(
                &self
                    .get(&format!("{label}-workloads"), &namespace, "workloads", &[])
                    .await?,
                "/items",
            )?
            .clone(),
        };
        self.write(&format!("{label}-state"), &value)?;
        Ok(value)
    }
    async fn continuity(&mut self, value: &Snapshot, mode: Continuity) -> anyhow::Result<bool> {
        ensure!(
            host_ids(&value.hosts)? == self.original_hosts,
            "the scheduler outage deleted or replaced a Host object"
        );
        ensure!(
            pod_ids(&value.pods, "host")? == self.original_pods,
            "a host process changed during operator recovery"
        );
        match mode {
            Continuity::Original => {
                ensure!(
                    pod_ids(&value.operator_pods, "runtime-operator")? == self.original_operator,
                    "the operator process changed before the scheduler fault"
                );
                Ok(true)
            }
            Continuity::Restarted => {
                Ok(value.operator_pods.len() == 1 && value.operator_pods.iter().all(ready))
            }
            Continuity::Supervised => {
                let current = operator_state(&value.operator_pods)?;
                let initial = self
                    .operator_initial
                    .as_ref()
                    .context("the initial operator was observed")?;
                let previous = self
                    .operator_seen
                    .as_ref()
                    .context("the previous operator was observed")?
                    .clone();
                ensure!(
                    current.pod_uid == initial.pod_uid && current.image_id == initial.image_id,
                    "the operator pod or image changed during the scheduler fault"
                );
                if current.restart_count != previous.restart_count {
                    let label = format!("operator-transition-{}", current.restart_count);
                    let pod = format!("pod/{}", current.pod_name);
                    let previous_log = self
                        .run(
                            &format!("{label}-previous-log"),
                            &[
                                "-n",
                                SYSTEM,
                                "logs",
                                &pod,
                                "-c",
                                "runtime-operator",
                                "--previous",
                                "--timestamps",
                            ],
                            30,
                            true,
                        )
                        .await?;
                    let event_log = self
                        .run(
                            &format!("{label}-events"),
                            &[
                                "-n",
                                SYSTEM,
                                "get",
                                "events",
                                "--field-selector",
                                &format!("involvedObject.uid={}", current.pod_uid),
                                "-o",
                                "json",
                            ],
                            30,
                            true,
                        )
                        .await?;
                    self.write(
                        &format!("{label}-state"),
                        &json!({"previous":previous,"current":current}),
                    )?;
                    let events = serde_json::from_slice(&event_log).unwrap_or_else(|_| json!({}));
                    let mut transition = supervised_restart(
                        &previous,
                        &current,
                        &String::from_utf8_lossy(&previous_log),
                        &events,
                        self.scheduler_fault_started
                            .context("the scheduler fault start was recorded")?,
                        &self.scheduler_address,
                    )?;
                    self.operator_fresh_after = now();
                    transition["observed_at"] = json!(self.operator_fresh_after);
                    self.operator_transitions.push(transition);
                    self.write("operator-supervision", &json!({"initial":self.operator_initial,"transitions":self.operator_transitions}))?;
                } else {
                    ensure!(
                        current.container_id == previous.container_id,
                        "the operator container changed without a recorded restart"
                    );
                }
                let ready = current.ready;
                self.operator_seen = Some(current);
                Ok(ready)
            }
        }
    }
    fn samples(&self, samples: &watch::Receiver<Samples>) -> anyhow::Result<Vec<Sample>> {
        let values = samples.borrow().clone().map_err(anyhow::Error::msg)?;
        self.write("route-samples", &values)?;
        Ok(values)
    }
    async fn recovered(
        &mut self,
        label: &str,
        after: f64,
        mode: Continuity,
        samples: &watch::Receiver<Samples>,
    ) -> anyhow::Result<Snapshot> {
        let deadline = Instant::now()
            + Duration::from_secs_f64((RECOVERY_SECONDS as f64 - (now() - after)).max(0.0));
        let mut ready_since = None;
        loop {
            let current = self.snapshot(label).await?;
            let operator_ready = self.continuity(&current, mode).await?;
            let fresh_after = if matches!(mode, Continuity::Supervised) {
                after.max(self.operator_fresh_after)
            } else {
                after
            };
            let fresh = current
                .hosts
                .iter()
                .map(|host| Ok(timestamp(text(host, "/status/lastSeen")?)? >= fresh_after))
                .collect::<anyhow::Result<Vec<_>>>()?
                .into_iter()
                .all(|fresh| fresh);
            if operator_ready && current.hosts.iter().all(ready) && fresh {
                ready_since.get_or_insert_with(now);
            } else {
                ready_since = None;
            }
            let responses = self.samples(samples)?;
            if let (Some(ready_since), Some(response)) = (ready_since, responses.last()) {
                if response.timestamp as f64 >= ready_since && response.result == "serving" {
                    let elapsed = now() - after;
                    ensure!(
                        elapsed <= RECOVERY_SECONDS as f64,
                        "recovery exceeded the unchanged 120-second ceiling"
                    );
                    let mut workloads = current
                        .workloads
                        .iter()
                        .map(|workload| text(workload, "/metadata/uid").map(str::to_owned))
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    workloads.sort();
                    self.phases.insert(label.to_owned(), json!({"started":after,"ready_and_serving_after_seconds":elapsed,
                        "readiness_observed":ready_since,"exact_200_after_readiness":response,"workload_uids":workloads}));
                    self.write("phases", &self.phases)?;
                    return Ok(current);
                }
            }
            ensure!(
                Instant::now() < deadline,
                "{label} did not recover within 120 seconds"
            );
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
    async fn installed_crds(&mut self) -> anyhow::Result<()> {
        let source = self
            .cluster
            .resources
            .repository
            .join("evidence/perf/2026.09/wasmcloud-2-9-cutover");
        let records: Value = serde_json::from_slice(&fs::read(
            source.join("deployment-crds-001/crd-inventory.json"),
        )?)?;
        let charts: Value = serde_json::from_slice(&fs::read(
            source.join("deployment-crds-001/distributed-chart-identities.json"),
        )?)?;
        let path = source.join("deployment-crds-001/helm-show-crds-2.9.0.stdout");
        let raw = self
            .run(
                "distributed-crds-client-decode",
                &[
                    "create",
                    "--dry-run=client",
                    "--validate=false",
                    "-f",
                    path.to_str().context("the CRD path is text")?,
                    "-o",
                    "json",
                ],
                30,
                false,
            )
            .await?;
        let expected = serde_json::Deserializer::from_slice(&raw)
            .into_iter::<Value>()
            .collect::<Result<Vec<_>, _>>()?;
        let names = records["versions"]["2.9.0"]
            .as_object()
            .context("the retained chart has CRD records")?
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let expected_names = expected
            .iter()
            .map(|crd| text(crd, "/metadata/name").map(str::to_owned))
            .collect::<anyhow::Result<BTreeSet<_>>>()?;
        ensure!(
            names.len() == 5 && expected.len() == 5 && expected_names == names,
            "the distributed chart must contain exactly its five recorded CRDs"
        );
        let mut args = vec!["get", "crd"];
        args.extend(names.iter().map(String::as_str));
        args.extend(["-o", "json"]);
        let actual: Value =
            serde_json::from_slice(&self.run("installed-crds", &args, 30, false).await?)?;
        let mut rows = Vec::new();
        for crd in &expected {
            let name = text(crd, "/metadata/name")?;
            let actual = array(&actual, "/items")?
                .iter()
                .find(|actual| actual["metadata"]["name"] == name)
                .context("the distributed CRD is installed")?;
            let expected_hash = digest(&crd["spec"])?;
            ensure!(
                records["versions"]["2.9.0"][name]["spec_sha256"] == expected_hash,
                "the captured distributed CRD source changed: {name}"
            );
            ensure!(
                normalized_crd(&actual["spec"])? == normalized_crd(&crd["spec"])?,
                "the installed CRD differs from the pinned distributed schema: {name}"
            );
            ensure!(
                ["Established", "NamesAccepted"].into_iter().all(|kind| {
                    actual["status"]["conditions"]
                        .as_array()
                        .is_some_and(|conditions| {
                            conditions.iter().any(|condition| {
                                condition["type"] == kind && condition["status"] == "True"
                            })
                        })
                }),
                "the installed CRD is not established: {name}"
            );
            ensure!(
                actual["status"]["storedVersions"] == json!(["v1alpha1"]),
                "the CRD has another storage version"
            );
            rows.push(json!({"name":name,"kind":crd["spec"]["names"]["kind"],"uid":actual["metadata"]["uid"],
                "distributed_spec_sha256":expected_hash,"installed_spec_sha256":digest(&actual["spec"])?,
                "defaulted_spec_sha256":digest(&normalized_crd(&actual["spec"])?)?}));
        }
        self.write(
            "installed-crd-identity",
            &json!({"result":"pass","chart":charts["2.9.0"],"source":records["source_commit"],
            "defaults":["conversion.strategy=None","preserveUnknownFields=false"],"crds":rows}),
        )
    }
    async fn final_operator_logs(&mut self) -> anyhow::Result<()> {
        self.run(
            "final-operator-log",
            &[
                "-n",
                SYSTEM,
                "logs",
                "deployment/runtime-operator",
                "--tail=2000",
            ],
            30,
            true,
        )
        .await?;
        let raw = self
            .run(
                "final-operator-pods",
                &[
                    "-n",
                    SYSTEM,
                    "get",
                    "pods",
                    "-l",
                    "wasmcloud.com/name=runtime-operator",
                    "-o",
                    "json",
                ],
                30,
                true,
            )
            .await?;
        let pods: Value = serde_json::from_slice(&raw).unwrap_or_else(|_| json!({}));
        let initial = self
            .operator_initial
            .as_ref()
            .context("the initial operator was observed")?;
        let mut targets = BTreeSet::from([(initial.pod_uid.clone(), initial.pod_name.clone())]);
        for pod in pods["items"].as_array().into_iter().flatten() {
            targets.insert((
                text(pod, "/metadata/uid")?.to_owned(),
                text(pod, "/metadata/name")?.to_owned(),
            ));
        }
        for (index, (uid, name)) in targets.into_iter().enumerate() {
            let label = format!("final-operator-{index}");
            let pod = format!("pod/{name}");
            self.run(
                &format!("{label}-current-log"),
                &[
                    "-n",
                    SYSTEM,
                    "logs",
                    &pod,
                    "-c",
                    "runtime-operator",
                    "--timestamps",
                    "--tail=2000",
                ],
                30,
                true,
            )
            .await?;
            self.run(
                &format!("{label}-previous-log"),
                &[
                    "-n",
                    SYSTEM,
                    "logs",
                    &pod,
                    "-c",
                    "runtime-operator",
                    "--previous",
                    "--timestamps",
                ],
                30,
                true,
            )
            .await?;
            self.run(
                &format!("{label}-events"),
                &[
                    "-n",
                    SYSTEM,
                    "get",
                    "events",
                    "--field-selector",
                    &format!("involvedObject.uid={uid}"),
                    "-o",
                    "json",
                ],
                30,
                true,
            )
            .await?;
        }
        Ok(())
    }
}

pub(super) async fn assert_recovery(cluster: &ReceivingCluster) -> anyhow::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    let resources = &cluster.resources;
    let evidence = resources.evidence.join("operator-recovery");
    ensure!(
        !evidence.starts_with(&resources.repository) && !evidence.exists(),
        "operator recovery evidence needs a new directory outside the source tree"
    );
    fs::DirBuilder::new().mode(0o700).create(&evidence)?;
    ensure!(
        resources.work.join("kubeconfig").is_file(),
        "the owner's private kubeconfig is present"
    );
    let mut recovery = Recovery {
        cluster,
        evidence: evidence.clone(),
        command_number: 0,
        host_selector: String::new(),
        original_hosts: Vec::new(),
        original_pods: Vec::new(),
        original_operator: Vec::new(),
        operator_initial: None,
        operator_seen: None,
        operator_transitions: Vec::new(),
        operator_fresh_after: 0.0,
        scheduler_fault_started: None,
        scheduler_address: String::new(),
        phases: BTreeMap::new(),
    };
    let context = format!("kind-{}", resources.name);
    let contexts = String::from_utf8(
        recovery
            .run(
                "private-context",
                &["config", "get-contexts", "-o", "name"],
                30,
                false,
            )
            .await?,
    )?;
    ensure!(
        contexts.split_whitespace().collect::<Vec<_>>() == [context.as_str()],
        "the private kubeconfig contains only the owned context"
    );
    let selected = recovery
        .run(
            "private-cluster",
            &[
                "config",
                "view",
                "--minify",
                "-o",
                "jsonpath={.contexts[0].context.cluster}",
            ],
            30,
            false,
        )
        .await?;
    ensure!(
        selected == context.as_bytes(),
        "the private context selects the owned cluster"
    );
    recovery.installed_crds().await?;
    let operator = recovery
        .get(
            "operator-deployment",
            SYSTEM,
            "deployment/runtime-operator",
            &[],
        )
        .await?;
    let scheduler = recovery
        .get("scheduler-deployment", SYSTEM, "deployment/nats", &[])
        .await?;
    let service = recovery
        .get("scheduler-service", SYSTEM, "service/nats", &[])
        .await?;
    let address = text(&service, "/spec/clusterIP")?;
    ensure!(
        address != "None"
            && array(&service, "/spec/ports")?
                .iter()
                .any(|port| port["port"] == 4222 && port["protocol"] == "TCP"),
        "the scheduler Service identifies its configured NATS endpoint"
    );
    recovery.scheduler_address = format!("{address}:4222");
    ensure!(
        operator["spec"]["replicas"] == 1 && scheduler["spec"]["replicas"] == 1,
        "the operator and scheduler each have one replica"
    );
    ensure!(
        array(&operator, "/spec/template/spec/containers")?
            .iter()
            .any(
                |container| container["image"] == "ghcr.io/wasmcloud/runtime-operator:2.9.0"
                    && container["args"]
                        .as_array()
                        .is_some_and(|args| args
                            .iter()
                            .any(|arg| arg
                                == "-nats-url=nats://nats.wamn-system.svc.cluster.local:4222"))
            ),
        "the operator runs the pinned source and scheduler endpoint"
    );
    let host = recovery
        .get(
            "host-deployment",
            &resources.name,
            "deployment/hostgroup-default",
            &[],
        )
        .await?;
    let labels = host
        .pointer("/spec/selector/matchLabels")
        .and_then(Value::as_object)
        .context("the host deployment has selector labels")?;
    recovery.host_selector = labels
        .iter()
        .map(|(key, value)| {
            Ok(format!(
                "{key}={}",
                value.as_str().context("the host selector value is text")?
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .join(",");
    ensure!(
        !recovery.host_selector.is_empty() && host["spec"]["replicas"] == 3,
        "three Receiving hosts are required"
    );
    ensure!(
        array(&host, "/spec/template/spec/containers")?
            .iter()
            .any(|container| container["name"] == "host"
                && container["image"] == resources.host_image),
        "operator recovery must reuse the selected Receiving host image"
    );
    let before = recovery.snapshot("before").await?;
    ensure!(
        before.hosts.len() == 3 && before.pods.len() == 3 && before.hosts.iter().all(ready),
        "the initial host group has three ready hosts and pods"
    );
    recovery.original_hosts = host_ids(&before.hosts)?;
    recovery.original_pods = pod_ids(&before.pods, "host")?;
    recovery.original_operator = pod_ids(&before.operator_pods, "runtime-operator")?;
    ensure!(
        recovery.original_operator.len() == 1,
        "one operator process is required"
    );
    let image: Value =
        serde_json::from_slice(&fs::read(resources.repository.join(
            "evidence/perf/2026.09/wasmcloud-2-9-cutover/deployment-001/distributed-image.json",
        ))?)?;
    let mut digests = vec![text(&image, "/digest")?];
    for platform in array(&image, "/platforms")?
        .iter()
        .filter(|platform| platform["platform"] == json!({"architecture":"amd64","os":"linux"}))
    {
        digests.push(text(platform, "/digest")?);
    }
    ensure!(
        digests
            .iter()
            .any(|digest| recovery.original_operator[0].3.ends_with(digest)),
        "the operator image differs from the distributed image identity"
    );
    recovery.write(
        "operator-image-identity",
        &json!({"distributed":image,"observed":recovery.original_operator}),
    )?;
    let initial = operator_state(&before.operator_pods)?;
    recovery.operator_initial = Some(initial.clone());
    recovery.operator_seen = Some(initial);

    let endpoint = materializer_case::endpoint(cluster, "receiving-operator-recovery").await?;
    recovery.write("request-origin",&json!({"kind":"owned-nodeport","endpoint":endpoint,"cluster":resources.name,
        "route_host":cluster.inputs.route_host,"route":"/purchase_order/get","connect_timeout_seconds":2,"read_timeout_seconds":5,"total_timeout_seconds":5}))?;
    let token = secret_value(&cluster.inputs.route_caller_secret_output, "token")?;
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .read_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()?;
    let (changed, samples) = watch::channel(Ok(Vec::<Sample>::new()));
    let (stop, stopped) = oneshot::channel();
    let mut sampler = tokio::spawn(sample_requests(
        http,
        endpoint,
        cluster.inputs.route_host.clone(),
        token,
        evidence.clone(),
        changed,
        stopped,
    ));
    let mut scheduler_stopped = false;
    let result = async {
        recovery.recovered("initial",now(),Continuity::Original,&samples).await?;
        let settling = Instant::now();
        while settling.elapsed() < Duration::from_secs(75) {
            let current = recovery.snapshot("settling").await?;
            recovery.continuity(&current,Continuity::Original).await?;
            ensure!(recovery.samples(&samples)?.last().is_some_and(|sample| sample.result == "serving"), "normal serving failed before the scheduler fault");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        scheduler_stopped = true; recovery.scheduler_fault_started = Some(now());
        recovery.run("stop-scheduler", &["-n",SYSTEM,"scale","deployment/nats","--replicas=0"],30,false).await?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let pods = recovery.get("scheduler-stopping",SYSTEM,"pods",&["-l","wasmcloud.com/name=nats"]).await?;
            if array(&pods,"/items")?.is_empty() { break; }
            ensure!(Instant::now() < deadline,"the scheduler did not stop within 60 seconds");
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let stopped = now(); let outage = Instant::now();
        recovery.phases.insert("scheduler-stopped".to_owned(),json!({"started":stopped,"required_seconds":OUTAGE_SECONDS}));
        recovery.write("phases",&recovery.phases)?;
        let mut guarded = BTreeSet::new();
        while outage.elapsed() < Duration::from_secs(OUTAGE_SECONDS) {
            let pods = recovery.get("scheduler-still-stopped",SYSTEM,"pods",&["-l","wasmcloud.com/name=nats"]).await?;
            ensure!(array(&pods,"/items")?.is_empty(),"the scheduler resumed before the required outage interval ended");
            let current = recovery.snapshot("scheduler-down").await?;
            recovery.continuity(&current,Continuity::Supervised).await?;
            for host in &current.hosts {
                if array(host,"/status/conditions")?.iter().any(|condition| condition["type"] == "Ready" && condition["status"] == "Unknown"
                    && condition["message"].as_str().is_some_and(|message| message.contains("operator has not been hearing the fleet"))) {
                    guarded.insert(text(host,"/metadata/uid")?.to_owned());
                }
            }
            recovery.samples(&samples)?;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        ensure!(guarded == recovery.original_hosts.iter().map(|(uid,_)|uid.clone()).collect(), "the native loss-of-heartbeat guard was not observed for every Host");
        ensure!(recovery.samples(&samples)?.iter().any(|sample| sample.timestamp as f64 >= stopped), "no actual request sampled the stopped interval");
        let resumed = now();
        let phase = recovery.phases.get_mut("scheduler-stopped").context("the scheduler outage was recorded")?;
        phase["ended"] = json!(resumed); phase["actual_seconds"] = json!(outage.elapsed().as_secs_f64()); phase["fleet_guard_host_uids"] = json!(guarded);
        recovery.write("phases",&recovery.phases)?;
        recovery.run("restore-scheduler", &["-n",SYSTEM,"scale","deployment/nats","--replicas=1"],30,false).await?;
        recovery.recovered("scheduler-recovery",resumed,Continuity::Supervised,&samples).await?;
        scheduler_stopped = false;
        recovery.run("scheduler-recovered-operator-log", &["-n",SYSTEM,"logs","deployment/runtime-operator","--tail=2000"],30,false).await?;
        let restarted = now();
        recovery.run("restart-operator", &["-n",SYSTEM,"rollout","restart","deployment/runtime-operator"],30,false).await?;
        recovery.run("wait-operator", &["-n",SYSTEM,"rollout","status","deployment/runtime-operator","--timeout=100s"],110,false).await?;
        let after = recovery.recovered("operator-restart",restarted,Continuity::Restarted,&samples).await?;
        let new_operator = pod_ids(&after.operator_pods,"runtime-operator")?;
        ensure!(new_operator.len() == 1 && new_operator[0].0 != recovery.original_operator[0].0 && new_operator[0].3 == recovery.original_operator[0].3,
            "the operator must restart in a new pod with the same image");
        let samples = recovery.samples(&samples)?;
        let mut counts = BTreeMap::<&str,usize>::new();
        for sample in &samples { *counts.entry(&sample.result).or_default() += 1; }
        recovery.write("route-summary", &json!({"sample_interval_seconds":5,"classifications":counts,
            "origin":"owned-nodeport","non_success":samples.iter().filter(|sample|sample.result!="serving").collect::<Vec<_>>()}))?;
        let ids = |workloads: &[Value]| -> anyhow::Result<Vec<String>> {
            let mut ids=workloads.iter().map(|workload|text(workload,"/metadata/uid").map(str::to_owned)).collect::<anyhow::Result<Vec<_>>>()?; ids.sort(); Ok(ids)
        };
        Ok::<Value,anyhow::Error>(json!({"result":"pass","context":context,"scope":"shared-scheduler-outage-and-operator-restart",
            "request_origin":"owned-nodeport","phases":recovery.phases,"hosts_preserved":recovery.original_hosts,"host_processes_preserved":recovery.original_pods,
            "operator_before":recovery.original_operator,"operator_after":new_operator,"scheduler_operator_transitions":recovery.operator_transitions,
            "before_workload_uids":ids(&before.workloads)?,"after_workload_uids":ids(&after.workloads)?}))
    }.await;
    let mut cleanup_errors = Vec::new();
    if scheduler_stopped {
        if let Err(error) = recovery
            .run(
                "restore-scheduler-after-failure",
                &["-n", SYSTEM, "scale", "deployment/nats", "--replicas=1"],
                30,
                false,
            )
            .await
        {
            cleanup_errors.push(error.to_string());
        }
    }
    let _ = stop.send(());
    match tokio::time::timeout(Duration::from_secs(10), &mut sampler).await {
        Ok(Ok(Ok(samples))) => recovery.write("route-samples", &samples)?,
        Ok(Ok(Err(error))) => cleanup_errors.push(error.to_string()),
        Ok(Err(error)) => cleanup_errors.push(error.to_string()),
        Err(_) => {
            sampler.abort();
            let _ = sampler.await;
            cleanup_errors.push("the request sampler did not stop within ten seconds".to_owned());
        }
    }
    if let Err(error) = recovery
        .run(
            "delete-sampler-endpoint",
            &[
                "-n",
                &resources.name,
                "delete",
                "service/receiving-operator-recovery",
                "endpointslice/receiving-operator-recovery",
                "--wait=true",
                "--timeout=30s",
            ],
            40,
            false,
        )
        .await
    {
        cleanup_errors.push(error.to_string());
    }
    if let Err(error) = recovery.final_operator_logs().await {
        cleanup_errors.push(error.to_string());
    }
    recovery.write("cleanup",&json!({"result":if cleanup_errors.is_empty(){"pass"}else{"fail"},"errors":cleanup_errors,"sampler":"stopped"}))?;
    ensure!(
        cleanup_errors.is_empty(),
        "operator recovery cleanup failed: {cleanup_errors:?}"
    );
    recovery.write("result", &result?)?;
    let mut paths = fs::read_dir(&evidence)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let mut hashes = fs::File::create(evidence.join("evidence.sha256"))?;
    for path in paths.into_iter().filter(|path| {
        path.is_file()
            && path
                .file_name()
                .is_some_and(|name| name != "evidence.sha256")
    }) {
        let bytes = fs::read(&path)?;
        writeln!(
            hashes,
            "{}  {}",
            hex::encode(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref()),
            path.strip_prefix(&resources.evidence)?.display()
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retained_root() -> anyhow::Result<PathBuf> {
        Ok(crate::route_authentication_live::repository_root()?
            .join("evidence/perf/2026.09/wasmcloud-2-9-cutover"))
    }
    fn read(path: &Path) -> anyhow::Result<Value> {
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    #[test]
    fn retained_route_results_keep_their_actual_response_classes() -> anyhow::Result<()> {
        let root = retained_root()?;
        let mut total = 0;
        let mut classes = BTreeSet::new();
        let mut last = None;
        for (run, count) in [("005", 52), ("006", 49), ("008", 45), ("009", 76)] {
            let value = read(&root.join(format!(
                "live-receiving-{run}/journey/operator-recovery/route-samples.json"
            )))?;
            let values = value
                .as_array()
                .context("the retained requests form an array")?;
            assert_eq!(values.len(), count);
            for value in values {
                let failure = match value["curl_exit"]
                    .as_i64()
                    .context("the retained curl exit is an integer")?
                {
                    0 => None,
                    7 => Some("connect"),
                    28 => Some("timeout"),
                    52 => Some("empty_response"),
                    56 => Some("receive"),
                    value => anyhow::bail!("unexpected retained curl exit {value}"),
                };
                let sample = Sample {
                    timestamp: value["timestamp"].as_i64().context("request timestamp")?,
                    status: u16::try_from(value["status"].as_u64().context("request status")?)?,
                    transport_failure: failure.map(str::to_owned),
                    content_type: text(value, "/content_type")?.to_owned(),
                    seconds: value["seconds"].as_f64().context("request elapsed time")?,
                    body: value["body"].as_str().context("request body")?.to_owned(),
                    result: String::new(),
                    origin: "retained-in-cluster-sampler".to_owned(),
                };
                let class = classify(&sample, ORDER_ID)?;
                assert_eq!(value["result"], class);
                classes.insert(class);
                total += 1;
                if class == "serving" {
                    last = Some(sample);
                }
            }
        }
        assert_eq!(total, 222);
        assert_eq!(classes, BTreeSet::from(["serving", "transport_failure"]));
        let mut sample = last.context("the retained data has a successful request")?;
        assert!(classify(&sample, "another-order").is_err());
        sample.content_type = "text/plain".to_owned();
        assert!(classify(&sample, ORDER_ID).is_err());
        sample.status = 404;
        sample.body.clear();
        assert_eq!(classify(&sample, ORDER_ID)?, "native_404");
        sample.status = 503;
        assert_eq!(classify(&sample, ORDER_ID)?, "native_503");
        sample.status = 403;
        assert!(classify(&sample, ORDER_ID).is_err());
        println!(
            "OPERATOR_ROUTE_REPLAY samples={total} recorded_classes=2 native_empty_statuses=2 unexpected_responses=refused"
        );
        Ok(())
    }

    #[test]
    fn retained_operator_transitions_require_the_recorded_cause() -> anyhow::Result<()> {
        let root = retained_root()?.join("live-receiving-008/journey/operator-recovery");
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
        let root = retained_root()?;
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

//! The host pod report that a failed step 6 or step 7 of `apply` carries
//! (docs/plan/alignment.md F2).
//!
//! Helm runs with `--rollback-on-failure`, so the failing pods are gone when
//! `helm upgrade` returns. A [`Poller`] reads the report every 10 s while Helm
//! waits and keeps the last read of each pod. After step 7 fails the pods still
//! exist, and [`collect`] reads the report once. Reading the report is best
//! effort: a failure adds one line and never replaces the step's own error.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::release_chart::Target;

const POLL: Duration = Duration::from_secs(10);
const LOG_LINES: &str = "--tail=50";

#[derive(Debug, Deserialize)]
struct Deployment {
    spec: DeploymentSpec,
}

#[derive(Debug, Deserialize)]
struct DeploymentSpec {
    selector: LabelSelector,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LabelSelector {
    match_labels: BTreeMap<String, String>,
    match_expressions: Vec<Requirement>,
}

#[derive(Debug, Deserialize)]
struct Requirement {
    key: String,
    operator: String,
    #[serde(default)]
    values: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct List<T> {
    items: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
struct Pod {
    metadata: Metadata,
    #[serde(default)]
    status: PodStatus,
}

#[derive(Debug, Clone, Deserialize)]
struct Metadata {
    name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct PodStatus {
    phase: Option<String>,
    conditions: Vec<Condition>,
    container_statuses: Vec<ContainerStatus>,
}

#[derive(Debug, Clone, Deserialize)]
struct Condition {
    #[serde(rename = "type")]
    kind: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContainerStatus {
    name: String,
    #[serde(default)]
    restart_count: u32,
    #[serde(default)]
    state: ContainerState,
    #[serde(default)]
    last_state: ContainerState,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ContainerState {
    waiting: Option<Waiting>,
    running: Option<serde_json::Value>,
    terminated: Option<Terminated>,
}

#[derive(Debug, Clone, Deserialize)]
struct Waiting {
    reason: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Terminated {
    reason: Option<String>,
    exit_code: i32,
}

#[derive(Debug, Clone, Deserialize)]
struct Event {
    reason: Option<String>,
    message: Option<String>,
    count: Option<u32>,
}

/// The logs of one container, read when its restart count was `restarts`.
#[derive(Debug, Clone)]
struct ContainerLogs {
    restarts: u32,
    /// `(previous, lines or the read failure)`.
    reads: Vec<(bool, Result<String, String>)>,
}

#[derive(Debug, Clone)]
struct PodReport {
    pod: Pod,
    events: Result<Vec<Event>, String>,
    logs: BTreeMap<String, ContainerLogs>,
}

/// The last read of each host pod of one release.
#[derive(Debug, Clone, Default)]
pub struct Report {
    deployment: String,
    pods: BTreeMap<String, PodReport>,
    /// Why the last read failed, if it did.
    failure: Option<String>,
}

impl Report {
    fn new(release_name: &str) -> Self {
        Self {
            deployment: format!("hostgroup-{release_name}"),
            ..Self::default()
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "host pods of Deployment {}:", self.deployment)?;
        if self.pods.is_empty() && self.failure.is_none() {
            write!(f, "\n  no pod was found")?;
        }
        for report in self.pods.values() {
            write_pod(f, report)?;
        }
        if let Some(failure) = &self.failure {
            write!(f, "\n  the host pod report could not be read: {failure}")?;
        }
        Ok(())
    }
}

fn write_pod(f: &mut fmt::Formatter<'_>, report: &PodReport) -> fmt::Result {
    let status = &report.pod.status;
    write!(
        f,
        "\npod {}: phase {}, {}",
        report.pod.metadata.name,
        status.phase.as_deref().unwrap_or("unknown"),
        if ready(&report.pod) {
            "Ready"
        } else {
            "not Ready"
        }
    )?;
    for container in &status.container_statuses {
        write!(
            f,
            "\n  container {}: {}; restarts {}",
            container.name,
            state(&container.state),
            container.restart_count
        )?;
        if let Some(last) = &container.last_state.terminated {
            write!(
                f,
                "; last terminated {}, exit code {}",
                last.reason.as_deref().unwrap_or("with no reason"),
                last.exit_code
            )?;
        }
    }
    match &report.events {
        Err(failure) => write!(f, "\n  the warning events could not be read: {failure}")?,
        Ok(events) => {
            for event in events {
                write!(
                    f,
                    "\n  warning {}: {} (x{})",
                    event.reason.as_deref().unwrap_or("Unknown"),
                    event.message.as_deref().unwrap_or_default().trim(),
                    event.count.unwrap_or(1)
                )?;
            }
        }
    }
    for (container, logs) in &report.logs {
        for (previous, read) in &logs.reads {
            let which = if *previous { " (previous)" } else { "" };
            match read {
                Err(failure) => write!(
                    f,
                    "\n  the log of container {container}{which} could not be read: {failure}"
                )?,
                Ok(lines) => {
                    write!(f, "\n  log of container {container}{which}:")?;
                    for line in lines.lines() {
                        write!(f, "\n    {line}")?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn state(state: &ContainerState) -> String {
    if let Some(waiting) = &state.waiting {
        let reason = waiting.reason.as_deref().unwrap_or("with no reason");
        match &waiting.message {
            Some(message) => format!("waiting {reason} ({})", message.trim()),
            None => format!("waiting {reason}"),
        }
    } else if let Some(terminated) = &state.terminated {
        format!(
            "terminated {}, exit code {}",
            terminated.reason.as_deref().unwrap_or("with no reason"),
            terminated.exit_code
        )
    } else if state.running.is_some() {
        "running".to_owned()
    } else {
        "no state".to_owned()
    }
}

fn ready(pod: &Pod) -> bool {
    pod.status
        .conditions
        .iter()
        .any(|condition| condition.kind == "Ready" && condition.status == "True")
}

/// Whether a read fetches the logs of `container`: every read when `all`,
/// otherwise only for a pod that is not Ready or restarted, and only when the
/// restart count changed since the last read.
fn fetch_logs(container: &ContainerStatus, pod_ready: bool, last: Option<u32>, all: bool) -> bool {
    all || ((!pod_ready || container.restart_count > 0) && last != Some(container.restart_count))
}

/// The `-l` argument for a Deployment's `spec.selector`.
fn selector(selector: &LabelSelector) -> anyhow::Result<String> {
    let mut terms: Vec<String> = selector
        .match_labels
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    for requirement in &selector.match_expressions {
        let (key, values) = (&requirement.key, requirement.values.join(","));
        terms.push(match requirement.operator.as_str() {
            "In" => format!("{key} in ({values})"),
            "NotIn" => format!("{key} notin ({values})"),
            "Exists" => key.clone(),
            "DoesNotExist" => format!("!{key}"),
            operator => anyhow::bail!("unknown selector operator {operator}"),
        });
    }
    anyhow::ensure!(!terms.is_empty(), "the Deployment selector is empty");
    Ok(terms.join(","))
}

async fn kubectl(target: &Target, arguments: &[&str]) -> anyhow::Result<String> {
    crate::kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        arguments,
        None,
    )
    .await
}

async fn get_json<T: DeserializeOwned>(target: &Target, arguments: &[&str]) -> anyhow::Result<T> {
    let output = kubectl(target, arguments).await?;
    serde_json::from_str(&output).context("decode the kubectl output")
}

/// Read the host pods into `report`. A pod read earlier and absent now keeps
/// its last read.
async fn read(target: &Target, report: &mut Report, all_logs: bool) -> anyhow::Result<()> {
    let output = kubectl(
        target,
        &[
            "get",
            &format!("deployment/{}", report.deployment),
            "-o",
            "json",
            "--ignore-not-found",
        ],
    )
    .await?;
    anyhow::ensure!(
        !output.is_empty(),
        "Deployment {} does not exist",
        report.deployment
    );
    let deployment: Deployment =
        serde_json::from_str(&output).context("decode the host Deployment")?;
    let selector = selector(&deployment.spec.selector)?;
    let pods: List<Pod> = get_json(target, &["get", "pods", "-l", &selector, "-o", "json"]).await?;
    for pod in pods.items {
        let name = pod.metadata.name.clone();
        let events = get_json::<List<Event>>(
            target,
            &[
                "get",
                "events",
                "--field-selector",
                &format!("involvedObject.name={name},type=Warning"),
                "-o",
                "json",
            ],
        )
        .await
        .map(|events| events.items)
        .map_err(|error| format!("{error:#}"));
        let mut logs = report
            .pods
            .remove(&name)
            .map(|previous| previous.logs)
            .unwrap_or_default();
        let pod_ready = ready(&pod);
        for container in &pod.status.container_statuses {
            let last = logs.get(&container.name).map(|logs| logs.restarts);
            if fetch_logs(container, pod_ready, last, all_logs) {
                logs.insert(
                    container.name.clone(),
                    container_logs(target, &name, container).await,
                );
            }
        }
        report.pods.insert(name, PodReport { pod, events, logs });
    }
    Ok(())
}

async fn container_logs(target: &Target, pod: &str, container: &ContainerStatus) -> ContainerLogs {
    let mut reads = Vec::new();
    let mut previous = vec![false];
    if container.restart_count > 0 {
        previous.insert(0, true);
    }
    for previous in previous {
        let mut arguments = vec!["logs", pod, "-c", &container.name, LOG_LINES];
        if previous {
            arguments.push("--previous");
        }
        let read = kubectl(target, &arguments)
            .await
            .map_err(|error| format!("{error:#}"));
        reads.push((previous, read));
    }
    ContainerLogs {
        restarts: container.restart_count,
        reads,
    }
}

/// Read the report of the host pods of `release_name` once, with the logs of
/// every container.
pub async fn collect(target: &Target, release_name: &str) -> Report {
    let mut report = Report::new(release_name);
    if let Err(error) = read(target, &mut report, true).await {
        report.failure = Some(format!("{error:#}"));
    }
    report
}

/// Reads the host pod report every 10 s until it is finished or dropped.
#[derive(Debug)]
pub struct Poller {
    report: Arc<Mutex<Report>>,
    task: tokio::task::JoinHandle<()>,
}

impl Poller {
    /// Start reading the host pods of `release_name`.
    pub fn start(target: &Target, release_name: &str) -> Self {
        let report = Arc::new(Mutex::new(Report::new(release_name)));
        let shared = Arc::clone(&report);
        let target = target.clone();
        let mut current = Report::new(release_name);
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(POLL).await;
                current.failure = read(&target, &mut current, false)
                    .await
                    .err()
                    .map(|error| format!("{error:#}"));
                *shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = current.clone();
            }
        });
        Self { report, task }
    }

    /// Stop reading, and return the last complete read.
    pub fn finish(self) -> Report {
        self.task.abort();
        self.report
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Keep `error` first and attach `report` after it.
pub fn attach(error: &anyhow::Error, report: &Report) -> anyhow::Error {
    anyhow::Error::msg(report.to_string()).context(format!("{error:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pod(json: serde_json::Value) -> Pod {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn the_selector_is_the_deployment_spec_selector() {
        let selector_json: LabelSelector = serde_json::from_value(serde_json::json!({
            "matchLabels": {"app": "host", "group": "r-prod"},
            "matchExpressions": [
                {"key": "tier", "operator": "In", "values": ["a", "b"]},
                {"key": "old", "operator": "DoesNotExist"}
            ]
        }))
        .unwrap();
        assert_eq!(
            selector(&selector_json).unwrap(),
            "app=host,group=r-prod,tier in (a,b),!old"
        );
        assert!(selector(&LabelSelector::default()).is_err());
    }

    #[test]
    fn logs_are_read_for_a_failing_pod_only_when_its_restarts_change() {
        let container: ContainerStatus = serde_json::from_value(serde_json::json!({
            "name": "host", "restartCount": 2
        }))
        .unwrap();
        assert!(fetch_logs(&container, true, None, false));
        assert!(!fetch_logs(&container, false, Some(2), false));
        assert!(fetch_logs(&container, false, Some(1), false));
        assert!(fetch_logs(&container, true, Some(2), true));
        let fresh: ContainerStatus =
            serde_json::from_value(serde_json::json!({"name": "host"})).unwrap();
        assert!(!fetch_logs(&fresh, true, None, false));
        assert!(fetch_logs(&fresh, false, None, false));
    }

    #[test]
    fn the_report_follows_the_original_error() {
        let crashing = pod(serde_json::json!({
            "metadata": {"name": "hostgroup-r-1"},
            "status": {
                "phase": "Running",
                "conditions": [{"type": "Ready", "status": "False"}],
                "containerStatuses": [{
                    "name": "host",
                    "restartCount": 3,
                    "state": {"waiting": {"reason": "CrashLoopBackOff"}},
                    "lastState": {"terminated": {"reason": "Error", "exitCode": 1}}
                }]
            }
        }));
        let mut report = Report::new("r");
        report.pods.insert(
            "hostgroup-r-1".into(),
            PodReport {
                pod: crashing,
                events: Ok(vec![Event {
                    reason: Some("BackOff".into()),
                    message: Some("Back-off restarting failed container".into()),
                    count: Some(4),
                }]),
                logs: [(
                    "host".into(),
                    ContainerLogs {
                        restarts: 3,
                        reads: vec![
                            (
                                true,
                                Ok("error: component-artifact-manifest-unavailable".into()),
                            ),
                            (false, Err("kubectl logs exited 1".into())),
                        ],
                    },
                )]
                .into(),
            },
        );
        report.failure = Some("kubectl get exited 1".into());
        let error = attach(&anyhow::anyhow!("helm upgrade exited 1"), &report);
        let text = format!("{error:#}");
        assert!(text.starts_with("helm upgrade exited 1: host pods of Deployment hostgroup-r:"));
        for line in [
            "pod hostgroup-r-1: phase Running, not Ready",
            "container host: waiting CrashLoopBackOff; restarts 3; last terminated Error, exit code 1",
            "warning BackOff: Back-off restarting failed container (x4)",
            "log of container host (previous):\n    error: component-artifact-manifest-unavailable",
            "the log of container host could not be read: kubectl logs exited 1",
            "the host pod report could not be read: kubectl get exited 1",
        ] {
            assert!(text.contains(line), "{line} is missing from {text}");
        }
    }
}

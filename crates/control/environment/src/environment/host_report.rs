//! The host pod report that a failed step 6 or step 7 of `apply` carries
//! (docs/plan/alignment.md F2).
//!
//! Helm runs with `--rollback-on-failure`, so the failing pods are gone when
//! `helm upgrade` returns. A [`Poller`] reads the report every 10 s while Helm
//! waits and keeps the last read of each pod. After step 7 fails the pods still
//! exist, and [`collect`] reads the report once. Reading the report is best
//! effort: a failure adds one line and never replaces the step's own error.

pub(super) mod plan;
pub use plan::attach;
use plan::{fetch_logs, ready, selector};

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use wamn_release_control::release_chart::Target;

const POLL: Duration = Duration::from_secs(10);
const LOG_LINES: &str = "--tail=50";

use plan::Deployment;

use plan::List;

use plan::Pod;

use plan::ContainerStatus;

use plan::Event;

use plan::ContainerLogs;

use plan::PodReport;

pub use plan::Report;

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

#[cfg(test)]
mod tests {
    use super::plan::LabelSelector;
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

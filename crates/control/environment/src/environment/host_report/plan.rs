use super::{BTreeMap, Deserialize, fmt};

pub(super) fn write_pod(f: &mut fmt::Formatter<'_>, report: &PodReport) -> fmt::Result {
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

pub(super) fn state(state: &ContainerState) -> String {
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

pub(super) fn ready(pod: &Pod) -> bool {
    pod.status
        .conditions
        .iter()
        .any(|condition| condition.type_ == "Ready" && condition.status == "True")
}

/// Whether a read fetches the logs of `container`: every read when `all`,
/// otherwise only for a pod that is not Ready or restarted, and only when the
/// restart count changed since the last read.
pub(super) fn fetch_logs(
    container: &ContainerStatus,
    pod_ready: bool,
    last: Option<u32>,
    all: bool,
) -> bool {
    all || ((!pod_ready || container.restart_count > 0) && last != Some(container.restart_count))
}

/// The `-l` argument for a Deployment's `spec.selector`.
pub(super) fn selector(selector: &LabelSelector) -> anyhow::Result<String> {
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

/// Keep `error` first and attach `report` after it.
pub fn attach(error: &anyhow::Error, report: &Report) -> anyhow::Error {
    anyhow::Error::msg(report.to_string()).context(format!("{error:#}"))
}

#[derive(Debug, Deserialize)]
pub(super) struct Deployment {
    pub(super) spec: DeploymentSpec,
}

#[derive(Debug, Deserialize)]
pub(super) struct DeploymentSpec {
    pub(super) selector: LabelSelector,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct LabelSelector {
    pub(super) match_labels: BTreeMap<String, String>,
    pub(super) match_expressions: Vec<Requirement>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Requirement {
    pub(super) key: String,
    pub(super) operator: String,
    #[serde(default)]
    pub(super) values: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct List<T> {
    pub(super) items: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Pod {
    pub(super) metadata: Metadata,
    #[serde(default)]
    pub(super) status: PodStatus,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Metadata {
    pub(super) name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct PodStatus {
    pub(super) phase: Option<String>,
    pub(super) conditions: Vec<Condition>,
    pub(super) container_statuses: Vec<ContainerStatus>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Condition {
    #[serde(rename = "type")]
    pub(super) type_: String,
    pub(super) status: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ContainerStatus {
    pub(super) name: String,
    #[serde(default)]
    pub(super) restart_count: u32,
    #[serde(default)]
    pub(super) state: ContainerState,
    #[serde(default)]
    pub(super) last_state: ContainerState,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(super) struct ContainerState {
    pub(super) waiting: Option<Waiting>,
    pub(super) running: Option<serde_json::Value>,
    pub(super) terminated: Option<Terminated>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Waiting {
    pub(super) reason: Option<String>,
    pub(super) message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Terminated {
    pub(super) reason: Option<String>,
    pub(super) exit_code: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Event {
    pub(super) reason: Option<String>,
    pub(super) message: Option<String>,
    pub(super) count: Option<u32>,
}

/// The logs of one container, read when its restart count was `restarts`.
#[derive(Debug, Clone)]
pub(super) struct ContainerLogs {
    pub(super) restarts: u32,
    /// `(previous, lines or the read failure)`.
    pub(super) reads: Vec<(bool, Result<String, String>)>,
}

#[derive(Debug, Clone)]
pub(super) struct PodReport {
    pub(super) pod: Pod,
    pub(super) events: Result<Vec<Event>, String>,
    pub(super) logs: BTreeMap<String, ContainerLogs>,
}

/// The last read of each host pod of one release.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub(super) deployment: String,
    pub(super) pods: BTreeMap<String, PodReport>,
    /// Why the last read failed, if it did.
    pub(super) failure: Option<String>,
}

impl Report {
    pub(super) fn new(release_name: &str) -> Self {
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

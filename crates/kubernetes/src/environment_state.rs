//! Observe application Deployments, workloads and Pods.
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
const RELEASE_DIGEST_ANNOTATION: &str = "wamn.release-digest";
/// Observe every application host in an environment through the same resource lists.
pub async fn observe_environment<F, Fut, E>(
    namespace: &str,
    coordinate: &str,
    mut get: F,
) -> Result<Observed, E>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<Value, E>>,
{
    let environment = coordinate
        .rsplit_once('/')
        .map_or(coordinate, |(environment, _)| environment);
    let mut deployments = get(format!("/apis/apps/v1/namespaces/{namespace}/deployments")).await?;
    let mut workloads = get(format!(
        "/apis/runtime.wasmcloud.dev/v1alpha1/namespaces/{namespace}/workloaddeployments"
    ))
    .await?;
    if let Some(items) = workloads["items"].as_array_mut() {
        items.retain(|item| {
            item["metadata"]["annotations"]["wamn.environment"].as_str() == Some(coordinate)
        });
    }
    let mut pods = get(format!("/api/v1/namespaces/{namespace}/pods")).await?;
    if let Some(items) = deployments["items"].as_array_mut() {
        items.retain(|item| {
            item["spec"]["template"]["metadata"]["annotations"]["wamn.environment"].as_str()
                == Some(environment)
        });
    }
    if let Some(items) = pods["items"].as_array_mut() {
        items.retain(|item| {
            item["metadata"]["annotations"]["wamn.environment"].as_str() == Some(environment)
        });
    }
    Ok(Observed {
        deployments,
        workloads,
        pods,
    })
}

/// The three lists one status read takes from the release namespace.
#[derive(Debug, Clone, PartialEq)]
pub struct Observed {
    /// The Deployments named `hostgroup-<application release name>` in the selected target.
    pub deployments: Value,
    /// Every WorkloadDeployment of the namespace.
    pub workloads: Value,
    /// The pods of the release's host group.
    pub pods: Value,
}

/// The current state of one application's release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentState {
    /// The host Deployment, or none.
    pub deployment: Option<DeploymentStatus>,
    /// Each WorkloadDeployment of the release, with its conditions.
    pub workloads: BTreeMap<String, BTreeMap<String, String>>,
    /// Release digest to the count of host pods that are not terminated.
    pub live: BTreeMap<String, usize>,
}

/// The digest and the rollout of the host Deployment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeploymentStatus {
    /// The digest of the pod template's `wamn.release-digest` annotation.
    pub manifest_digest: Option<String>,
    pub replicas: u64,
    pub updated_replicas: u64,
    pub ready_replicas: u64,
    pub available_replicas: u64,
}

/// Read the three lists of `release_name` in `namespace` with `get`, which
/// sends one GET of an API path and answers the decoded body.
///
/// # Errors
///
/// The first error of `get`.
pub async fn observe<F, Fut, E>(
    namespace: &str,
    release_name: &str,
    mut get: F,
) -> Result<Observed, E>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<Value, E>>,
{
    let deployments = get(list_path(
        &format!("/apis/apps/v1/namespaces/{namespace}/deployments"),
        "fieldSelector",
        &format!("metadata.name=hostgroup-{release_name}"),
    ))
    .await?;
    let workloads = get(format!(
        "/apis/runtime.wasmcloud.dev/v1alpha1/namespaces/{namespace}/workloaddeployments"
    ))
    .await?;
    let pods = get(list_path(
        &format!("/api/v1/namespaces/{namespace}/pods"),
        "labelSelector",
        &format!("wasmcloud.com/hostgroup={release_name}"),
    ))
    .await?;
    Ok(Observed {
        deployments,
        workloads,
        pods,
    })
}

fn list_path(path: &str, key: &str, selector: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair(key, selector)
        .finish();
    format!("{path}?{query}")
}

/// The state of `release_name` in what [`observe`] read.
pub fn environment_state(observed: &Observed, release_name: &str) -> EnvironmentState {
    let deployment_name = format!("hostgroup-{release_name}");
    EnvironmentState {
        deployment: observed.deployments["items"]
            .as_array()
            .and_then(|items| {
                items.iter().find(|deployment| {
                    deployment["metadata"]["name"].as_str() == Some(deployment_name.as_str())
                })
            })
            .map(deployment_status),
        workloads: workload_conditions(&observed.workloads, release_name),
        live: BTreeMap::new(),
    }
}

/// The digest of the pod template and the rollout of one Deployment.
pub fn deployment_status(deployment: &Value) -> DeploymentStatus {
    let count = |field: &str| deployment["status"][field].as_u64().unwrap_or(0);
    DeploymentStatus {
        manifest_digest: deployment["spec"]["template"]["metadata"]["annotations"]
            [RELEASE_DIGEST_ANNOTATION]
            .as_str()
            .map(str::to_owned),
        replicas: count("replicas"),
        updated_replicas: count("updatedReplicas"),
        ready_replicas: count("readyReplicas"),
        available_replicas: count("availableReplicas"),
    }
}

/// The conditions of each WorkloadDeployment named `<release name>-<role>`.
pub fn workload_conditions(
    workloads: &Value,
    release_name: &str,
) -> BTreeMap<String, BTreeMap<String, String>> {
    let prefix = format!("{release_name}-");
    workloads["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|workload| {
            let name = workload["metadata"]["name"].as_str()?;
            name.starts_with(&prefix).then(|| {
                let conditions = workload["status"]["conditions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|condition| {
                        Some((
                            condition["type"].as_str()?.to_owned(),
                            condition["status"].as_str()?.to_owned(),
                        ))
                    })
                    .collect();
                (name.to_owned(), conditions)
            })
        })
        .collect()
}

/// One application's resident release and the unfinished runs that require it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseResidency {
    pub application: String,
    pub manifest_digest: String,
    pub loaded: bool,
    pub runs: Vec<String>,
}

/// A project remains observable when another project's initialization fails.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectResidency {
    pub releases: Vec<ReleaseResidency>,
    pub error: Option<String>,
}

/// Project names partition the environment host's residency report.
pub type Residency = BTreeMap<String, ProjectResidency>;

/// Read every live environment host through the Kubernetes pod proxy.
/// An unreadable host leaves residency unknown instead of implying an empty host.
pub async fn residency<F, Fut>(
    namespace: &str,
    org: &str,
    env: &str,
    mut get: F,
) -> anyhow::Result<Vec<Residency>>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = anyhow::Result<Value>>,
{
    use anyhow::Context as _;
    let name = wamn_registry::name::environment_host_name(org, env)?;
    let pods = get(list_path(
        &format!("/api/v1/namespaces/{namespace}/pods"),
        "labelSelector",
        &format!("wasmcloud.com/hostgroup={name}"),
    ))
    .await?;
    let mut reports = Vec::new();
    for pod in pods["items"].as_array().context("pod list has no items")? {
        if matches!(
            pod["status"]["phase"].as_str(),
            Some("Succeeded" | "Failed")
        ) {
            continue;
        }
        let pod = pod["metadata"]["name"]
            .as_str()
            .context("host pod has no name")?;
        reports.push(serde_json::from_value(
            get(format!(
                "/api/v1/namespaces/{namespace}/pods/{pod}:8082/proxy/_wamn/residency"
            ))
            .await?,
        )?);
    }
    Ok(reports)
}

/// Merge one project's reports while preserving application ownership and run pins.
pub fn project_residency(
    reports: Vec<Residency>,
    project: &str,
) -> anyhow::Result<Vec<ReleaseResidency>> {
    let mut releases: BTreeMap<(String, String), ReleaseResidency> = BTreeMap::new();
    for mut report in reports {
        let state = report
            .remove(project)
            .ok_or_else(|| anyhow::anyhow!("host has not prepared project {project}"))?;
        if let Some(error) = state.error {
            anyhow::bail!("project {project}: {error}");
        }
        for release in state.releases {
            use std::collections::btree_map::Entry;
            match releases.entry((release.application.clone(), release.manifest_digest.clone())) {
                Entry::Vacant(entry) => {
                    entry.insert(release);
                }
                Entry::Occupied(mut entry) => {
                    let row = entry.get_mut();
                    row.loaded |= release.loaded;
                    row.runs.extend(release.runs);
                }
            }
        }
    }
    for row in releases.values_mut() {
        row.runs.sort();
        row.runs.dedup();
    }
    Ok(releases.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn residency_keeps_projects_applications_and_pinned_releases_independent() {
        let row = |application: &str, loaded, run: &str| ReleaseResidency {
            application: application.into(),
            manifest_digest: wamn_digest_for_test(),
            loaded,
            runs: vec![run.into()],
        };
        let report = BTreeMap::from([
            (
                "orders".into(),
                ProjectResidency {
                    releases: vec![row("first", true, "run-a"), row("second", false, "run-b")],
                    error: None,
                },
            ),
            (
                "billing".into(),
                ProjectResidency {
                    releases: vec![],
                    error: Some("unavailable".into()),
                },
            ),
        ]);
        let releases = project_residency(vec![report.clone(), report.clone()], "orders").unwrap();
        for row in &report["orders"].releases {
            let actual = releases
                .iter()
                .find(|actual| {
                    actual.application == row.application
                        && actual.manifest_digest == row.manifest_digest
                })
                .unwrap();
            assert_eq!(actual.loaded, row.loaded);
            assert_eq!(actual.runs, row.runs);
        }
        assert!(project_residency(vec![report.clone()], "billing").is_err());
        assert!(project_residency(vec![report], "foreign").is_err());
    }
    fn wamn_digest_for_test() -> String {
        format!("sha256:{}", "a".repeat(64))
    }
}

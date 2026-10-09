//! The status operation (docs/plan/platform-deploy.md §11.4, owner ruling of
//! 2026-10-09 on `wamn-snz0.6`).
//!
//! Current state only, read and never written: the newest successful Helm
//! revision and its digest, the digest the host Deployment's pod template
//! carries and its rollout, the conditions of the release's
//! WorkloadDeployments, the live set, and the drain: the pods that are
//! terminating and the runs pinned to a release with no live pod. No history
//! of checks, no Secret, no verb. Who applied is in the Helm revision, read
//! with `helm history`.

use std::collections::BTreeMap;

use anyhow::Context as _;
use serde::Serialize;
use serde_json::Value;
use wamn_control_registry::Triple;

use super::Platform;
use super::analyse::{connect, live_set, read_pods, read_revision};
use super::drain::{Stranded, stranded};
use super::ensure::{kubectl, project_url};
use crate::release_chart;

/// The status of one environment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub release_name: String,
    /// The newest successful Helm revision, or none.
    pub revision: Option<RevisionStatus>,
    /// The host Deployment, or none.
    pub deployment: Option<DeploymentStatus>,
    /// Each WorkloadDeployment of the release, with its conditions.
    pub workloads: BTreeMap<String, BTreeMap<String, String>>,
    /// Release digest to the count of host pods that are not terminated.
    pub live: BTreeMap<String, usize>,
    /// Release digest to the count of host pods that are terminating.
    pub terminating: BTreeMap<String, usize>,
    /// Runs pinned to a release with no live pod.
    pub stranded: Vec<Stranded>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RevisionStatus {
    pub number: u32,
    pub chart: String,
    pub manifest_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeploymentStatus {
    /// The digest of the pod template's `wamn.release-digest` annotation.
    pub manifest_digest: Option<String>,
    pub replicas: u64,
    pub updated_replicas: u64,
    pub ready_replicas: u64,
    pub available_replicas: u64,
}

/// Read the status of one environment.
///
/// # Errors
///
/// When Helm, Kubernetes or the project database cannot be read.
pub async fn status(platform: &Platform, triple: &Triple) -> anyhow::Result<Status> {
    let (org, project, env) = (
        triple.org.as_str(),
        triple.project.as_str(),
        triple.env.as_str(),
    );
    let release_name = release_chart::release_name(org, project, env)?;
    let revision = read_revision(platform, &release_name)
        .await?
        .map(|revision| RevisionStatus {
            number: revision.number,
            chart: revision.chart.clone(),
            manifest_digest: revision.manifest_digest().map(str::to_owned),
        });
    let namespace = platform.target.namespace.as_str();
    let deployment = kubectl(
        platform,
        namespace,
        &[
            "get",
            "deployment",
            &format!("hostgroup-{release_name}"),
            "--ignore-not-found",
            "-o",
            "json",
        ],
        None,
    )
    .await?;
    let deployment = if deployment.is_empty() {
        None
    } else {
        Some(deployment_status(
            &serde_json::from_str(&deployment).context("decode the host Deployment")?,
        ))
    };
    let workloads: Value = serde_json::from_str(
        &kubectl(
            platform,
            namespace,
            &["get", "workloaddeployments", "-o", "json"],
            None,
        )
        .await?,
    )
    .context("decode the WorkloadDeployments")?;
    let pods = read_pods(platform, &release_name).await?;
    let live = live_set(&pods);
    let stranded = stranded_runs(platform, triple, &live).await?;
    Ok(Status {
        workloads: workload_conditions(&workloads, &release_name),
        terminating: terminating(&pods),
        release_name,
        revision,
        deployment,
        live,
        stranded,
    })
}

/// The runs pinned to a release with no live pod, or none when the
/// environment has no row or no database.
async fn stranded_runs(
    platform: &Platform,
    triple: &Triple,
    live: &BTreeMap<String, usize>,
) -> anyhow::Result<Vec<Stranded>> {
    let system = connect(&platform.system_database_url).await?;
    system
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let present = system
        .query_opt(
            "SELECT 1 FROM registry.project_envs WHERE org = $1 AND project = $2 AND env = $3",
            &[&triple.org, &triple.project, &triple.env.as_str()],
        )
        .await
        .context("read the environment row")?
        .is_some();
    if !present {
        return Ok(Vec::new());
    }
    let url = project_url(platform, triple).await?;
    if connect(&url).await.is_err() {
        return Ok(Vec::new());
    }
    let tenant = wamn_control_provision::project_env_tenant(
        &triple.org,
        &triple.project,
        triple.env.as_str(),
    );
    stranded(&url, &tenant, triple.env.as_str(), live).await
}

fn deployment_status(deployment: &Value) -> DeploymentStatus {
    let count = |field: &str| deployment["status"][field].as_u64().unwrap_or(0);
    DeploymentStatus {
        manifest_digest:
            deployment["spec"]["template"]["metadata"]["annotations"]["wamn.release-digest"]
                .as_str()
                .map(str::to_owned),
        replicas: count("replicas"),
        updated_replicas: count("updatedReplicas"),
        ready_replicas: count("readyReplicas"),
        available_replicas: count("availableReplicas"),
    }
}

/// The conditions of each WorkloadDeployment named `<release name>-<role>`.
fn workload_conditions(
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

/// Release digest to the count of host pods with a deletion timestamp that
/// are not terminated: the pods that are draining.
fn terminating(pods: &Value) -> BTreeMap<String, usize> {
    let mut terminating = BTreeMap::new();
    for pod in pods["items"].as_array().into_iter().flatten() {
        if pod["metadata"]["deletionTimestamp"].is_null()
            || matches!(
                pod["status"]["phase"].as_str(),
                Some("Succeeded" | "Failed")
            )
        {
            continue;
        }
        let digest = pod["metadata"]["annotations"]["wamn.release-digest"]
            .as_str()
            .unwrap_or("unlabelled");
        *terminating.entry(digest.to_owned()).or_insert(0) += 1;
    }
    terminating
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_deployment_reports_its_template_digest_and_rollout() {
        let deployment = json!({
            "spec": {"template": {"metadata": {"annotations": {"wamn.release-digest": "sha256:a"}}}},
            "status": {"replicas": 3, "updatedReplicas": 2, "readyReplicas": 2, "availableReplicas": 2}
        });
        assert_eq!(
            deployment_status(&deployment),
            DeploymentStatus {
                manifest_digest: Some("sha256:a".to_owned()),
                replicas: 3,
                updated_replicas: 2,
                ready_replicas: 2,
                available_replicas: 2,
            }
        );
    }

    #[test]
    fn only_the_releases_workloads_and_draining_pods_are_reported() {
        let workloads = json!({"items": [
            {"metadata": {"name": "r-prod-x-http"}, "status": {"conditions": [{"type": "Ready", "status": "True"}]}},
            {"metadata": {"name": "r-qa-y-http"}, "status": {"conditions": [{"type": "Ready", "status": "False"}]}}
        ]});
        let conditions = workload_conditions(&workloads, "r-prod-x");
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions["r-prod-x-http"]["Ready"], "True");
        let pods = json!({"items": [
            {"metadata": {"annotations": {"wamn.release-digest": "sha256:a"}, "deletionTimestamp": "t"}, "status": {"phase": "Running"}},
            {"metadata": {"annotations": {"wamn.release-digest": "sha256:b"}}, "status": {"phase": "Running"}}
        ]});
        assert_eq!(
            terminating(&pods),
            BTreeMap::from([("sha256:a".to_owned(), 1)])
        );
    }
}

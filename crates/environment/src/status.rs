//! Runtime status and the order of status changes across the two stores.

use std::{collections::BTreeMap, time::Duration};

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_application::application_config::{APPLICATION_CONFIG, ApplicationConfig};
use wamn_registry::{EnvName, EnvironmentStatus, OrgId, ProjectId, name};

use crate::environment::document::release_annotations;

/// Counts reported for the shared host rollout.
#[derive(Debug, Default)]
pub struct HostRollout {
    pub replicas: Option<i32>,
    pub updated_replicas: Option<i32>,
    pub ready_replicas: Option<i32>,
    pub available_replicas: Option<i32>,
}

/// A candidate deployment and the metadata that identifies its environment.
#[derive(Debug)]
pub struct HostDeployment<'a> {
    pub name: Option<&'a str>,
    pub environment: Option<&'a str>,
    pub rollout: HostRollout,
}

/// A deployed workload's metadata and JSON fields.
#[derive(Debug)]
pub struct Workload<'a> {
    pub name: Option<&'a str>,
    pub annotations: Option<&'a BTreeMap<String, String>>,
    pub data: &'a Value,
}

impl Workload<'_> {
    fn annotation(&self, key: &str) -> Option<&str> {
        self.annotations?.get(key).map(String::as_str)
    }
}

fn project_workloads<'a>(
    org: &OrgId,
    project: &ProjectId,
    env: &EnvName,
    workloads: impl IntoIterator<Item = Workload<'a>>,
) -> impl Iterator<Item = Workload<'a>> {
    let coordinate = name::host_status(org, project, env);
    workloads.into_iter().filter(move |workload| {
        workload.annotation("wamn.environment") == Some(coordinate.as_str())
    })
}

/// Report the shared host rollout and workloads for one project environment.
///
/// # Errors
/// Returns an error if a matching workload lacks valid release metadata.
pub fn runtime_status<'a>(
    org: &OrgId,
    project: &ProjectId,
    env: &EnvName,
    deployments: impl IntoIterator<Item = HostDeployment<'a>>,
    workloads: impl IntoIterator<Item = Workload<'a>>,
) -> anyhow::Result<Value> {
    let environment = format!("{org}/{env}");
    let group = name::environment_host_name(org, env);
    let deployment_name = name::host_deployment_name(&group);
    let rollout = deployments
        .into_iter()
        .find(|deployment| {
            deployment.name == Some(deployment_name.as_str())
                && deployment.environment == Some(environment.as_str())
        })
        .map_or_else(HostRollout::default, |deployment| deployment.rollout);
    let workloads = project_workloads(org, project, env, workloads)
        .map(|workload| {
            let name = workload.name.context("workload has no name")?;
            let release = release_annotations(workload.annotations)?;
            let conditions = workload.data["status"]["conditions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|row| json!({"type":row["type"],"status":row["status"]}))
                .collect::<Vec<_>>();
            Ok(json!({
                "name":name,"application":workload.annotation("wamn.application"),
                "package_digest":release.package_digest,"revision":release.revision,"conditions":conditions,
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(json!({
        "rollout":{
            "replicas":rollout.replicas.unwrap_or(0),
            "updated_replicas":rollout.updated_replicas.unwrap_or(0),
            "ready_replicas":rollout.ready_replicas.unwrap_or(0),
            "available_replicas":rollout.available_replicas.unwrap_or(0),
        },
        "workloads":workloads,
    }))
}

/// Read the declared application of each workload in one project environment.
///
/// # Errors
/// Returns an error if a matching workload lacks a valid application for this coordinate.
pub fn applications<'a>(
    org: &OrgId,
    project: &ProjectId,
    env: &EnvName,
    workloads: impl IntoIterator<Item = Workload<'a>>,
) -> anyhow::Result<Vec<ApplicationConfig>> {
    project_workloads(org, project, env, workloads)
        .map(|workload| {
            let components = workload.data["spec"]["template"]["spec"]["components"]
                .as_array()
                .context("application workload has no components")?;
            let config = components
                .iter()
                .find_map(|component| {
                    component["localResources"]["config"][APPLICATION_CONFIG].as_str()
                })
                .context("application workload has no wamn.application")?;
            let application: ApplicationConfig = serde_json::from_str(config)?;
            anyhow::ensure!(
                &application.org == org
                    && &application.project == project
                    && &application.environment == env,
                "application workload coordinate mismatch"
            );
            Ok(application)
        })
        .collect()
}

/// Update the system first on activation, and the project databases first on inactivation.
///
/// # Errors
/// Returns the first error from either update.
pub async fn change_status<E, System, Projects>(
    status: EnvironmentStatus,
    write_system: impl FnOnce() -> System,
    write_projects: impl FnOnce() -> Projects,
) -> Result<(), E>
where
    System: Future<Output = Result<(), E>>,
    Projects: Future<Output = Result<(), E>>,
{
    match status {
        EnvironmentStatus::Active => {
            write_system().await?;
            write_projects().await
        }
        EnvironmentStatus::Inactive => {
            write_projects().await?;
            write_system().await
        }
    }
}

/// Bound each count read so unavailable databases cannot prevent runtime status from answering.
const STATUS_COUNTS_TIMEOUT: Duration = Duration::from_secs(2);

async fn optional_status_count<T>(
    project: &ProjectId,
    env: &EnvName,
    source: &str,
    read: impl Future<Output = anyhow::Result<T>>,
) -> Option<T> {
    match tokio::time::timeout(STATUS_COUNTS_TIMEOUT, read).await {
        Ok(Ok(value)) => Some(value),
        Ok(Err(error)) => {
            tracing::warn!(%error, %project, %env, source, "environment status count read failed");
            None
        }
        Err(_) => {
            tracing::warn!(
                %project,
                %env,
                source,
                "environment status count read timed out"
            );
            None
        }
    }
}

/// Add independent count results to runtime status, using null for unavailable counts.
pub async fn status_with_counts(
    mut status: Value,
    project: &ProjectId,
    env: &EnvName,
    application: impl Future<Output = anyhow::Result<(i32, i32)>>,
    memberships: impl Future<Output = anyhow::Result<i32>>,
) -> Value {
    let (application, memberships) = tokio::join!(
        optional_status_count(project, env, "application", application),
        optional_status_count(project, env, "memberships", memberships),
    );
    status["users_with_role"] = json!(application.map(|counts| counts.0));
    status["roles"] = json!(application.map(|counts| counts.1));
    status["memberships"] = json!(memberships);
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn status_answers_when_count_reads_fail() {
        let status = json!({"digest": "sha256:live", "workloads": [], "live": [], "rollout": {"replicas": 1}});
        let answer = status_with_counts(
            status.clone(),
            &"billing".parse().unwrap(),
            &"dev".parse().unwrap(),
            async { anyhow::bail!("application unavailable") },
            async { anyhow::bail!("control unavailable") },
        )
        .await;
        for name in ["users_with_role", "roles", "memberships"] {
            assert!(answer[name].is_null());
        }
        for (name, value) in status.as_object().unwrap() {
            assert_eq!(answer[name], *value, "{name}");
        }
    }

    #[tokio::test]
    async fn status_answers_when_count_reads_time_out() {
        let status = json!({"digest": "sha256:live"});
        let answer = status_with_counts(
            status.clone(),
            &"billing".parse().unwrap(),
            &"dev".parse().unwrap(),
            std::future::pending(),
            std::future::pending(),
        )
        .await;
        for name in ["users_with_role", "roles", "memberships"] {
            assert!(answer[name].is_null());
        }
        for (name, value) in status.as_object().unwrap() {
            assert_eq!(answer[name], *value, "{name}");
        }
    }
}

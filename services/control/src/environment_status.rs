//! `environment.status` of the control serving root
//! (docs/architecture/execution.md#host-run-routes).
//!
//! Shared host rollout and project-owned workload conditions come from Kubernetes.
//! Workload annotations give the package digest and content revision.
//! Application history remains in Helm, independently of runtime placement.
use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_kubernetes::environment_state::observe_environment;
use wamn_kubernetes::kubernetes_api::KubernetesApi;

/// Read deployed status through the control service.
#[async_trait::async_trait]
pub trait StatusSource: Send + Sync + std::fmt::Debug {
    /// Read the current runtime status of the named environment.
    async fn read(
        &self,
        org: &wamn_registry::OrgId,
        project: &wamn_registry::ProjectId,
        env: &wamn_registry::EnvName,
    ) -> anyhow::Result<Value>;
}

/// Status from the release namespace through the pod's ServiceAccount.
#[derive(Debug)]
pub struct KubernetesSource;

#[async_trait::async_trait]
impl StatusSource for KubernetesSource {
    async fn read(
        &self,
        org: &wamn_registry::OrgId,
        project: &wamn_registry::ProjectId,
        env: &wamn_registry::EnvName,
    ) -> anyhow::Result<Value> {
        let api = KubernetesApi::in_cluster()?;
        let observed = observe_environment(
            &api.namespace,
            &wamn_registry::name::host_status(org, project, env),
            |path| api.get(path),
        )
        .await?;
        let group = wamn_registry::name::environment_host_name(org, env);
        let summary = wamn_kubernetes::environment_state::environment_state(&observed, &group);
        let mut workloads = Vec::new();
        for workload in observed.workloads["items"]
            .as_array()
            .context("workload list has no items")?
        {
            let name = workload["metadata"]["name"]
                .as_str()
                .context("workload has no name")?;
            let annotations = &workload["metadata"]["annotations"];
            let release =
                wamn_environment::environment::document::workload_release_annotations(workload)?;
            let conditions = workload["status"]["conditions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|row| json!({"type":row["type"],"status":row["status"]}))
                .collect::<Vec<_>>();
            workloads.push(json!({
                "name":name,"application":annotations["wamn.application"],
                "package_digest":release.package_digest,"revision":release.revision,"conditions":conditions,
            }));
        }
        let count = |read: fn(&wamn_kubernetes::environment_state::DeploymentStatus) -> u64| {
            summary.deployment.as_ref().map_or(0, read)
        };
        Ok(json!({
            "rollout":{
                "replicas":count(|deployment| deployment.replicas),
                "updated_replicas":count(|deployment| deployment.updated_replicas),
                "ready_replicas":count(|deployment| deployment.ready_replicas),
                "available_replicas":count(|deployment| deployment.available_replicas),
            },
            "workloads":workloads,
        }))
    }
}

/// Read deployed application facts from the route components of this project environment.
pub async fn applications(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> anyhow::Result<Vec<wamn_catalog::application_config::ApplicationConfig>> {
    let api = KubernetesApi::in_cluster()?;
    let coordinate = wamn_registry::name::host_status(org, project, env);
    let workloads = api
        .get(format!(
            "/apis/runtime.wasmcloud.dev/v1alpha1/namespaces/{org}/workloaddeployments"
        ))
        .await?;
    let mut applications = Vec::new();
    for workload in workloads["items"]
        .as_array()
        .context("workload list has no items")?
    {
        if workload["metadata"]["annotations"]["wamn.environment"].as_str() != Some(&coordinate) {
            continue;
        }
        let components = workload["spec"]["template"]["spec"]["components"]
            .as_array()
            .context("application workload has no components")?;
        let config = components
            .iter()
            .find_map(|component| {
                component["localResources"]["config"]
                    [wamn_catalog::application_config::APPLICATION_CONFIG]
                    .as_str()
            })
            .context("application workload has no wamn.application")?;
        let application: wamn_catalog::application_config::ApplicationConfig =
            serde_json::from_str(config)?;
        anyhow::ensure!(
            &application.org == org
                && &application.project == project
                && &application.environment == env,
            "application workload coordinate mismatch"
        );
        applications.push(application);
    }
    Ok(applications)
}

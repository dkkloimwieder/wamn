//! `environment.status` of the control serving root
//! (docs/architecture/execution.md#host-run-routes).
//!
//! Shared host rollout and project-owned workload conditions come from Kubernetes.
//! Workload annotations give the package digest and content revision.
//! Application history remains in Helm, independently of runtime placement.
use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_kubernetes::kubernetes_api::KubernetesApi;
use wamn_registry::name;

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
        let namespace = &api.namespace;
        let deployments = api
            .get(format!("/apis/apps/v1/namespaces/{namespace}/deployments"))
            .await?;
        let workload_list = api
            .get(format!(
                "/apis/runtime.wasmcloud.dev/v1alpha1/namespaces/{namespace}/workloaddeployments"
            ))
            .await?;
        // Preserve the status read's resource checks.
        drop(
            api.get(format!("/api/v1/namespaces/{namespace}/pods"))
                .await?,
        );

        let coordinate = name::host_status(org, project, env);
        let environment = format!("{org}/{env}");
        let group = name::environment_host_name(org, env);
        let deployment_name = name::host_deployment_name(&group);
        let deployment = deployments["items"].as_array().and_then(|items| {
            items.iter().find(|deployment| {
                deployment["metadata"]["name"].as_str() == Some(deployment_name.as_str())
                    && deployment["spec"]["template"]["metadata"]["annotations"]["wamn.environment"]
                        .as_str()
                        == Some(environment.as_str())
            })
        });
        let mut workloads = Vec::new();
        for workload in workload_list["items"]
            .as_array()
            .context("workload list has no items")?
        {
            let annotations = &workload["metadata"]["annotations"];
            if annotations["wamn.environment"].as_str() != Some(coordinate.as_str()) {
                continue;
            }
            let name = workload["metadata"]["name"]
                .as_str()
                .context("workload has no name")?;
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
        let count = |field: &str| {
            deployment
                .and_then(|item| item["status"][field].as_u64())
                .unwrap_or(0)
        };
        Ok(json!({
            "rollout":{
                "replicas":count("replicas"),
                "updated_replicas":count("updatedReplicas"),
                "ready_replicas":count("readyReplicas"),
                "available_replicas":count("availableReplicas"),
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
) -> anyhow::Result<Vec<wamn_engine::application_config::ApplicationConfig>> {
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
                    [wamn_engine::application_config::APPLICATION_CONFIG]
                    .as_str()
            })
            .context("application workload has no wamn.application")?;
        let application: wamn_engine::application_config::ApplicationConfig =
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

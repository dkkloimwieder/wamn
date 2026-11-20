//! `environment.status` of the control serving root
//! (docs/architecture/execution.md#host-run-routes).
//!
//! Shared host rollout and project-owned workload conditions come from Kubernetes.
//! Workload annotations give the package digest and content revision.
//! Application history remains in Helm, independently of runtime placement.
use anyhow::Context as _;
use serde_json::{Value, json};
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Pod};
use kube::{Api, api::ListParams};
use wamn_kubernetes::client::{KubernetesApi, workloads};
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
        let params = ListParams::default();
        let deployments = Api::<Deployment>::namespaced(api.client(), namespace);
        let deployments = tokio::time::timeout(
            wamn_kubernetes::client::REQUEST_TIMEOUT,
            deployments.list(&params),
        )
        .await??;
        let workload_api = workloads(api.client(), namespace);
        let workload_list = tokio::time::timeout(
            wamn_kubernetes::client::REQUEST_TIMEOUT,
            workload_api.list(&params),
        )
        .await??;
        // Keep the Pod permission check in the status read.
        let pods = Api::<Pod>::namespaced(api.client(), namespace);
        drop(
            tokio::time::timeout(wamn_kubernetes::client::REQUEST_TIMEOUT, pods.list(&params))
                .await??,
        );

        let coordinate = name::host_status(org, project, env);
        let environment = format!("{org}/{env}");
        let group = name::environment_host_name(org, env);
        let deployment_name = name::host_deployment_name(&group);
        let deployment = deployments.items.iter().find(|deployment| {
            deployment.metadata.name.as_deref() == Some(deployment_name.as_str())
                && deployment
                    .spec
                    .as_ref()
                    .and_then(|spec| spec.template.metadata.as_ref())
                    .and_then(|meta| meta.annotations.as_ref())
                    .and_then(|values| values.get("wamn.environment"))
                    == Some(&environment)
        });
        let mut workloads = Vec::new();
        for workload in workload_list.items {
            let annotations = workload.metadata.annotations.as_ref();
            let annotation = |key| annotations.and_then(|values| values.get(key)).map(String::as_str);
            if annotation("wamn.environment") != Some(coordinate.as_str()) {
                continue;
            }
            let name = workload.metadata.name.as_deref().context("workload has no name")?;
            let release =
                wamn_environment::environment::document::workload_release_annotations(&workload)?;
            let conditions = workload.data["status"]["conditions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|row| json!({"type":row["type"],"status":row["status"]}))
                .collect::<Vec<_>>();
            workloads.push(json!({
                "name":name,"application":annotation("wamn.application"),
                "package_digest":release.package_digest,"revision":release.revision,"conditions":conditions,
            }));
        }
        let status = deployment.and_then(|item| item.status.as_ref());
        Ok(json!({
            "rollout":{
                "replicas":status.and_then(|s| s.replicas).unwrap_or(0),
                "updated_replicas":status.and_then(|s| s.updated_replicas).unwrap_or(0),
                "ready_replicas":status.and_then(|s| s.ready_replicas).unwrap_or(0),
                "available_replicas":status.and_then(|s| s.available_replicas).unwrap_or(0),
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
    let workloads = workloads(api.client(), org.as_str());
    let list = tokio::time::timeout(
        wamn_kubernetes::client::REQUEST_TIMEOUT,
        workloads.list(&ListParams::default()),
    )
    .await??;
    let mut applications = Vec::new();
    for workload in list.items {
        if workload
            .metadata
            .annotations
            .as_ref()
            .and_then(|values| values.get("wamn.environment"))
            != Some(&coordinate)
        {
            continue;
        }
        let components = workload.data["spec"]["template"]["spec"]["components"]
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

//! `environment.status` of the control serving root
//! (docs/architecture/execution.md#host-run-routes).
//!
//! Shared host rollout and project-owned workload conditions come from Kubernetes.
//! Workload annotations give the package digest and content revision.
//! Application history remains in Helm, independently of runtime placement.
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Pod};
use kube::{
    Api,
    api::{DynamicObject, ListParams},
};
use serde_json::Value;
use wamn_environment::status::{self, HostDeployment, HostRollout, Workload};
use wamn_kubernetes::client::{KubernetesApi, workloads};

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

        let deployments = deployments.items.iter().map(|deployment| {
            let environment = deployment
                .spec
                .as_ref()
                .and_then(|spec| spec.template.metadata.as_ref())
                .and_then(|meta| meta.annotations.as_ref())
                .and_then(|values| values.get("wamn.environment"))
                .map(String::as_str);
            let rollout = deployment
                .status
                .as_ref()
                .map_or_else(HostRollout::default, |status| HostRollout {
                    replicas: status.replicas,
                    updated_replicas: status.updated_replicas,
                    ready_replicas: status.ready_replicas,
                    available_replicas: status.available_replicas,
                });
            HostDeployment {
                name: deployment.metadata.name.as_deref(),
                environment,
                rollout,
            }
        });
        status::runtime_status(
            org,
            project,
            env,
            deployments,
            workload_list.items.iter().map(workload_data),
        )
    }
}

/// Read deployed application facts from the route components of this project environment.
pub async fn applications(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
) -> anyhow::Result<Vec<wamn_application::application_config::ApplicationConfig>> {
    let api = KubernetesApi::in_cluster()?;
    let workloads = workloads(api.client(), org.as_str());
    let list = tokio::time::timeout(
        wamn_kubernetes::client::REQUEST_TIMEOUT,
        workloads.list(&ListParams::default()),
    )
    .await??;
    status::applications(org, project, env, list.items.iter().map(workload_data))
}

fn workload_data(item: &DynamicObject) -> Workload<'_> {
    Workload {
        name: item.metadata.name.as_deref(),
        annotations: item.metadata.annotations.as_ref(),
        data: &item.data,
    }
}

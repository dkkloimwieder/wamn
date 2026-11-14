//! `environment.status` of the control serving root
//! (docs/architecture/execution.md#host-run-routes).
//!
//! Shared host rollout and project-owned workload conditions come from Kubernetes.
//! The same pod-proxy reader as lifecycle status supplies loaded releases and run pins.
//! Application history remains in Helm, independently of runtime placement.
use serde_json::{Value, json};
use wamn_kubernetes::environment_state::{EnvironmentState, observe_environment};
use wamn_kubernetes::kubernetes_api::KubernetesApi;

/// Read deployed status through the control service.
#[async_trait::async_trait]
pub trait StatusSource: Send + Sync + std::fmt::Debug {
    /// Read the current runtime status of the named environment.
    async fn read(&self, org: &str, project: &str, env: &str) -> anyhow::Result<Value>;
}

/// Status from the release namespace through the pod's ServiceAccount.
#[derive(Debug)]
pub struct KubernetesSource;

#[async_trait::async_trait]
impl StatusSource for KubernetesSource {
    async fn read(&self, org: &str, project: &str, env: &str) -> anyhow::Result<Value> {
        let api = KubernetesApi::in_cluster()?;
        let observed = observe_environment(
            &api.namespace,
            &wamn_registry::name::host_status(org, project, env),
            |path| api.get(path),
        )
        .await?;
        let group = wamn_registry::name::environment_host_name(org, env)?;
        let mut summary = wamn_kubernetes::environment_state::environment_state(&observed, &group);
        summary.workloads = observed.workloads["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|workload| workload["metadata"]["name"].as_str())
            .flat_map(|name| {
                wamn_kubernetes::environment_state::workload_conditions(
                    &observed.workloads,
                    name.strip_suffix("-http").unwrap_or(name),
                )
            })
            .collect();
        let reports =
            wamn_kubernetes::environment_state::residency(&api.namespace, org, env, |path| async {
                api.get(path).await.map_err(anyhow::Error::from)
            })
            .await?;
        summary.live.clear();
        for report in &reports {
            let digests: std::collections::BTreeSet<_> = report
                .get(project)
                .into_iter()
                .flat_map(|state| &state.releases)
                .filter(|release| release.loaded)
                .map(|release| release.manifest_digest.as_str())
                .collect();
            for digest in digests {
                *summary.live.entry(digest.to_owned()).or_default() += 1;
            }
        }
        let residency = wamn_kubernetes::environment_state::project_residency(reports, project)?;
        let mut value = status_value(&summary);
        value["residency"] = serde_json::to_value(residency)?;
        Ok(value)
    }
}

/// The result of `environment.status`: the digest, the rollout, the
/// workload conditions and the live set. With no host Deployment the digest
/// is null and the rollout counts are zero.
fn status_value(state: &EnvironmentState) -> Value {
    let deployment = state.deployment.as_ref();
    let count = |read: fn(&wamn_kubernetes::environment_state::DeploymentStatus) -> u64| {
        deployment.map_or(0, read)
    };
    json!({
        "digest": deployment.and_then(|deployment| deployment.manifest_digest.clone()),
        "rollout": {
            "replicas": count(|deployment| deployment.replicas),
            "updated_replicas": count(|deployment| deployment.updated_replicas),
            "ready_replicas": count(|deployment| deployment.ready_replicas),
            "available_replicas": count(|deployment| deployment.available_replicas),
        },
        "workloads": state
            .workloads
            .iter()
            .map(|(name, conditions)| json!({
                "name": name,
                "conditions": conditions
                    .iter()
                    .map(|(type_, status)| json!({ "type": type_, "status": status }))
                    .collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
        "live": state
            .live
            .iter()
            .map(|(digest, pods)| json!({ "digest": digest, "pods": pods }))
            .collect::<Vec<_>>(),
    })
}

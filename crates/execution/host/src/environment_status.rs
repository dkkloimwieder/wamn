//! `environment.status` of the control serving root
//! (docs/architecture/execution.md#host-run-routes).
//!
//! The current state of one environment, read with the control host's
//! ServiceAccount from the release namespace `hosts`: the digest of
//! the host Deployment's pod template, its rollout, the conditions of the
//! release's WorkloadDeployments and the live set. The reader is
//! [`wamn_kubernetes::environment_state`], the one `wamn-ctl env
//! status` uses. No Helm revision or history; the role in
//! `deploy/platform/control-status-rbac.yaml` grants only get, list and watch
//! on pods, deployments and workloaddeployments. Credential access grants
//! create, get and update on Secrets.
use serde_json::{Value, json};
use wamn_kubernetes::environment_state::{
    DeploymentStatus, EnvironmentState, application_states, observe_environment,
};
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
        let applications = application_states(&observed);
        let mut summary = EnvironmentState {
            deployment: None,
            workloads: std::collections::BTreeMap::new(),
            live: std::collections::BTreeMap::new(),
        };
        let single_application = applications.len() == 1;
        for state in applications.into_values() {
            if let Some(deployment) = state.deployment {
                let total = summary.deployment.get_or_insert(DeploymentStatus {
                    manifest_digest: None,
                    replicas: 0,
                    updated_replicas: 0,
                    ready_replicas: 0,
                    available_replicas: 0,
                });
                total.replicas += deployment.replicas;
                total.updated_replicas += deployment.updated_replicas;
                total.ready_replicas += deployment.ready_replicas;
                total.available_replicas += deployment.available_replicas;
                if single_application {
                    total.manifest_digest = deployment.manifest_digest;
                }
            }
            summary.workloads.extend(state.workloads);
            for (digest, count) in state.live {
                *summary.live.entry(digest).or_default() += count;
            }
        }
        Ok(status_value(&summary))
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

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_kubernetes::environment_state::{Observed, environment_state};

    /// The result from the lists the control host's ServiceAccount reads.
    #[test]
    fn the_result_has_the_digest_the_rollout_the_conditions_and_the_live_set() {
        let observed = Observed {
            deployments: json!({"kind": "DeploymentList", "items": [{
                "metadata": {"name": "hostgroup-r-prod-x", "namespace": "hosts"},
                "spec": {"template": {"metadata": {"labels": {"wasmcloud.com/hostgroup": "r-prod-x"}, "annotations": {"wamn.release-digest": "sha256:b"}}}},
                "status": {"replicas": 2, "updatedReplicas": 1, "readyReplicas": 2, "availableReplicas": 2}
            }]}),
            workloads: json!({"kind": "WorkloadDeploymentList", "items": [
                {"metadata": {"name": "r-prod-x-http"}, "status": {"conditions": [
                    {"type": "Ready", "status": "True", "reason": "Placed"}
                ]}},
                {"metadata": {"name": "r-prod-x-materializer"}, "status": {"conditions": [
                    {"type": "Ready", "status": "False"}
                ]}},
                {"metadata": {"name": "r-qa-y-http"}, "status": {"conditions": [{"type": "Ready", "status": "True"}]}}
            ]}),
            pods: json!({"kind": "PodList", "items": [
                {"metadata": {"labels": {"wasmcloud.com/hostgroup": "r-prod-x"}, "annotations": {"wamn.release-digest": "sha256:a"}}, "status": {"phase": "Running"}},
                {"metadata": {"labels": {"wasmcloud.com/hostgroup": "r-prod-x"}, "annotations": {"wamn.release-digest": "sha256:b"}}, "status": {"phase": "Running"}},
                {"metadata": {"labels": {"wasmcloud.com/hostgroup": "r-prod-x"}, "annotations": {"wamn.release-digest": "sha256:b"}}, "status": {"phase": "Failed"}}
            ]}),
        };
        assert_eq!(
            status_value(&environment_state(&observed, "r-prod-x")),
            json!({
                "digest": "sha256:b",
                "rollout": {
                    "replicas": 2,
                    "updated_replicas": 1,
                    "ready_replicas": 2,
                    "available_replicas": 2,
                },
                "workloads": [
                    {"name": "r-prod-x-http", "conditions": [{"type": "Ready", "status": "True"}]},
                    {"name": "r-prod-x-materializer", "conditions": [{"type": "Ready", "status": "False"}]},
                ],
                "live": [
                    {"digest": "sha256:a", "pods": 1},
                    {"digest": "sha256:b", "pods": 1},
                ],
            })
        );
    }

    #[test]
    fn an_environment_with_no_host_deployment_has_no_digest() {
        let empty = json!({"items": []});
        let observed = Observed {
            deployments: empty.clone(),
            workloads: empty.clone(),
            pods: empty,
        };
        assert_eq!(
            status_value(&environment_state(&observed, "r-prod-x")),
            json!({
                "digest": null,
                "rollout": {
                    "replicas": 0,
                    "updated_replicas": 0,
                    "ready_replicas": 0,
                    "available_replicas": 0,
                },
                "workloads": [],
                "live": [],
            })
        );
    }
}

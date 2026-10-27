//! The current state of one environment in its release namespace
//! (docs/architecture/execution.md#host-run-routes).
//!
//! The one reader of the host Deployment, the WorkloadDeployments and the host
//! pods of an environment's release: `wamn-ctl env status` reads them through
//! `kubectl get --raw`, and the control host's `environment.status` through its
//! read-only ServiceAccount. Both send the same three list requests and decode
//! them with the same functions. No Secret, no Helm revision, no history.
//!
//! This module holds no Kubernetes client: the caller passes the function that
//! sends one GET of an API path and answers its JSON.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::{ProvisionError, validate_project_env};

/// The longest slug in a release name: the env name cut to this length.
pub const SLUG_LEN: usize = 24;
/// The hex characters of the coordinate hash in a release name (80 bits).
pub const HASH_LEN: usize = 20;

/// The pod-template and pod annotation that carries the release digest.
const RELEASE_DIGEST_ANNOTATION: &str = "wamn.release-digest";

/// The Helm release name of an environment: `r-<slug>-<hash>`.
///
/// The slug is the env name cut to 24 characters. The hash is the first 20 hex
/// characters of SHA-256 over `org/project/env`. The name is at most 47
/// characters, inside Helm's bound of 53. It is also the environment's host group.
///
/// # Errors
///
/// When the coordinate is not valid.
pub fn release_name(org: &str, project: &str, env: &str) -> Result<String, ProvisionError> {
    validate_project_env(org, project, env)?;
    let hash = hex::encode(Sha256::digest(format!("{org}/{project}/{env}")));
    let slug: String = env.chars().take(SLUG_LEN).collect();
    Ok(format!("r-{slug}-{}", &hash[..HASH_LEN]))
}

/// The three lists one status read takes from the release namespace.
#[derive(Debug, Clone, PartialEq)]
pub struct Observed {
    /// The Deployments named `hostgroup-<release name>`: none or one.
    pub deployments: Value,
    /// Every WorkloadDeployment of the namespace.
    pub workloads: Value,
    /// The pods of the release's host group.
    pub pods: Value,
}

/// The current state of one environment.
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
    EnvironmentState {
        deployment: observed.deployments["items"]
            .as_array()
            .and_then(|items| items.first())
            .map(deployment_status),
        workloads: workload_conditions(&observed.workloads, release_name),
        live: live_set(&observed.pods),
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

/// Release digest to the count of host pods that are not terminated: the
/// live set.
pub fn live_set(pods: &Value) -> BTreeMap<String, usize> {
    let mut live = BTreeMap::new();
    for pod in pods["items"].as_array().into_iter().flatten() {
        if matches!(
            pod["status"]["phase"].as_str(),
            Some("Succeeded" | "Failed")
        ) {
            continue;
        }
        *live.entry(release_digest(pod).to_owned()).or_insert(0) += 1;
    }
    live
}

/// The release digest a host pod carries, or `unlabelled`.
pub fn release_digest(pod: &Value) -> &str {
    pod["metadata"]["annotations"][RELEASE_DIGEST_ANNOTATION]
        .as_str()
        .unwrap_or("unlabelled")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_three_lists_are_named_by_the_release() {
        let mut paths = Vec::new();
        let observed = block_on(observe("hosts", "r-prod-x", |path| {
            paths.push(path);
            std::future::ready(Ok::<_, ()>(json!({"items": []})))
        }))
        .expect("the reads answer");
        assert_eq!(
            paths,
            [
                "/apis/apps/v1/namespaces/hosts/deployments?fieldSelector=metadata.name%3Dhostgroup-r-prod-x",
                "/apis/runtime.wasmcloud.dev/v1alpha1/namespaces/hosts/workloaddeployments",
                "/api/v1/namespaces/hosts/pods?labelSelector=wasmcloud.com%2Fhostgroup%3Dr-prod-x",
            ]
        );
        assert_eq!(
            environment_state(&observed, "r-prod-x"),
            EnvironmentState {
                deployment: None,
                workloads: BTreeMap::new(),
                live: BTreeMap::new(),
            }
        );
    }

    #[test]
    fn the_state_has_the_digest_the_rollout_the_conditions_and_the_live_set() {
        let observed = Observed {
            deployments: json!({"items": [{
                "metadata": {"name": "hostgroup-r-prod-x"},
                "spec": {"template": {"metadata": {"annotations": {"wamn.release-digest": "sha256:b"}}}},
                "status": {"replicas": 3, "updatedReplicas": 2, "readyReplicas": 2, "availableReplicas": 1}
            }]}),
            workloads: json!({"items": [
                {"metadata": {"name": "r-prod-x-http"}, "status": {"conditions": [
                    {"type": "Ready", "status": "True"}, {"type": "Placed", "status": "True"}
                ]}},
                {"metadata": {"name": "r-qa-y-http"}, "status": {"conditions": [{"type": "Ready", "status": "False"}]}}
            ]}),
            pods: json!({"items": [
                {"metadata": {"annotations": {"wamn.release-digest": "sha256:a"}}, "status": {"phase": "Running"}},
                {"metadata": {"annotations": {"wamn.release-digest": "sha256:b"}}, "status": {"phase": "Running"}},
                {"metadata": {"annotations": {"wamn.release-digest": "sha256:b"}}, "status": {"phase": "Pending"}},
                {"metadata": {"annotations": {"wamn.release-digest": "sha256:a"}}, "status": {"phase": "Succeeded"}},
                {"metadata": {}, "status": {"phase": "Running"}}
            ]}),
        };
        let state = environment_state(&observed, "r-prod-x");
        assert_eq!(
            state.deployment,
            Some(DeploymentStatus {
                manifest_digest: Some("sha256:b".to_owned()),
                replicas: 3,
                updated_replicas: 2,
                ready_replicas: 2,
                available_replicas: 1,
            })
        );
        assert_eq!(state.workloads.len(), 1);
        assert_eq!(state.workloads["r-prod-x-http"]["Ready"], "True");
        assert_eq!(state.workloads["r-prod-x-http"]["Placed"], "True");
        assert_eq!(
            state.live,
            BTreeMap::from([
                ("sha256:a".to_owned(), 1),
                ("sha256:b".to_owned(), 2),
                ("unlabelled".to_owned(), 1),
            ])
        );
    }

    #[test]
    fn release_name_matches_the_release_chart() {
        assert_eq!(
            release_name("acme", "wms", "dev").expect("valid coordinate"),
            "r-dev-2d9920a989cb26764e77"
        );
        assert!(release_name("acme", "wms", "Bad Env").is_err());
    }

    /// Poll a future whose reads are ready, with no runtime.
    fn block_on<T>(future: impl Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => panic!("a ready read is pending"),
        }
    }
}

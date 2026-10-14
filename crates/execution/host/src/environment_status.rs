//! `environment.status` of the control serving root
//! (docs/plan/platform-deploy.md §11.4).
//!
//! The current state of one environment, read with the control host's
//! read-only ServiceAccount from the release namespace `hosts`: the digest of
//! the host Deployment's pod template, its rollout, the conditions of the
//! release's WorkloadDeployments and the live set. The reader is
//! [`wamn_control_provision::environment_state`], the one `wamn-ctl env
//! status` uses. No Secret, no Helm revision, no history; the role in
//! `deploy/platform/control-status-rbac.yaml` grants only get, list and watch
//! on pods, deployments and workloaddeployments.

use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_control_provision::environment_state::{
    EnvironmentState, environment_state, observe, release_name,
};

/// The mounted token, CA and namespace of the pod's ServiceAccount.
const SERVICE_ACCOUNT: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
/// The release namespace of every environment.
const RELEASE_NAMESPACE: &str = "hosts";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Read the state of one environment of `org` and answer the route's result.
pub(crate) async fn read(org: &str, project: &str, env: &str) -> anyhow::Result<Value> {
    let release_name = release_name(org, project, env).context("derive the release name")?;
    let api = KubernetesApi::in_cluster()?;
    let observed = observe(RELEASE_NAMESPACE, &release_name, |path| api.get(path)).await?;
    Ok(status_value(&environment_state(&observed, &release_name)))
}

/// The result of `environment.status`: the digest, the rollout, the
/// workload conditions and the live set. With no host Deployment the digest
/// is null and the rollout counts are zero.
fn status_value(state: &EnvironmentState) -> Value {
    let deployment = state.deployment.as_ref();
    let count = |read: fn(&wamn_control_provision::environment_state::DeploymentStatus) -> u64| {
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

/// The Kubernetes API of the pod's cluster, with the pod's ServiceAccount.
struct KubernetesApi {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl KubernetesApi {
    /// The API server from the pod's environment, and the token and CA its
    /// ServiceAccount mounts. The token is read for each status read, so a
    /// rotated token is used.
    fn in_cluster() -> anyhow::Result<Self> {
        let host = std::env::var("KUBERNETES_SERVICE_HOST")
            .context("the control host does not run in a Kubernetes pod")?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".to_owned());
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        let token = std::fs::read_to_string(format!("{SERVICE_ACCOUNT}/token"))
            .context("read the ServiceAccount token")?;
        let ca = std::fs::read(format!("{SERVICE_ACCOUNT}/ca.crt"))
            .context("read the ServiceAccount CA")?;
        let roots =
            reqwest::Certificate::from_pem_bundle(&ca).context("decode the ServiceAccount CA")?;
        ensure!(!roots.is_empty(), "the ServiceAccount CA is empty");
        let http = reqwest::Client::builder()
            .tls_backend_rustls()
            .tls_certs_only(roots)
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("build the Kubernetes API client")?;
        Ok(Self {
            http,
            base: format!("https://{host}:{port}"),
            token: token.trim().to_owned(),
        })
    }

    /// One GET of an API path, decoded.
    async fn get(&self, path: String) -> anyhow::Result<Value> {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        let status = response.status();
        ensure!(status.is_success(), "GET {path} answered {status}");
        let body = response
            .bytes()
            .await
            .with_context(|| format!("read GET {path}"))?;
        serde_json::from_slice(&body).with_context(|| format!("decode GET {path}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_control_provision::environment_state::Observed;

    /// The result from the lists the control host's ServiceAccount reads.
    #[test]
    fn the_result_has_the_digest_the_rollout_the_conditions_and_the_live_set() {
        let observed = Observed {
            deployments: json!({"kind": "DeploymentList", "items": [{
                "metadata": {"name": "hostgroup-r-prod-x", "namespace": "hosts"},
                "spec": {"template": {"metadata": {"annotations": {"wamn.release-digest": "sha256:b"}}}},
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
                {"metadata": {"annotations": {"wamn.release-digest": "sha256:a"}}, "status": {"phase": "Running"}},
                {"metadata": {"annotations": {"wamn.release-digest": "sha256:b"}}, "status": {"phase": "Running"}},
                {"metadata": {"annotations": {"wamn.release-digest": "sha256:b"}}, "status": {"phase": "Failed"}}
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

    /// The role grants get, list and watch on pods, deployments and
    /// workloaddeployments in `hosts`, and nothing else, to the control
    /// host's ServiceAccount.
    #[test]
    fn the_status_role_reads_only_the_three_resources_in_hosts() {
        let manifest = include_str!("../../../../deploy/platform/control-status-rbac.yaml");
        let documents: Vec<Value> = serde_yaml::Deserializer::from_str(manifest)
            .map(|document| {
                serde::Deserialize::deserialize(document).expect("the manifest is YAML")
            })
            .collect();
        let role = documents
            .iter()
            .find(|document| document["kind"] == "Role")
            .expect("a Role");
        assert_eq!(role["metadata"]["namespace"], RELEASE_NAMESPACE);
        let verbs = json!(["get", "list", "watch"]);
        assert_eq!(
            role["rules"],
            json!([
                {"apiGroups": [""], "resources": ["pods"], "verbs": verbs},
                {"apiGroups": ["apps"], "resources": ["deployments"], "verbs": verbs},
                {"apiGroups": ["runtime.wasmcloud.dev"], "resources": ["workloaddeployments"], "verbs": verbs},
            ])
        );
        let binding = documents
            .iter()
            .find(|document| document["kind"] == "RoleBinding")
            .expect("a RoleBinding");
        assert_eq!(binding["metadata"]["namespace"], RELEASE_NAMESPACE);
        assert_eq!(binding["roleRef"]["kind"], "Role");
        assert_eq!(binding["roleRef"]["name"], role["metadata"]["name"]);
        assert_eq!(
            binding["subjects"],
            json!([{
                "kind": "ServiceAccount",
                "name": "wamn-host-runtime-operator-runtime",
                "namespace": RELEASE_NAMESPACE,
            }])
        );
        assert!(
            documents
                .iter()
                .all(|document| matches!(document["kind"].as_str(), Some("Role" | "RoleBinding"))),
            "the manifest holds only the Role and its RoleBinding"
        );
    }
}

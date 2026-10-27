//! `environment.status` of the control serving root
//! (docs/architecture/execution.md#host-run-routes).
//!
//! The current state of one environment, read with the control host's
//! read-only ServiceAccount from the release namespace `hosts`: the digest of
//! the host Deployment's pod template, its rollout, the conditions of the
//! release's WorkloadDeployments and the live set. The reader is
//! [`wamn_control_provision::environment_state`], the one `wamn-ctl env
//! status` uses. No Secret, no Helm revision, no history; the role in
//! `deploy/platform/control-status-rbac.yaml` grants only get, list and watch
//! on pods, deployments and workloaddeployments.
//! The local source reads the development loop's selected host over NATS.
//! The serving mode selects one source through [`StatusSource`].

use std::path::PathBuf;
use std::time::Duration;
use wash_runtime::washlet::{rpc_subject, types::v2};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_control_provision::environment_state::{
    DeploymentStatus, EnvironmentState, environment_state, observe, release_name,
};

/// The mounted token, CA and namespace of the pod's ServiceAccount.
const SERVICE_ACCOUNT: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The status reader selected by the control host's serving mode.
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
        let release_name = release_name(org, project, env).context("derive the release name")?;
        let api = KubernetesApi::in_cluster()?;
        let observed = observe(&api.namespace, &release_name, |path| api.get(path)).await?;
        Ok(status_value(&environment_state(&observed, &release_name)))
    }
}

/// Status from the runtime host selected by the owned local development loop.
#[derive(Debug)]
pub struct LocalSource {
    /// The loop's existing application endpoint record.
    pub endpoint: PathBuf,
    /// The control host's scheduler connection.
    pub nats: async_nats::Client,
}

#[derive(serde::Deserialize)]
struct LocalEndpoint {
    org: String,
    project: String,
    env: String,
    digest: String,
    host_id: String,
    workloads: Vec<String>,
}

#[async_trait::async_trait]
impl StatusSource for LocalSource {
    async fn read(&self, org: &str, project: &str, env: &str) -> anyhow::Result<Value> {
        let bytes = match std::fs::read(&self.endpoint) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(local_value(None, &[]));
            }
            Err(error) => return Err(error).context("read the local application endpoint"),
        };
        let endpoint: LocalEndpoint =
            serde_json::from_slice(&bytes).context("decode the local application endpoint")?;
        if (
            endpoint.org.as_str(),
            endpoint.project.as_str(),
            endpoint.env.as_str(),
        ) != (org, project, env)
        {
            return Ok(local_value(None, &[]));
        }
        let mut workloads = Vec::new();
        for name in &endpoint.workloads {
            let message = tokio::time::timeout(
                REQUEST_TIMEOUT,
                self.nats.request(
                    rpc_subject(&endpoint.host_id, "workload.status"),
                    serde_json::to_vec(&v2::WorkloadStatusRequest {
                        workload_id: name.clone(),
                    })?
                    .into(),
                ),
            )
            .await
            .context("local workload status timed out")??;
            let response: v2::WorkloadStatusResponse = serde_json::from_slice(&message.payload)?;
            let running = response
                .workload_status
                .is_some_and(|status| status.workload_state == v2::WorkloadState::Running as i32);
            workloads.push((name.clone(), running));
        }
        Ok(local_value(Some(&endpoint.digest), &workloads))
    }
}

// A responding local host is one live instance. It is ready only when all
// workloads selected by its activation are running.
fn local_value(digest: Option<&str>, workloads: &[(String, bool)]) -> Value {
    let present = u64::from(digest.is_some());
    let ready = u64::from(
        digest.is_some() && !workloads.is_empty() && workloads.iter().all(|(_, running)| *running),
    );
    status_value(&EnvironmentState {
        deployment: digest.map(|digest| DeploymentStatus {
            manifest_digest: Some(digest.to_owned()),
            replicas: present,
            updated_replicas: present,
            ready_replicas: ready,
            available_replicas: ready,
        }),
        workloads: workloads
            .iter()
            .map(|(name, running)| {
                (
                    name.clone(),
                    [(
                        "Ready".to_owned(),
                        if *running { "True" } else { "False" }.to_owned(),
                    )]
                    .into(),
                )
            })
            .collect(),
        live: digest
            .map(|digest| [(digest.to_owned(), 1)].into())
            .unwrap_or_default(),
    })
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
    /// The pod's own namespace, the release namespace of every environment.
    namespace: String,
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
        let namespace = std::fs::read_to_string(format!("{SERVICE_ACCOUNT}/namespace"))
            .context("read the ServiceAccount namespace")?;
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
            namespace: namespace.trim().to_owned(),
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

    #[test]
    fn local_source_reports_the_selected_release_and_observed_workloads() {
        let status = local_value(
            Some("sha256:local"),
            &[
                ("http".to_owned(), true),
                ("materializer".to_owned(), false),
            ],
        );
        assert_eq!(
            status,
            json!({
                "digest": "sha256:local",
                "rollout": {"replicas": 1, "updated_replicas": 1, "ready_replicas": 0, "available_replicas": 0},
                "workloads": [
                    {"name": "http", "conditions": [{"type": "Ready", "status": "True"}]},
                    {"name": "materializer", "conditions": [{"type": "Ready", "status": "False"}]}
                ],
                "live": [{"digest": "sha256:local", "pods": 1}]
            })
        );
        assert_eq!(
            local_value(Some("sha256:local"), &[("http".to_owned(), true)])["rollout"]["ready_replicas"],
            1
        );
        assert_eq!(local_value(None, &[])["digest"], Value::Null);
        assert_eq!(local_value(None, &[])["live"], json!([]));
    }

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
    /// workloaddeployments in the namespace it is applied to, and nothing
    /// else, to the control host's ServiceAccount. No object states a
    /// namespace: the install applies it with `-n <release namespace>`.
    #[test]
    fn the_status_role_reads_only_the_three_resources_in_the_release_namespace() {
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
        assert_eq!(role["metadata"].get("namespace"), None);
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
        assert_eq!(binding["metadata"].get("namespace"), None);
        assert_eq!(binding["roleRef"]["kind"], "Role");
        assert_eq!(binding["roleRef"]["name"], role["metadata"]["name"]);
        assert_eq!(
            binding["subjects"],
            json!([{
                "kind": "ServiceAccount",
                "name": "wamn-host",
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

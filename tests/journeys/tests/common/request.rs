//! Request path helpers: a second environment in one org, more API service callers,
//! host pods and Kubernetes waits.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kube::Api;
use kube::api::{ApiResource, DeleteParams, DynamicObject, GroupVersionKind, ListParams};
use serde_json::{Value, json};
use tokio::time::Instant;
use wamn_http_client::HttpClient;
use wamn_identity_protocol::PrincipalId;
use wamn_registry::{EnvName, Triple, name};
use wamn_test_infrastructure::scratch::ScratchRoot;

use super::{App, CALLER_ROLE, Env, Org, PROJECT, Routes, failure, run};

/// How long a host pod takes to start again, and its workloads to be placed.
pub const RESTART_DEADLINE: Duration = Duration::from_secs(300);

/// Poll `check` once a second until it answers a value, or fail at `deadline`.
pub async fn until<T, F, Fut>(deadline: Duration, what: &str, mut check: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<Option<T>>>,
{
    let end = Instant::now() + deadline;
    loop {
        if let Some(value) = check().await? {
            return Ok(value);
        }
        ensure!(Instant::now() < end, "{what} within {deadline:?}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// The path of a fixture application's source package.
pub fn fixture(app: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps")
        .join(app)
}

/// One more environment of an org. Runs `wamn env deactivate-project` after failure capture, if enabled.
pub struct OtherEnv<'a> {
    org: &'a Org,
    pub triple: Triple,
    pub file: std::path::PathBuf,
    _scratch: ScratchRoot,
}

impl<'a> OtherEnv<'a> {
    /// Write an environment file for `env` with the harness project and an application
    /// labelled `env`, and run `wamn env apply`. A label is unique in its org.
    pub async fn apply(org: &'a Org, env: &str, app: &str) -> anyhow::Result<Self> {
        let scratch = ScratchRoot::create()?;
        let file = scratch.path().join("env.yaml");
        std::fs::write(
            &file,
            serde_json::to_vec(&json!({
                "org": org.name,
                "env": env,
                "target": "cloud",
                "revocation_seconds": 60,
                "database": {"instances": 1, "storage": "1Gi", "cpu": "100m", "memory": "256Mi"},
                "projects": [{"name": PROJECT, "apps": {app: {"label": env}}}],
            }))?,
        )?;
        let env = Self {
            org,
            triple: Triple::new(org.name.clone(), env.parse()?, PROJECT.parse()?),
            file,
            _scratch: scratch,
        };
        failure::env(&env.triple);
        org.create_env(&env.triple.env).await?;
        let mut command = org.command();
        command.args(["env", "apply"]).arg(&env.file);
        run(command).await?;
        Ok(env)
    }
}

impl Drop for OtherEnv<'_> {
    fn drop(&mut self) {
        failure::delete_env(self.org, &self.triple);
    }
}

/// Run one prepared `wamn` child and return its output.
pub async fn output(command: std::process::Command) -> anyhow::Result<String> {
    run(command).await
}

/// Run one `wamn` child with the owner session and return its output.
pub async fn wamn(org: &Org, args: &[&str]) -> anyhow::Result<String> {
    let mut command = org.command();
    command.args(args);
    run(command).await
}

/// Create one more API service of the application's environment with the harness's
/// route caller role, and return its principal and routes that carry its token.
pub async fn caller(app: &App, name: &str) -> anyhow::Result<(PrincipalId, Routes)> {
    let env = &app.env;
    let scratch = ScratchRoot::create()?;
    let secret = scratch.path().join("caller.json");
    let triple = &env.triple;
    let mut command = env.org.command();
    command
        .args(["create-api-service", "--project", triple.project.as_str()])
        .args(["--env", triple.env.as_str(), "--name", name])
        .args(["--display-name", name, "--role", CALLER_ROLE])
        .args(["--lifetime-seconds", "900", "--emit-secret"])
        .arg(&secret);
    run(command).await?;
    let service: Value = serde_json::from_slice(&std::fs::read(&secret)?)?;
    let bearer = service["token"]
        .as_str()
        .context("the API service secret has no token")?
        .to_owned();
    let principal = service["principal_id"]
        .as_str()
        .context("the API service secret has no principal")?
        .parse()?;
    let routes = &app.routes;
    Ok((
        principal,
        Routes {
            endpoint: routes.endpoint.clone(),
            host: routes.host.clone(),
            bearer,
            client: HttpClient::builder()
                .timeout(Duration::from_secs(60))
                .build()?,
        },
    ))
}

/// GET a read route with one request element in its query, and return the status and body.
pub async fn read(routes: &Routes, path: &str, input: &Value) -> anyhow::Result<(u16, Value)> {
    let mut url = url::Url::parse(&format!("{}{path}", routes.endpoint))?;
    url.query_pairs_mut().extend_pairs(
        input
            .as_object()
            .context("read input")?
            .iter()
            .map(|(name, value)| (name, value.to_string())),
    );
    let response = routes
        .client
        .get(url.as_str())
        .header("Host", &routes.host)
        .bearer_auth(&routes.bearer)
        .send()
        .await?;
    Ok((response.status().as_u16(), response.json().await?))
}

/// The host pods of one org environment, selected by its host group Deployment.
pub async fn host_pods(client: &kube::Client, env: &Env) -> anyhow::Result<Vec<Pod>> {
    let org = &env.triple.org;
    host_pods_of(client, org, &env.triple.env).await
}

async fn host_pods_of(
    client: &kube::Client,
    org: &wamn_registry::OrgId,
    env: &EnvName,
) -> anyhow::Result<Vec<Pod>> {
    let deployment = name::host_deployment_name(&name::env_host_name(org, env));
    let selector = Api::<Deployment>::namespaced(client.clone(), org.as_str())
        .get(&deployment)
        .await?
        .spec
        .and_then(|spec| spec.selector.match_labels)
        .with_context(|| format!("the Deployment {deployment} has no selector"))?
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",");
    Ok(Api::<Pod>::namespaced(client.clone(), org.as_str())
        .list(&ListParams::default().labels(&selector))
        .await?
        .items)
}

fn ready(pod: &Pod) -> bool {
    pod.metadata.deletion_timestamp.is_none()
        && pod
            .status
            .as_ref()
            .and_then(|status| status.conditions.as_ref())
            .is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|condition| condition.type_ == "Ready" && condition.status == "True")
            })
}

/// Delete the one host pod of the environment, and return its UID. `grace` of zero
/// kills it without a drain.
pub async fn delete_host(
    client: &kube::Client,
    env: &Env,
    grace: Option<u32>,
) -> anyhow::Result<String> {
    let [pod] = <[_; 1]>::try_from(host_pods(client, env).await?)
        .map_err(|found| anyhow::anyhow!("{} host pods, not one", found.len()))?;
    let uid = pod.metadata.uid.context("host pod UID")?;
    let name = pod.metadata.name.context("host pod name")?;
    let params = DeleteParams {
        grace_period_seconds: grace,
        ..DeleteParams::default()
    };
    Api::<Pod>::namespaced(client.clone(), env.triple.org.as_str())
        .delete(&name, &params)
        .await?;
    Ok(uid)
}

/// Wait until a host pod other than `old` is Ready, and every WorkloadDeployment of the
/// org namespace reports Ready after that pod started.
pub async fn wait_host_restarted(
    client: &kube::Client,
    env: &Env,
    old: &str,
) -> anyhow::Result<()> {
    let started = until(RESTART_DEADLINE, "a new host pod is Ready", || async {
        Ok(host_pods(client, env)
            .await?
            .into_iter()
            .find(|pod| pod.metadata.uid.as_deref() != Some(old) && ready(pod))
            .and_then(|pod| pod.status?.start_time))
    })
    .await?;
    let resource = ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("runtime.wasmcloud.dev", "v1alpha1", "WorkloadDeployment"),
        "workloaddeployments",
    );
    let deployments =
        Api::<DynamicObject>::namespaced_with(client.clone(), env.triple.org.as_str(), &resource);
    until(
        RESTART_DEADLINE,
        "every WorkloadDeployment is Ready on the new host",
        || async {
            let items = deployments.list(&ListParams::default()).await?.items;
            if items.is_empty() {
                bail!("the org namespace has no WorkloadDeployment");
            }
            Ok(items
                .iter()
                .all(|deployment| {
                    deployment.data["status"]["conditions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|condition| {
                            condition["type"] == "Ready"
                                && condition["status"] == "True"
                                && serde_json::from_value::<Time>(
                                    condition["lastTransitionTime"].clone(),
                                )
                                .is_ok_and(|time| time >= started)
                        })
                })
                .then_some(()))
        },
    )
    .await
}

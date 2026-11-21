//! Host journey helpers: the environment's host pod, the stock runtime objects,
//! calls through web and identity, and a changed environment file.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Pod};
use kube::Api;
use kube::api::{ApiResource, DynamicObject, GroupVersionKind, ListParams};
use serde_json::{Value, json};
use tokio::time::Instant;
use wamn_http_client::HttpClient;

use super::{Environment, Org, PLATFORM, PortForward, identity_pods, run};

/// The fixture application that every host journey deploys.
pub const FIXTURE: &str = "platform_fixture";

/// The source package of [`FIXTURE`].
pub fn fixture() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps")
        .join(FIXTURE)
}

/// Run `check` once a second until it gives a value, and fail at `deadline`.
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
        ensure!(
            Instant::now() < end,
            "{what} did not happen within {} seconds",
            deadline.as_secs()
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Change the environment file with `edit`, and run `wamn env apply` on it.
pub async fn reapply(
    environment: &Environment,
    edit: impl FnOnce(&mut Value),
) -> anyhow::Result<()> {
    let mut file: Value = serde_json::from_slice(&std::fs::read(&environment.file)?)?;
    edit(&mut file);
    // A JSON document is a YAML document.
    std::fs::write(&environment.file, serde_json::to_vec(&file)?)?;
    let mut command = environment.org.command();
    command.args(["env", "apply"]).arg(&environment.file);
    run(command).await.map(drop)
}

/// Declare `application` with `label` in the environment's one project.
pub fn declare(file: &mut Value, application: &str, label: &str) {
    file["projects"][0]["applications"][application] = json!({"label": label});
}

/// The label that the environment file gives `application`.
fn label(environment: &Environment, application: &str) -> anyhow::Result<String> {
    let file: Value = serde_json::from_slice(&std::fs::read(&environment.file)?)?;
    file["projects"][0]["applications"][application]["label"]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("the environment file has no label for {application}"))
}

/// A stock wasmCloud runtime resource, such as `Host` or `Workload`.
pub fn runtime(
    client: &kube::Client,
    namespace: &str,
    kind: &str,
    plural: &str,
) -> Api<DynamicObject> {
    let resource = ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("runtime.wasmcloud.dev", "v1alpha1", kind),
        plural,
    );
    Api::namespaced_with(client.clone(), namespace, &resource)
}

/// The status of the condition `type_` of a stock runtime object.
pub fn condition<'a>(object: &'a DynamicObject, type_: &str) -> Option<&'a str> {
    object.data["status"]["conditions"]
        .as_array()?
        .iter()
        .find(|condition| condition["type"] == type_)?["status"]
        .as_str()
}

/// Whether a pod has the condition `Ready=True`.
pub fn ready(pod: &Pod) -> bool {
    pod.status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Ready" && condition.status == "True")
        })
}

/// The pods of the environment's host Deployment that are not being deleted.
pub async fn host_pods(environment: &Environment) -> anyhow::Result<Vec<Pod>> {
    let client = &environment.org.cluster.client;
    let triple = &environment.triple;
    let namespace = triple.org.as_str();
    let name = wamn_registry::name::host_deployment_name(
        &wamn_registry::name::environment_host_name(&triple.org, &triple.env),
    );
    let selector = Api::<Deployment>::namespaced(client.clone(), namespace)
        .get(&name)
        .await?
        .spec
        .and_then(|spec| spec.selector.match_labels)
        .with_context(|| format!("the host Deployment {name} has no selector"))?
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",");
    Ok(Api::<Pod>::namespaced(client.clone(), namespace)
        .list(&ListParams::default().labels(&selector))
        .await?
        .items
        .into_iter()
        .filter(|pod| pod.metadata.deletion_timestamp.is_none())
        .collect())
}

/// Wait until the environment has one host pod and it is Ready.
pub async fn ready_host_pod(environment: &Environment, deadline: Duration) -> anyhow::Result<Pod> {
    until(deadline, "one Ready host pod", || async {
        let pods = host_pods(environment).await?;
        Ok(match <[_; 1]>::try_from(pods) {
            Ok([pod]) if ready(&pod) => Some(pod),
            _ => None,
        })
    })
    .await
}

/// The stock Host object of the host pod `pod`. Every Host lives in the operator's
/// namespace, and names its pod by namespace and pod address.
pub async fn host_object(
    client: &kube::Client,
    pod: &Pod,
) -> anyhow::Result<Option<DynamicObject>> {
    let address = pod
        .status
        .as_ref()
        .and_then(|status| status.pod_ip.as_deref())
        .context("the host pod has no address")?;
    let namespace = pod
        .metadata
        .namespace
        .as_deref()
        .context("host pod namespace")?;
    Ok(runtime(client, PLATFORM, "Host", "hosts")
        .list(&ListParams::default())
        .await?
        .items
        .into_iter()
        .find(|host| host.data["environment"] == namespace && host.data["hostname"] == address))
}

/// The workloads that a Host object reports. Its status leaves out a count of zero.
pub fn workload_count(host: &DynamicObject) -> u64 {
    host.data["status"]["workloadCount"].as_u64().unwrap_or(0)
}

/// The stock runtime objects of `kind` that the release of `application` owns.
pub async fn application_objects(
    environment: &Environment,
    kind: &str,
    plural: &str,
    application: &str,
) -> anyhow::Result<Vec<DynamicObject>> {
    let client = &environment.org.cluster.client;
    Ok(
        runtime(client, environment.triple.org.as_str(), kind, plural)
            .list(&ListParams::default())
            .await?
            .items
            .into_iter()
            .filter(|object| {
                object
                    .metadata
                    .annotations
                    .as_ref()
                    .and_then(|annotations| annotations.get("wamn.application"))
                    .is_some_and(|name| name == application)
            })
            .collect(),
    )
}

/// Call the control route `operation` with the owner session, and return its value.
pub async fn control(org: &Org, operation: &str, value: Value) -> anyhow::Result<Value> {
    let (_, attachment) = wamn_routes::HostRouteSet::Control
        .attachments()
        .find(|(_, attachment)| attachment.route.operation == operation)
        .with_context(|| format!("no control route {operation}"))?;
    let path = attachment.definition["route"]["path"]
        .as_str()
        .context("control route path")?;
    let outputs = &org.cluster.outputs;
    let response = web()?
        .post(format!("http://{}{path}", outputs.web_address))
        .header("Host", format!("{}.{}", org.name, outputs.base_domain))
        .bearer_auth(&org.token)
        .json(&json!([{"request_id": operation, "value": value}]))
        .send()
        .await?;
    let status = response.status();
    let text = response.text().await?;
    ensure!(
        status == hyper::StatusCode::OK,
        "{operation} answered {status}: {text}"
    );
    let mut answer: Value = serde_json::from_str(&text)?;
    answer[0]
        .as_object_mut()
        .and_then(|element| element.remove("value"))
        .with_context(|| format!("{operation} refused: {text}"))
}

/// Read the application route `operation` of `application` with `token`, and return the status.
pub async fn application_get(
    environment: &Environment,
    application: &str,
    operation: &str,
    token: &str,
) -> anyhow::Result<hyper::StatusCode> {
    let (_, attachment) = wamn_routes::HostRouteSet::Application
        .attachments()
        .find(|(_, attachment)| attachment.route.operation == operation)
        .with_context(|| format!("no application route {operation}"))?;
    let path = attachment.definition["route"]["path"]
        .as_str()
        .context("application route path")?;
    let org = &environment.org;
    let outputs = &org.cluster.outputs;
    let host = wamn_registry::name::application_host(
        &label(environment, application)?,
        &org.name,
        &outputs.base_domain.parse()?,
    );
    Ok(web()?
        .get(format!("http://{}/api{path}", outputs.web_address))
        .header("Host", host)
        .bearer_auth(token)
        .send()
        .await?
        .status())
}

fn web() -> anyhow::Result<HttpClient> {
    HttpClient::builder()
        .timeout(Duration::from_secs(60))
        .build()
}

/// Invite a member without grants through control, enroll its password and open its session.
pub async fn member(
    org: &Org,
    email: &str,
) -> anyhow::Result<(wamn_identity_protocol::PrincipalId, String)> {
    control(
        org,
        "user.invite",
        json!({"email": email, "display_name": "Journey member"}),
    )
    .await?;
    let client = &org.cluster.client;
    let (principal, invitation) = super::invitation(client, email).await?;
    let token = super::session(client, &org.name, email, &principal, &invitation).await?;
    Ok((principal, token))
}

/// Post `body` to the identity route `path` with `bearer`, and return the status and body.
pub async fn identity_post(
    client: &kube::Client,
    path: &str,
    bearer: &str,
    body: &impl serde::Serialize,
) -> anyhow::Result<(hyper::StatusCode, String)> {
    let (service, pods) = identity_pods(client).await?;
    let pod = pods
        .into_iter()
        .find(ready)
        .and_then(|pod| pod.metadata.name)
        .context("no identity pod is ready")?;
    let ca = Api::<ConfigMap>::namespaced(client.clone(), "hosts")
        .get("identity-ca")
        .await?
        .data
        .and_then(|mut data| data.remove("ca.crt"))
        .context("the identity CA has no ca.crt")?;
    let forward = PortForward::start(Api::namespaced(client.clone(), PLATFORM), &pod, 8443).await?;
    // The serving certificate names the Service address.
    let host = format!(
        "{}.{PLATFORM}.svc.cluster.local",
        service.metadata.name.context("identity Service name")?
    );
    let response = HttpClient::builder()
        .roots(ca.as_bytes())?
        .resolve(&host, forward.address)
        .timeout(Duration::from_secs(60))
        .build()?
        .post(format!("https://{host}:{}{path}", forward.address.port()))
        .bearer_auth(bearer)
        .json(body)
        .send()
        .await?;
    let status = response.status();
    Ok((status, response.text().await?))
}

/// A local port-forward to one pod port. Drop closes it.
pub struct Forward(PortForward);

impl Forward {
    pub async fn start(
        client: &kube::Client,
        namespace: &str,
        pod: &str,
        port: u16,
    ) -> anyhow::Result<Self> {
        Ok(Self(
            PortForward::start(Api::namespaced(client.clone(), namespace), pod, port).await?,
        ))
    }

    pub fn address(&self) -> SocketAddr {
        self.0.address
    }
}

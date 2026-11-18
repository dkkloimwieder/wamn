//! Deploy the org control host from selected artifacts and bootstrap credentials.

use std::path::Path;

use anyhow::Context as _;
use serde_json::{Value, json};

use wamn_kubernetes::Target;
use wamn_kubernetes::kubectl;

use super::document::ClusterDocument;

use wamn_oci::package_artifact::http_client::{HttpClient, Redirects};
use wamn_registry::name::REGISTRY_PULL;

/// Wait until every control host observes the environment's administration login change.
pub async fn wait_administration(
    target: &Target,
    triple: &wamn_registry::Triple,
    present: bool,
    budget: std::time::Duration,
) -> anyhow::Result<()> {
    tokio::time::timeout(budget, async {
        let name = wamn_provision::control_administration_secret_name(&triple.org);
        let Some(secret) = kubectl::get::<Value>(
            &target.kubeconfig,
            &target.context,
            &target.namespace,
            "configmap",
            &name,
        )
        .await?
        else {
            return Ok(());
        };
        let key = wamn_provision::control_administration_key(&triple.project, &triple.env);
        let login = present
            .then(|| {
                secret["data"][&key]
                    .as_str()
                    .map(str::to_owned)
                    .context("administration configuration is absent")
            })
            .transpose()?;
        let path = format!("/etc/wamn/control-administration/{key}");
        let selector = format!(
            "app.kubernetes.io/name={}",
            wamn_registry::name::control_resource(&triple.org)
        );
        let script = if present {
            r#"test -r "$1" && cmp -s "$1" -"#
        } else {
            r#"test ! -e "$1""#
        };
        loop {
            let output = kubectl::kubectl(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                &["get", "pods", "-l", &selector, "-o", "json"],
                None,
            )
            .await?;
            let pods: Value = serde_json::from_str(&output).context("decode control host pods")?;
            let pods = pods["items"].as_array().context("control host pod list")?;
            let mut projected = !present || !pods.is_empty();
            for pod in pods {
                let pod = pod["metadata"]["name"]
                    .as_str()
                    .context("control host pod name")?;
                projected &= kubectl::kubectl(
                    &target.kubeconfig,
                    &target.context,
                    &target.namespace,
                    &[
                        "exec", "-i", pod, "-c", "control", "--", "sh", "-c", script, "--", &path,
                    ],
                    Some(login.as_deref().unwrap_or_default().as_bytes()),
                )
                .await
                .is_ok();
            }
            if projected {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    })
    .await
    .with_context(|| {
        format!(
            "control administration login projection for {triple} exceeded the readiness budget"
        )
    })?
}

/// Render the platform registry credential for hosts using the local registry Service.
pub fn registry_pull_secret(document: &ClusterDocument, auth_file: &Path) -> anyhow::Result<Value> {
    let authority = document.registry.split('/').next().unwrap_or_default();
    let credential = wamn_oci::package_artifact::registry_credentials::read_registry_credentials(
        auth_file, authority,
    )?;
    let mut config: Value = serde_json::from_slice(&std::fs::read(auth_file)?)?;
    config["auths"][super::resources::POD_REGISTRY] = json!({
        "username":credential.username(), "password":credential.password(),
    });
    Ok(json!({
        "apiVersion":"v1", "kind":"Secret", "type":"kubernetes.io/dockerconfigjson",
        "metadata":{"name":REGISTRY_PULL, "namespace":document.org},
        "stringData":{".dockerconfigjson":serde_json::to_string(&config)?},
    }))
}

/// Read bootstrap credentials and copy the operator Secret through the deploy client.
pub async fn initialize(target: &Target, document: &ClusterDocument) -> anyhow::Result<()> {
    let configuration: Option<Value> = kubectl::get(
        &target.kubeconfig,
        &target.context,
        document.org.as_str(),
        "configmap",
        &wamn_registry::name::control_administration_secret_name(&document.org),
    )
    .await?;
    anyhow::ensure!(
        configuration
            .as_ref()
            .is_some_and(|value| value["data"]["control.json"].is_string()),
        "control database configuration is absent"
    );
    Ok(())
}

/// Require the control route to refuse a request without credentials.
///
/// # Errors
/// Returns a forwarding, HTTP, or authorization-boundary failure.
pub async fn probe(target: &Target, document: &ClusterDocument) -> anyhow::Result<()> {
    let service = format!(
        "service/{}",
        wamn_registry::name::resource_suffix(
            &wamn_registry::name::control_resource(&document.org),
            "http"
        )
    );
    let forward =
        wamn_kubernetes::forward::port_forward(target, document.org.as_str(), &service, 80).await?;
    let response = HttpClient::builder()
        .redirects(Redirects::None)
        .timeout(std::time::Duration::from_secs(5))
        .build()?
        .get(format!(
            "http://127.0.0.1:{}/wamn_control/control/mine",
            forward.port
        ))
        .header(hyper::header::HOST, &document.route_hosts.control)
        .send()
        .await
        .context("probe the unauthenticated control route")?;
    anyhow::ensure!(
        response.status() == hyper::StatusCode::UNAUTHORIZED,
        "the unauthenticated control route returned {}; expected 401",
        response.status()
    );
    // The hint uses the exported context and the native control Deployment.
    println!(
        "Local control access: kubectl --context '{}' -n {} port-forward --address=127.0.0.1 deployment/{} 8080:80\nHost: {}",
        target.context.replace('\'', "'\\''"),
        document.org,
        wamn_registry::name::control_resource(&document.org),
        document.route_hosts.control,
    );
    Ok(())
}

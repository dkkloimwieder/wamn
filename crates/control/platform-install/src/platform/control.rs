//! Deploy the org control host from selected artifacts and bootstrap credentials.

use std::path::Path;

use anyhow::Context as _;
use serde_json::{Value, json};

use wamn_kubernetes::kubectl;
use wamn_release_control::release_chart::Target;

use super::artifact_plan::SelectedChart;
use super::document::{ClusterDocument, Provider, UpgradeTarget};

const REGISTRY_PULL: &str = "wamn-registry-pull";

/// Wait until every control host observes the environment's administration login change.
pub async fn wait_administration(
    target: &Target,
    triple: &wamn_registry::Triple,
    present: bool,
    budget: std::time::Duration,
) -> anyhow::Result<()> {
    tokio::time::timeout(budget, async {
        let name = wamn_control_provision::control_administration_secret_name(&triple.org);
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
        let key = wamn_control_provision::control_administration_key(
            &triple.project,
            triple.env.as_str(),
        );
        let login = present
            .then(|| {
                secret["data"][&key]
                    .as_str()
                    .map(str::to_owned)
                    .context("administration configuration is absent")
            })
            .transpose()?;
        let path = format!("/etc/wamn/control-administration/{key}");
        let selector = wamn_registry::name::host_selector(&wamn_registry::name::resource_suffix(
            "c",
            &triple.org,
        ));
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
                        "exec", "-i", pod, "-c", "host", "--", "sh", "-c", script, "--", &path,
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
    let credential = wamn_package_artifact::registry_credentials::read_registry_credentials(
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
        &document.org,
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
    super::oauth::ensure_key(target, document).await?;
    Ok(())
}

/// Render the org host values shared by the cluster and local control host.
pub fn host_values(org: &str) -> Value {
    let name = wamn_registry::name::resource_suffix("c", org);
    let mut values = json!({"runtime-operator": {"runtime": {
        "terminationGracePeriodSeconds": 70,
        "serviceAccount": {"create":true,"name":wamn_registry::control_nats_user(org)},
        // Control operation routes create their Jobs with this account's token.
        "automountServiceAccountToken": true,
        "hostGroups": [{
            "name": name, "namespace": org, "replicas": 1,
            "schedulerNatsUrl": "nats://wamn-nats.platform.svc.cluster.local:4222",
            "dataNatsUrl": "nats://wamn-nats.platform.svc.cluster.local:4222",
            "extraArgs": ["--control", format!("--org={org}")],
            "envFrom": [{"configMapRef":{"name":"wamn-host-settings"}}],
            "env": [
                {"name": "WAMN_DRAIN_BOUND_SECONDS", "value": "70"},
                {"name": "WAMN_CONTROL_URL", "value": "/etc/wamn/control-administration/control.json"},
                {"name": "WAMN_PAT_ISSUER", "value": format!("https://identity.{org}.svc.cluster.local")},
                {"name": "WAMN_PAT_CLIENT_CERT", "value": "/etc/identity-operator/tls.crt"},
                {"name": "WAMN_PAT_CLIENT_KEY", "value": "/etc/identity-operator/tls.key"},
                {"name": "WAMN_PAT_SERVER_CA", "value": "/etc/identity-ca/ca.crt"},
                {"name": "WAMN_CONTROL_ADMINISTRATION_DIR", "value": "/etc/wamn/control-administration"}
            ],
            "volumes": [
                {"name": "identity-operator", "secret": {"secretName": wamn_registry::name::resource_suffix("operator",org)}},
                {"name": "control-administration", "configMap": {"name": wamn_registry::name::control_administration_secret_name(org)}}
            ],
            "volumeMounts": [
                {"name": "identity-operator", "mountPath": "/etc/identity-operator", "readOnly": true},
                {"name": "control-administration", "mountPath": "/etc/wamn/control-administration", "readOnly": true}
            ],
            "http": {"enabled": true, "port": 80},
            "resources": {"requests": {"memory": "256Mi", "cpu": "250m"},
                "limits": {"memory": "4Gi", "cpu": "2"}}
        }]
    }}});
    // The control host's data connection is its org's NATS user, which covers
    // every environment of the org (docs/plan/alignment.md R7).
    values["global"] = json!({"certificates": {"dataCertSecretName": wamn_registry::name::control_nats_certificate(org)}});
    values
}

pub(crate) fn release_values(document: &ClusterDocument, chart: &Value) -> anyhow::Result<Value> {
    let mut desired = chart.clone();
    super::resources::merge(&mut desired, host_values(&document.org));
    let group = &mut desired["runtime-operator"]["runtime"]["hostGroups"][0];
    if let Some(configuration) = chart["runtime-operator"]["runtime"]["hostGroups"]
        .as_array()
        .and_then(|groups| groups.iter().find(|entry| entry["name"] == group["name"]))
    {
        super::resources::merge(group, configuration.clone());
    }
    let mut shared = super::installation::host_settings(document)["hostConfiguration"].take();
    for field in ["volumes", "volumeMounts"] {
        group[field]
            .as_array_mut()
            .context("control mounts")?
            .append(
                shared[field]
                    .take()
                    .as_array_mut()
                    .context("shared host mounts")?,
            );
    }
    if document.provider == Provider::Kind {
        group["extraArgs"]
            .as_array_mut()
            .expect("host arguments")
            .push("--allow-insecure-registries".into());
    }
    let scheme = if document.provider == Provider::Kind {
        "http"
    } else {
        "https"
    };
    group["env"].as_array_mut().expect("host environment").push(
        json!({"name":"WAMN_CONTROL_PUBLIC_ORIGIN","value":format!("{scheme}://{}", document.route_hosts.control)}));
    let platform_ui = match chart.get("platformUi") {
        Some(value) => value
            .as_str()
            .context("platformUi must be an artifact digest or empty")?,
        None => "",
    };
    if !platform_ui.is_empty() {
        wamn_package_artifact::ensure_digest(platform_ui)?;
    }
    desired["platformUi"] = platform_ui.into();
    desired["org"] = document.org.as_str().into();
    desired["routeHost"] = document.route_hosts.control.as_str().into();
    // The resource entry supplies the native control Service.
    desired["roles"] = json!([]);
    Ok(desired)
}

pub(super) fn resource_entry(
    document: &ClusterDocument,
    chart: &SelectedChart,
    installed: Option<&Value>,
) -> anyhow::Result<Value> {
    let mut desired = release_values(document, chart.values())?;
    for group in desired["runtime-operator"]["runtime"]["hostGroups"]
        .as_array_mut()
        .expect("control host groups")
    {
        if let Some(image) = super::installed::host_image(
            installed,
            group["name"].as_str().expect("control host group name"),
        ) {
            group["image"] = image.clone();
        }
    }
    desired["controlService"] = true.into();
    let desired = chart.selection_values(document, &UpgradeTarget::Control, desired)?;
    Ok(json!({"chart":chart.directory(), "version":chart.version,
        "name":wamn_registry::name::control_resource(&document.org), "namespace":document.org, "values":desired}))
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
        wamn_kubernetes::forward::port_forward(target, &document.org, &service, 80).await?;
    let response = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5))
        .build()?
        .get(format!(
            "http://127.0.0.1:{}/wamn_control/control/mine",
            forward.port
        ))
        .header(reqwest::header::HOST, &document.route_hosts.control)
        .send()
        .await
        .context("probe the unauthenticated control route")?;
    anyhow::ensure!(
        response.status() == reqwest::StatusCode::UNAUTHORIZED,
        "the unauthenticated control route returned {}; expected 401",
        response.status()
    );
    // The deploy kubeconfig is private and temporary, and the workload Service
    // has no selector, so the hint names the exported context and the host Deployment.
    println!(
        "Local control access: kubectl --context '{}' -n hosts port-forward --address=127.0.0.1 deployment/hostgroup-c-{} 8080:80\nHost: {}",
        target.context.replace('\'', "'\\''"),
        document.org,
        document.route_hosts.control,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_reapply_renders_the_recorded_host_image_override() -> anyhow::Result<()> {
        let document = document();
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let chart = super::super::artifact_plan::test_util::prepared_chart(&repository)?;
        let selected = super::super::artifact_plan::select(&chart.0)?;
        let mut installed = release_values(&document, selected.values())?;
        let image = json!({"registry":"previous.example", "repository":"host",
            "tag":format!("previous@sha256:{}", "b".repeat(64)), "pull_policy":"IfNotPresent"});
        installed["runtime-operator"]["runtime"]["hostGroups"][0]["image"] = image.clone();
        let entry = resource_entry(&document, &selected, Some(&installed))?;
        let objects = wamn_kubernetes::helm::template(&entry)?;
        let deployment = objects
            .iter()
            .find(|object| object["kind"] == "Deployment")
            .unwrap();
        assert_eq!(
            deployment["spec"]["template"]["spec"]["containers"][0]["image"],
            format!("previous.example/host:previous@sha256:{}", "b".repeat(64))
        );
        Ok(())
    }

    fn document() -> ClusterDocument {
        serde_json::from_value(json!({
            "provider": "kind", "project_id": "", "zone": "", "cluster_name": "test",
            "org": "acme", "owner_email": "owner@example.com", "registry": "localhost:5000",
            "buckets": {"web": "", "otel": "", "state": ""},
            "route_hosts": {"control": "control.example.com"}, "resend_from": "sender@example.com"
        }))
        .unwrap()
    }
}

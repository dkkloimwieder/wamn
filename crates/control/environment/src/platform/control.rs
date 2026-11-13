//! Deploy the org control host from selected artifacts and bootstrap credentials.

use std::path::Path;

use anyhow::Context as _;
use serde::Deserialize as _;
use serde_json::{Value, json};

use wamn_kubernetes::kubectl;
use wamn_release_control::release_chart::Target;

use super::artifact_plan::SelectedChart;
use super::bootstrap::{SecretData, secret_text};
use super::document::{ClusterDocument, Provider, UpgradeTarget};

const REGISTRY_PULL: &str = "wamn-registry-pull";
const REGISTRY_AUTH_FILE: &str = "/etc/wamn/registry/config.json";

/// Wait until every control host observes the environment's administration login change.
pub(crate) async fn wait_administration(
    target: &Target,
    triple: &wamn_registry::Triple,
    present: bool,
    budget: std::time::Duration,
) -> anyhow::Result<()> {
    tokio::time::timeout(budget, async {
        let name = wamn_control_provision::control_administration_secret_name(&triple.org);
        let Some(secret) = kubectl::get::<SecretData>(
            &target.kubeconfig,
            &target.context,
            &target.namespace,
            "secret",
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
        let login = present.then(|| secret_text(&secret, &key)).transpose()?;
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
        "metadata":{"name":REGISTRY_PULL, "namespace":"hosts"},
        "stringData":{".dockerconfigjson":serde_json::to_string(&config)?},
    }))
}

/// Read bootstrap credentials and copy the operator Secret through the deploy client.
pub async fn initialize(target: &Target, document: &ClusterDocument) -> anyhow::Result<()> {
    if document.provider == Provider::Kind {
        let auth_file = std::env::var_os("WAMN_REGISTRY_AUTH_FILE")
            .context("set WAMN_REGISTRY_AUTH_FILE for the platform registry credential")?;
        super::resources::apply_object(
            target,
            &registry_pull_secret(document, Path::new(&auth_file))?,
        )
        .await?;
    }
    let credential: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "hosts",
        "secret",
        &wamn_registry::control_nats_user(&document.org),
    )
    .await?
    .context("bootstrap control credential is missing")?;
    anyhow::ensure!(
        !secret_text(&credential, "url")?.is_empty(),
        "bootstrap control credential is empty"
    );
    let administration: Option<Value> = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "hosts",
        "secret",
        &wamn_registry::name::control_administration_secret_name(&document.org),
    )
    .await?;
    anyhow::ensure!(
        administration.is_some(),
        "bootstrap control administration Secret is missing"
    );
    let operator = wamn_registry::name::resource_suffix("operator", &document.org);
    let secret: Value = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "identity",
        "secret",
        &operator,
    )
    .await?
    .context("missing identity operator Secret")?;
    kubectl::apply(
        &target.kubeconfig,
        &target.context,
        "hosts",
        &operator_secret(secret, &operator),
    )
    .await
    .context("copy the control operator Secret")?;
    super::oauth::ensure_key(target, document).await?;
    let tls: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "identity",
        "secret",
        "identity-tls",
    )
    .await?
    .context("missing Secret identity/identity-tls")?;
    super::resources::apply_object(target, &identity_ca(&secret_text(&tls, "ca.crt")?)).await
}

fn operator_secret(mut secret: Value, name: &str) -> Value {
    secret["metadata"] = json!({"name":name, "namespace":"hosts"});
    secret
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
            "name": name, "namespace": "hosts", "replicas": 1,
            "schedulerNatsUrl": "nats://wamn-nats.platform.svc.cluster.local:4222",
            "dataNatsUrl": "nats://wamn-nats.platform.svc.cluster.local:4222",
            "extraArgs": ["--control", format!("--org={org}")],
            "env": [
                {"name": "WAMN_DRAIN_BOUND_SECONDS", "value": "70"},
                {"name": "DOCKER_CONFIG", "value": "/etc/wamn/registry"},
                {"name": "WAMN_REGISTRY_TOKEN_METADATA", "value": "true"},
                {"name": "WAMN_CONTROL_URL", "valueFrom": {"secretKeyRef": {
                    "name": wamn_registry::control_nats_user(org), "key": "url"
                }}},
                {"name": "WAMN_SESSION_ISSUER", "value": "https://identity.identity.svc.cluster.local"},
                {"name": "WAMN_SESSION_JWKS_CA", "value": "/etc/identity-ca/ca.crt"},
                {"name": "WAMN_PAT_ISSUER", "value": "https://identity.identity.svc.cluster.local"},
                {"name": "WAMN_PAT_CLIENT_CERT", "value": "/etc/identity-operator/tls.crt"},
                {"name": "WAMN_PAT_CLIENT_KEY", "value": "/etc/identity-operator/tls.key"},
                {"name": "WAMN_PAT_SERVER_CA", "value": "/etc/identity-ca/ca.crt"},
                {"name": "WAMN_CONTROL_ADMINISTRATION_DIR", "value": "/etc/wamn/control-administration"}
            ],
            "volumes": [
                {"name": "registry-pull", "configMap": {"name": "wamn-registry-helper"}},
                {"name": "identity-ca", "configMap": {"name": "identity-ca"}},
                {"name": "identity-operator", "secret": {"secretName": wamn_registry::name::resource_suffix("operator",org)}},
                {"name": "control-administration", "secret": {"secretName": wamn_registry::name::control_administration_secret_name(org)}}
            ],
            "volumeMounts": [
                {"name": "registry-pull", "mountPath": "/etc/wamn/registry", "readOnly": true},
                {"name": "identity-ca", "mountPath": "/etc/identity-ca", "readOnly": true},
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
    super::oauth::host_group(
        &mut values["runtime-operator"]["runtime"]["hostGroups"][0],
        &json!({"provider":"local"}),
    )
    .expect("control host group");
    values
}

pub(super) fn service_objects(
    payload: &Path,
    document: &ClusterDocument,
    image: &str,
) -> anyhow::Result<Vec<Value>> {
    let image = component_image(document, image)?;
    let source = std::fs::read_to_string(payload.join("control-service.yaml"))?;
    let mut objects = serde_yaml::Deserializer::from_str(&source)
        .map(Value::deserialize)
        .collect::<Result<Vec<_>, _>>()?;
    let name = wamn_registry::name::control_resource(&document.org);
    for object in &mut objects {
        object["metadata"]["name"] = wamn_registry::name::resource_suffix(&name, "http").into();
        if object["kind"] == "WorkloadDeployment" {
            let spec = &mut object["spec"]["template"]["spec"];
            spec["hostSelector"]["hostgroup"] = name.clone().into();
            spec["kubernetes"]["service"]["name"] =
                wamn_registry::name::resource_suffix(&name, "http").into();
            spec["hostInterfaces"][0]["config"]["host"] =
                document.route_hosts.control.clone().into();
            spec["components"][0]["image"] = image.as_ref().into();
        }
    }
    Ok(objects)
}

fn component_image<'a>(
    document: &ClusterDocument,
    image: &'a str,
) -> anyhow::Result<std::borrow::Cow<'a, str>> {
    if document.provider == Provider::Kind {
        let (_, path) = image
            .split_once('/')
            .context("HTTP image has no OCI authority")?;
        Ok(format!("{}/{path}", super::resources::POD_REGISTRY).into())
    } else {
        Ok(image.into())
    }
}

pub(crate) fn release_values(document: &ClusterDocument, chart: &Value) -> anyhow::Result<Value> {
    let mut desired = host_values(&document.org);
    desired["runtime-operator"]["runtime"]["image"] =
        chart["runtime-operator"]["runtime"]["image"].clone();
    let group = &mut desired["runtime-operator"]["runtime"]["hostGroups"][0];
    if let Some(configuration) = chart["runtime-operator"]["runtime"]["hostGroups"]
        .as_array()
        .and_then(|groups| groups.iter().find(|entry| entry["name"] == group["name"]))
    {
        super::manifests::merge(group, configuration.clone());
    }
    super::oauth::host_group(group, &super::oauth::configuration(document))?;
    if document.provider == Provider::Kind {
        let variables = group["env"].as_array_mut().expect("host environment");
        variables.retain(|value| value["name"] != "WAMN_REGISTRY_TOKEN_METADATA");
        variables.push(json!({"name":"WAMN_REGISTRY_AUTH_FILE", "value":REGISTRY_AUTH_FILE}));
        let volume = group["volumes"]
            .as_array_mut()
            .expect("host volumes")
            .iter_mut()
            .find(|volume| volume["name"] == "registry-pull")
            .expect("registry volume");
        *volume = json!({"name":"registry-pull", "secret":{
            "secretName":REGISTRY_PULL, "optional":false,
            "items":[{"key":".dockerconfigjson", "path":"config.json"}],
        }});
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
    // The resource entry supplies the control Service and WorkloadDeployment.
    desired["roles"] = json!([]);
    Ok(desired)
}

fn identity_ca(ca: &str) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {"name": "identity-ca", "namespace": "hosts"},
        "data": {"ca.crt": ca},
    })
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
        if let Some(image) = crate::environment::runtime::host_image(
            installed,
            group["name"].as_str().expect("control host group name"),
        ) {
            group["image"] = image.clone();
        }
    }
    desired["controlResources"] = service_objects(
        &chart.directory().join("platform/resources"),
        document,
        chart.image("http")?,
    )?
    .into();
    let desired = chart.selection_values(document, &UpgradeTarget::Control, desired)?;
    Ok(json!({"chart":chart.directory(), "version":chart.version,
        "name":wamn_registry::name::control_resource(&document.org), "namespace":"hosts", "values":desired}))
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
    let forward = wamn_kubernetes::forward::port_forward(target, "hosts", &service, 80).await?;
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
        let values = &entry["values"];
        assert_eq!(
            values["runtime-operator"]["runtime"]["hostGroups"][0]["image"],
            image
        );
        let objects = wamn_kubernetes::helm::template(&entry)?;
        let deployment = objects
            .iter()
            .find(|object| object["kind"] == "Deployment")
            .unwrap();
        assert_eq!(
            deployment["spec"]["template"]["spec"]["containers"][0]["image"],
            format!("previous.example/host:previous@sha256:{}", "b".repeat(64))
        );
        let entry = resource_entry(&document, &selected, None)?;
        let fresh = &entry["values"];
        assert!(
            fresh["runtime-operator"]["runtime"]["hostGroups"][0]
                .get("image")
                .is_none()
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

    #[test]
    fn control_objects_follow_the_org_route_and_stamped_image() {
        let document = document();
        let values = host_values(&document.org);
        let group = &values["runtime-operator"]["runtime"]["hostGroups"][0];
        assert_eq!(group["name"], "c-acme");
        assert_eq!(group["extraArgs"], json!(["--control", "--org=acme"]));
        assert_eq!(
            values["global"]["certificates"]["dataCertSecretName"],
            "wamn-nats-control-acme"
        );
        let variables = group["env"].as_array().unwrap();
        assert!(
            variables
                .iter()
                .any(|value| value["name"] == "WAMN_OAUTH_ENCRYPTION"
                    && value["value"] == r#"{"provider":"local"}"#)
        );
        assert!(
            variables
                .iter()
                .any(|value| value["name"] == "WAMN_OAUTH_KEY_FILE"
                    && value["value"] == "/etc/wamn/oauth/key")
        );
        assert!(
            group["volumes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value["secret"]["secretName"] == "wamn-oauth"
                    && value["secret"]["defaultMode"] == 384)
        );
        let mut cloud = document.clone();
        cloud.provider = super::super::document::Provider::Gcp;
        cloud.project_id = "project-one".into();
        cloud.zone = "us-east1-b".into();
        let configuration = super::super::oauth::configuration(&cloud);
        assert_eq!(
            configuration,
            json!({"provider":"gcp","project":"project-one","region":"us-east1"})
        );
        let mut cloud_group = group.clone();
        super::super::oauth::host_group(&mut cloud_group, &configuration).unwrap();
        let variables = cloud_group["env"].as_array().unwrap();
        assert!(
            variables
                .iter()
                .any(|value| value["name"] == "WAMN_OAUTH_ENCRYPTION"
                    && serde_json::from_str::<Value>(value["value"].as_str().unwrap()).unwrap()
                        == configuration)
        );
        assert!(
            !variables
                .iter()
                .any(|value| value["name"] == "WAMN_OAUTH_KEY_FILE")
        );
        assert!(
            !cloud_group["volumes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value["name"] == "oauth-key")
        );
        let objects = service_objects(
            Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../../deploy/platform"
            )),
            &document,
            "localhost:5000/components/http:build@sha256:abc",
        )
        .unwrap();
        let workload = &objects[1];
        assert_eq!(objects[0]["metadata"]["name"], "c-acme-http");
        assert_eq!(
            workload["spec"]["template"]["spec"]["components"][0]["image"],
            format!(
                "{}/components/http:build@sha256:abc",
                super::super::resources::POD_REGISTRY
            )
        );
        assert_eq!(
            workload["spec"]["template"]["spec"]["hostInterfaces"][0]["config"]["host"],
            "control.example.com"
        );
    }

    #[test]
    fn kind_host_keeps_selected_image_and_tls_without_cloud_credentials_or_application_routes() {
        let document = document();
        let image = json!({"registry":"", "repository":"localhost:5000/platform/host",
            "tag":format!("build@sha256:{}", "a".repeat(64)), "pull_policy":"IfNotPresent"});
        let chart = json!({"runtime-operator":{"runtime":{"image":image}}});
        let values = release_values(&document, &chart).unwrap();
        assert_eq!(values["runtime-operator"]["runtime"]["image"], image);
        assert_eq!(values["platformUi"], "");
        assert_eq!(values["routeHost"], document.route_hosts.control);
        assert_eq!(values["roles"], json!([]));
        let groups = values["runtime-operator"]["runtime"]["hostGroups"]
            .as_array()
            .unwrap();
        assert_eq!(groups.len(), 1);
        let group = &groups[0];
        assert_eq!(
            group["name"],
            wamn_registry::name::control_resource(&document.org)
        );
        assert!(
            group["extraArgs"]
                .as_array()
                .unwrap()
                .contains(&json!("--allow-insecure-registries"))
        );
        let variables = group["env"].as_array().unwrap();
        assert!(
            !variables
                .iter()
                .any(|value| value["name"] == "WAMN_REGISTRY_TOKEN_METADATA")
        );
        assert!(
            variables
                .iter()
                .any(|value| value["name"] == "WAMN_REGISTRY_AUTH_FILE"
                    && value["value"] == REGISTRY_AUTH_FILE)
        );
        assert!(
            variables
                .iter()
                .any(|value| value["name"] == "WAMN_CONTROL_URL"
                    && value["valueFrom"]["secretKeyRef"]
                        == json!({"name":"wamn-control-acme", "key":"url"})
                    && value.get("value").is_none())
        );
        let mounts = group["volumeMounts"].as_array().unwrap();
        for path in variables
            .iter()
            .filter_map(|variable| variable["value"].as_str())
            .filter(|path| path.starts_with("/etc/identity-"))
            .map(Path::new)
        {
            let mount = mounts
                .iter()
                .find(|mount| path.starts_with(mount["mountPath"].as_str().unwrap()))
                .unwrap();
            assert_eq!(mount["readOnly"], true);
            assert!(
                group["volumes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|volume| volume["name"] == mount["name"]
                        && (volume["configMap"]["name"] == "identity-ca"
                            || volume["secret"]["secretName"] == "operator-acme"))
            );
        }
        assert!(
            mounts
                .iter()
                .any(|mount| mount["name"] == "registry-pull" && mount["readOnly"] == true)
        );

        let mut cloud = document;
        cloud.provider = Provider::Gcp;
        cloud.project_id = "project-one".into();
        cloud.zone = "us-east1-b".into();
        let mut chart = chart;
        let digest = format!("sha256:{}", "a".repeat(64));
        chart["platformUi"] = digest.as_str().into();
        let values = release_values(&cloud, &chart).unwrap();
        assert_eq!(values["platformUi"], digest);
        assert_eq!(values["routeHost"], cloud.route_hosts.control);
        assert!(
            !values["runtime-operator"]["runtime"]["hostGroups"][0]["extraArgs"]
                .as_array()
                .unwrap()
                .contains(&json!("--allow-insecure-registries"))
        );
        let component = format!(
            "registry.example/components/http:build@sha256:{}",
            "b".repeat(64)
        );
        assert_eq!(component_image(&cloud, &component).unwrap(), component);
    }

    #[test]
    fn operator_copy_keeps_credentials_and_replaces_identity_namespace_ownership() {
        let source = json!({"apiVersion":"v1", "kind":"Secret", "type":"kubernetes.io/tls",
            "metadata":{"name":"operator-acme", "namespace":"identity", "uid":"source-uid",
                "ownerReferences":[{"name":"identity-certificate"}], "resourceVersion":"123"},
            "data":{"tls.crt":"certificate", "tls.key":"private-key"}});
        let copied = operator_secret(source, "operator-acme");
        assert_eq!(
            copied["metadata"],
            json!({"name":"operator-acme", "namespace":"hosts"})
        );
        assert_eq!(copied["type"], "kubernetes.io/tls");
        assert_eq!(
            copied["data"],
            json!({"tls.crt":"certificate", "tls.key":"private-key"})
        );
    }
}

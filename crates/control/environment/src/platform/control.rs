//! Deploy the org control host from selected artifacts and bootstrap credentials.

use std::path::Path;

use anyhow::Context as _;
use serde::{Deserialize as _, Serialize};
use serde_json::{Value, json};

use crate::kubectl;
use wamn_release_control::release_chart::Target;

use super::artifact_plan::SelectedChart;
use super::document::{ClusterDocument, Provider};
use super::job::{self, JobRequest};

// Bootstrap owns credential provisioning. Inspect its state without printing it.
pub(super) const PROVISION: &str = r#"credentials() {
  credential=$(kubectl -n hosts get secret "wamn-control-$ORG" -o jsonpath='{.data.url}') || return 1
  [ -n "$credential" ] || return 1
  kubectl -n hosts get secret "wamn-control-administration-$ORG" -o name >/dev/null || return 1
}
credentials || { echo 'bootstrap control credentials are missing' >&2; exit 1; }
"#;

pub(super) const COPY_OPERATOR: &str = r#"copy_operator() {
  umask 077
  scratch=$(mktemp -d)
  trap 'rm -rf "$scratch"' EXIT
  kubectl -n identity get secret "$OPERATOR" -o json > "$scratch/source.json" || return 1
  kubectl patch --local --type=json -f "$scratch/source.json" \
    --patch-file /inputs/metadata.json -o json > "$scratch/hosts.json" 2>/dev/null || return 1
  kubectl apply --server-side -f "$scratch/hosts.json" 2>/dev/null || return 1
}
copy_operator || { echo 'copy operator Secret failed' >&2; exit 1; }
"#;

#[derive(Serialize)]
struct SecretMetadata<'a> {
    name: &'a str,
    namespace: &'a str,
}

#[derive(Serialize)]
struct MetadataPatch<'a> {
    op: &'static str,
    path: &'static str,
    value: SecretMetadata<'a>,
}

pub(super) fn provision_request(document: &ClusterDocument, image: &str) -> JobRequest {
    JobRequest {
        name: format!("platform-control-credentials-{}", document.org),
        image: image.into(),
        command: vec!["/bin/sh".into(), "-ec".into()],
        arguments: vec![PROVISION.into()],
        calls_kubernetes: true,
        environment: [("ORG".into(), document.org.clone())].into(),
        ..JobRequest::default()
    }
}

pub async fn credentials(
    target: &Target,
    document: &ClusterDocument,
    chart: &SelectedChart,
    directory: &Path,
) -> anyhow::Result<()> {
    let image = chart.image("ctl")?;
    let request = provision_request(document, image);
    job::run(target, directory, &request, &[]).await?;
    let request = operator_request(document, image)?;
    job::run(target, directory, &request, &[])
        .await
        .context("copy the control operator Secret")?;
    Ok(())
}

/// Render the org host values shared by the cluster and local control host.
pub fn host_values(org: &str) -> Value {
    let name = format!("c-{org}");
    let mut values = json!({"runtime-operator": {"runtime": {
        "terminationGracePeriodSeconds": 70,
        "serviceAccount": {"create":true,"name":format!("wamn-control-{org}")},
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
                    "name": format!("wamn-control-{org}"), "key": "url"
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
                {"name": "identity-operator", "secret": {"secretName": format!("operator-{org}")}},
                {"name": "control-administration", "secret": {"secretName": format!("wamn-control-administration-{org}")}}
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
    values["global"] =
        json!({"certificates": {"dataCertSecretName": format!("wamn-nats-control-{org}")}});
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
    let name = format!("c-{}", document.org);
    for object in &mut objects {
        object["metadata"]["name"] = format!("{name}-http").into();
        if object["kind"] == "WorkloadDeployment" {
            let spec = &mut object["spec"]["template"]["spec"];
            spec["hostSelector"]["hostgroup"] = name.clone().into();
            spec["kubernetes"]["service"]["name"] = format!("{name}-http").into();
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

fn release_values(
    document: &ClusterDocument,
    chart: &Value,
    platform_ui: bool,
) -> anyhow::Result<Value> {
    let mut desired = host_values(&document.org);
    desired["runtime-operator"]["runtime"]["image"] =
        chart["runtime-operator"]["runtime"]["image"].clone();
    let group = &mut desired["runtime-operator"]["runtime"]["hostGroups"][0];
    super::oauth::host_group(group, &super::oauth::configuration(document))?;
    if document.provider == Provider::Kind {
        group["env"]
            .as_array_mut()
            .expect("host environment")
            .retain(|value| {
                !matches!(
                    value["name"].as_str(),
                    Some("DOCKER_CONFIG" | "WAMN_REGISTRY_TOKEN_METADATA")
                )
            });
        for field in ["volumes", "volumeMounts"] {
            group[field]
                .as_array_mut()
                .expect("host volumes")
                .retain(|value| value["name"] != "registry-pull");
        }
        // This host loads only the control HTTP component from the local Service.
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
    desired["platformUi"] = platform_ui.into();
    desired["routeHost"] = if platform_ui {
        document.route_hosts.control.as_str()
    } else {
        ""
    }
    .into();
    // Direct apply owns the control Service and WorkloadDeployment.
    desired["roles"] = json!([]);
    Ok(desired)
}

pub async fn release(
    target: &Target,
    document: &ClusterDocument,
    chart: &SelectedChart,
    directory: &Path,
) -> anyhow::Result<()> {
    let build_record = chart
        .directory()
        .join(crate::platform_build::BUILD_RECORD_FILE);
    let platform_ui = match std::fs::read(&build_record) {
        Ok(bytes) => serde_json::from_slice::<crate::platform_build::PlatformBuildRecord>(&bytes)
            .context("decode the selected platform build record")?
            .platform_ui
            .is_some(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error).context("read the selected platform build record"),
    };
    let desired = release_values(document, chart.values(), platform_ui)?;
    super::oauth::ensure_key(target, document).await?;
    let values = directory.join("values-control.yaml");
    std::fs::write(&values, serde_yaml::to_string(&desired)?)?;
    let name = format!("c-{}", document.org);
    let hosts = Target {
        namespace: "hosts".into(),
        ..target.clone()
    };
    wamn_release_control::release_chart::recover_pending(&hosts, &name).await?;
    // As environment apply does, a failed upgrade carries the host pods.
    let poller = crate::environment::host_report::Poller::start(&hosts, &name);
    let upgraded = wamn_release_control::release_chart::upgrade(
        &hosts,
        chart.directory(),
        &name,
        &values,
        std::time::Duration::from_secs(600),
        "platform control host",
    )
    .await;
    let report = poller.finish();
    upgraded.map_err(|error| crate::environment::host_report::attach(&error, &report))?;
    for object in service_objects(
        &chart.directory().join("platform/resources"),
        document,
        chart.image("http")?,
    )? {
        super::resources::apply_object(target, &object).await?;
    }
    kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        "hosts",
        &[
            "rollout",
            "status",
            &format!("deployment/hostgroup-{name}"),
            "--timeout=600s",
        ],
        None,
    )
    .await?;
    Ok(())
}

pub(super) fn operator_request(
    document: &ClusterDocument,
    image: &str,
) -> anyhow::Result<JobRequest> {
    let operator = format!("operator-{}", document.org);
    Ok(JobRequest {
        name: format!("platform-control-operator-{}", document.org),
        image: image.into(),
        command: vec!["/bin/sh".into(), "-ec".into()],
        arguments: vec![COPY_OPERATOR.into()],
        calls_kubernetes: true,
        environment: [("OPERATOR".into(), operator.clone())].into(),
        files: [(
            "metadata.json".into(),
            serde_json::to_string(&[MetadataPatch {
                op: "replace",
                path: "/metadata",
                value: SecretMetadata {
                    name: &operator,
                    namespace: "hosts",
                },
            }])?,
        )]
        .into(),
        ..JobRequest::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn control_preparation_consumes_bootstrap_state_without_database_credentials() {
        let request = provision_request(&document(), "registry/ctl@sha256:abc");
        assert!(!request.emits_secret);
        assert!(request.secret_mounts.is_empty());
        assert_eq!(request.image, "registry/ctl@sha256:abc");
        assert_eq!(
            super::super::job_contract::access(&request).unwrap()[0].namespace,
            "hosts"
        );
        assert!(!request.environment.contains_key("WAMN_SYSTEM_ADMIN_URL"));
    }

    #[test]
    fn kind_host_keeps_selected_image_and_tls_without_cloud_credentials_or_application_routes() {
        let document = document();
        let image = json!({"registry":"", "repository":"localhost:5000/platform/host",
            "tag":format!("build@sha256:{}", "a".repeat(64)), "pull_policy":"IfNotPresent"});
        let chart = json!({"runtime-operator":{"runtime":{"image":image}}});
        let values = release_values(&document, &chart, false).unwrap();
        assert_eq!(values["runtime-operator"]["runtime"]["image"], image);
        assert_eq!(values["platformUi"], false);
        assert_eq!(values["routeHost"], "");
        assert_eq!(values["roles"], json!([]));
        let groups = values["runtime-operator"]["runtime"]["hostGroups"]
            .as_array()
            .unwrap();
        assert_eq!(groups.len(), 1);
        let group = &groups[0];
        assert_eq!(group["name"], format!("c-{}", document.org));
        assert!(
            group["extraArgs"]
                .as_array()
                .unwrap()
                .contains(&json!("--allow-insecure-registries"))
        );
        let variables = group["env"].as_array().unwrap();
        assert!(!variables.iter().any(|value| matches!(
            value["name"].as_str(),
            Some("DOCKER_CONFIG" | "WAMN_REGISTRY_TOKEN_METADATA")
        )));
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
        assert!(!mounts.iter().any(|mount| mount["name"] == "registry-pull"));

        let mut cloud = document;
        cloud.provider = Provider::Gcp;
        cloud.project_id = "project-one".into();
        cloud.zone = "us-east1-b".into();
        let values = release_values(&cloud, &chart, true).unwrap();
        assert_eq!(values["platformUi"], true);
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
    fn operator_copy_uses_the_selected_ctl_and_only_org_scoped_references() {
        let image = "registry.example/platform/ctl@sha256:abc";
        let request = operator_request(&document(), image).unwrap();
        assert_eq!(request.image, image);
        assert!(!request.emits_secret);
        assert_eq!(request.environment["OPERATOR"], "operator-acme");
        let patch: Value = serde_json::from_str(&request.files["metadata.json"]).unwrap();
        assert_eq!(
            patch,
            json!([{"op":"replace", "path":"/metadata", "value":{"name":"operator-acme", "namespace":"hosts"}}])
        );
    }
}

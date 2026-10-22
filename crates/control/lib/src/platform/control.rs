//! Install the org control host and its first-install credentials.

use std::path::Path;

use anyhow::Context as _;
use serde::{Deserialize as _, Serialize};
use serde_json::{Value, json};

use crate::{kubectl, release_chart::Target};

use super::document::ClusterDocument;
use super::job::{self, Access, JobRequest, PolicyRule, SecretMount};

// Neither command output nor an API error can copy credential bytes to a log.
const PROVISION: &str = r#"prepare() {
  umask 077
  scratch=$(mktemp -d)
  trap 'rm -rf "$scratch"' EXIT
  database=$(cat /etc/wamn/postgres/uri)
  export WAMN_SYSTEM_ADMIN_URL="${database%/*}/wamn_system"
  existing=$(kubectl -n hosts get secret "wamn-control-$ORG" --ignore-not-found -o name) || return 1
  if [ -z "$existing" ]; then
    /usr/local/bin/wamn-ctl provision-org --org "$ORG" --template trials --pool wamn-pg \
      --admin "$OWNER_EMAIL" --prepare-control-generation a --namespace hosts \
      --db-host "$DATABASE_HOST" --emit-control-secret "$scratch/control.json" || return 1
    kubectl apply --server-side -f "$scratch/control.json" || return 1
  fi
  existing=$(kubectl -n hosts get secret "wamn-control-administration-$ORG" --ignore-not-found -o name) || return 1
  if [ -z "$existing" ]; then
    /usr/local/bin/wamn-ctl provision-org --org "$ORG" --template trials --pool wamn-pg \
      --admin "$OWNER_EMAIL" --namespace hosts \
      --emit-control-administration-secret "$scratch/administration.json" || return 1
    kubectl create -f "$scratch/administration.json" || return 1
  fi
}
prepare >/dev/null 2>/dev/null || { echo 'prepare control credentials failed' >&2; exit 1; }
"#;

const COPY_OPERATOR: &str = r#"copy_operator() {
  umask 077
  scratch=$(mktemp -d)
  trap 'rm -rf "$scratch"' EXIT
  kubectl -n identity get secret "$OPERATOR" -o json > "$scratch/source.json" || return 1
  kubectl patch --local --type=json -f "$scratch/source.json" \
    --patch-file /inputs/metadata.json -o json > "$scratch/hosts.json" || return 1
  kubectl apply --server-side -f "$scratch/hosts.json" || return 1
}
copy_operator >/dev/null 2>/dev/null || { echo 'copy operator Secret failed' >&2; exit 1; }
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

fn secret_access(namespace: &str, verbs: &[&str]) -> Access {
    Access {
        namespace: namespace.into(),
        rules: vec![PolicyRule {
            api_groups: vec![String::new()],
            resources: vec!["secrets".into()],
            verbs: verbs.iter().map(|verb| (*verb).into()).collect(),
        }],
    }
}

fn provision_request(document: &ClusterDocument, image: &str) -> JobRequest {
    JobRequest {
        name: format!("platform-control-credentials-{}", document.org),
        image: image.into(),
        command: vec!["/bin/sh".into(), "-ec".into()],
        arguments: vec![PROVISION.into()],
        calls_kubernetes: true,
        once: true,
        environment: [
            ("ORG".into(), document.org.clone()),
            ("OWNER_EMAIL".into(), document.owner_email.clone()),
            ("DATABASE_HOST".into(), super::install::DATABASE_HOST.into()),
        ]
        .into(),
        files: [(
            "kubeconfig".into(),
            super::environments::kubeconfig().to_string(),
        )]
        .into(),
        secret_mounts: vec![SecretMount {
            name: "wamn-pg-superuser".into(),
            path: "/etc/wamn/postgres".into(),
            keys: vec!["uri".into()],
        }],
        permissions: vec![secret_access("hosts", &["get", "create", "patch"])],
        ..JobRequest::default()
    }
}

pub(super) async fn credentials(
    target: &Target,
    document: &ClusterDocument,
    directory: &Path,
) -> anyhow::Result<bool> {
    let image = super::install::runner_image(directory)?;
    let mut changed = job::run(
        target,
        directory,
        &provision_request(document, &image),
        &json!({"org": document.org}),
        &[],
    )
    .await?;
    let operator = format!("operator-{}", document.org);
    // Read only metadata on the verb's machine. The Job copies the private data.
    let version = kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        "identity",
        &[
            "get",
            "secret",
            &operator,
            "-o",
            "jsonpath={.metadata.resourceVersion}",
        ],
        None,
    )
    .await?;
    anyhow::ensure!(
        !version.is_empty(),
        "operator Secret has no resource version"
    );
    let request = JobRequest {
        name: format!("platform-control-operator-{}", document.org),
        image,
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
        permissions: vec![
            secret_access("identity", &["get"]),
            secret_access("hosts", &["get", "create", "patch"]),
        ],
        ..JobRequest::default()
    };
    changed |= job::run(
        target,
        directory,
        &request,
        &json!({"source_version": version}),
        &[],
    )
    .await
    .context("copy the control operator Secret")?;
    Ok(changed)
}

/// Render the org host values shared by the cluster and local control host.
pub(crate) fn host_values(org: &str) -> Value {
    let name = format!("c-{org}");
    json!({"runtime-operator": {"runtime": {
        "terminationGracePeriodSeconds": 70,
        "hostGroups": [{
            "name": name, "namespace": "hosts", "replicas": 1,
            "schedulerNatsUrl": "nats://nats.platform.svc.cluster.local:4222",
            "dataNatsUrl": "nats://nats.platform.svc.cluster.local:4222",
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
    }}})
}

fn service_objects(
    repository: &Path,
    document: &ClusterDocument,
    chart: &Value,
) -> anyhow::Result<Vec<Value>> {
    let image = chart["platform"]["roles"]["http"]["image"]
        .as_str()
        .filter(|image| image.contains("@sha256:"))
        .context("the chart has no pinned HTTP image")?;
    let source = std::fs::read_to_string(repository.join("deploy/gcp/control-service.yaml"))?;
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
            spec["components"][0]["image"] = image.into();
        }
    }
    Ok(objects)
}

pub(super) async fn release(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    directory: &Path,
    registry: &wamn_package_artifact::PackageRegistry,
) -> anyhow::Result<bool> {
    let chart = directory.join("release-chart");
    let values = directory.join("values-control.yaml");
    let mut desired = host_values(&document.org);
    desired["routeHost"] = document.route_hosts.control.clone().into();
    desired["platformUi"] = true.into();
    let mut changed = registry_ca(target, &document.org, registry, &mut desired).await?;
    std::fs::write(&values, serde_yaml::to_string(&desired)?)?;
    let name = format!("c-{}", document.org);
    let rendered = super::install::helm(
        target,
        &[
            "template".as_ref(),
            name.as_ref(),
            chart.as_os_str(),
            "--namespace".as_ref(),
            "hosts".as_ref(),
            "-f".as_ref(),
            values.as_os_str(),
        ],
    )
    .await?;
    let source = std::str::from_utf8(&rendered)?;
    let objects = serde_yaml::Deserializer::from_str(source)
        .map(Value::deserialize)
        .collect::<Result<Vec<_>, _>>()?;
    let mut release_changed = false;
    for object in objects.iter().filter(|object| !object.is_null()) {
        let kind = object["kind"].as_str().context("control manifest kind")?;
        let object_name = object["metadata"]["name"]
            .as_str()
            .context("control manifest name")?;
        let live: Option<Value> = kubectl::get(
            &target.kubeconfig,
            &target.context,
            "hosts",
            kind,
            object_name,
        )
        .await?;
        release_changed |= live
            .as_ref()
            .is_none_or(|live| !super::manifests::already_applied(object, live));
    }
    if release_changed {
        let hosts = Target {
            namespace: "hosts".into(),
            ..target.clone()
        };
        crate::release_chart::upgrade(
            &hosts,
            &chart,
            &name,
            &values,
            std::time::Duration::from_secs(600),
            "platform control host",
        )
        .await?;
    }
    changed |= release_changed;
    let stamped: Value = serde_yaml::from_slice(&std::fs::read(chart.join("values.yaml"))?)?;
    for object in service_objects(repository, document, &stamped)? {
        changed |= super::install::apply_object(target, &object).await?;
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
    Ok(changed)
}

async fn registry_ca(
    target: &Target,
    org: &str,
    registry: &wamn_package_artifact::PackageRegistry,
    values: &mut Value,
) -> anyhow::Result<bool> {
    if registry.oci_ca_paths.is_empty() {
        return Ok(false);
    }
    let name = format!("control-oci-ca-{org}");
    let mut data = serde_json::Map::new();
    let mut paths = Vec::new();
    for (index, path) in registry.oci_ca_paths.iter().enumerate() {
        let key = format!("ca-{index}.crt");
        data.insert(key.clone(), std::fs::read_to_string(path)?.into());
        paths.push(format!("/etc/wamn/oci-ca/{key}"));
    }
    let group = &mut values["runtime-operator"]["runtime"]["hostGroups"][0];
    group["ociCaPaths"] = json!(paths);
    group["volumes"]
        .as_array_mut()
        .context("control volumes")?
        .push(json!({"name": "oci-ca", "configMap": {"name": name}}));
    group["volumeMounts"]
        .as_array_mut()
        .context("control mounts")?
        .push(json!({"name": "oci-ca", "mountPath": "/etc/wamn/oci-ca", "readOnly": true}));
    super::install::apply_object(
        target,
        &json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": {"name": name, "namespace": "hosts"}, "data": data
        }),
    )
    .await
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
        let chart = json!({"platform": {"roles": {"http": {"image": "registry/http@sha256:abc"}}}});
        let objects = service_objects(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../..")),
            &document,
            &chart,
        )
        .unwrap();
        let workload = &objects[1];
        assert_eq!(objects[0]["metadata"]["name"], "c-acme-http");
        assert_eq!(
            workload["spec"]["template"]["spec"]["components"][0]["image"],
            "registry/http@sha256:abc"
        );
        assert_eq!(
            workload["spec"]["template"]["spec"]["hostInterfaces"][0]["config"]["host"],
            "control.example.com"
        );
        let mut live = workload.clone();
        live["metadata"]["resourceVersion"] = "12".into();
        assert!(super::super::manifests::already_applied(workload, &live));
        live["spec"]["template"]["spec"]["components"][0]["image"] = "old".into();
        assert!(!super::super::manifests::already_applied(workload, &live));
    }

    #[test]
    fn control_preparation_mounts_credentials_and_never_exports_them() {
        let request = provision_request(&document(), "registry/runner@sha256:abc");
        assert!(request.once);
        assert!(!request.emits_secret);
        assert!(!request.capture_output);
        assert_eq!(request.secret_mounts[0].name, "wamn-pg-superuser");
        assert_eq!(request.secret_mounts[0].keys, ["uri"]);
        assert_eq!(request.permissions[0].namespace, "hosts");
        assert!(!request.environment.contains_key("WAMN_SYSTEM_ADMIN_URL"));
    }
}

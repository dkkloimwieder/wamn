//! Read serving Kubernetes resources before a package upgrade and before application.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;

/// Explicit locations of the host and package workloads whose live state is proved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadTarget {
    pub kubeconfig: PathBuf,
    pub context: String,
    pub namespace: String,
    pub host_deployment: String,
    pub package_workloads: BTreeMap<String, String>,
}

/// Stable Kubernetes object identity and its exact deployed specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectIdentity {
    pub name: String,
    pub uid: String,
    pub generation: u64,
    pub spec_sha256: String,
}

/// Live application schema and the resources that supplied its serving replicas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageWorkload {
    pub deployment: ObjectIdentity,
    pub replica_set: ObjectIdentity,
    pub workloads: BTreeMap<String, ObjectIdentity>,
    pub schema: String,
}

/// Serving identities retained in qualification evidence and compared before apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServingWorkloads {
    pub host_deployment: ObjectIdentity,
    pub manifest_digest: String,
    pub packages: BTreeMap<String, PackageWorkload>,
}

/// Observe a converged host release and each package's actual ready workload schema.
///
/// Reads Kubernetes only. Repeating this observation supplies the pre-apply recheck;
/// pod replacements do not change its evidence, while changed deployed specs do.
pub async fn observe(
    target: &WorkloadTarget,
    tenant: &str,
    environment: &str,
    expected_manifest_digest: &str,
) -> anyhow::Result<ServingWorkloads> {
    ensure!(
        !target.kubeconfig.as_os_str().is_empty(),
        "Kubernetes configuration path is required"
    );
    ensure!(!target.context.is_empty(), "Kubernetes context is required");
    resource_name(&target.namespace)?;
    resource_name(&target.host_deployment)?;
    wamn_catalog::ManifestDigest::parse(expected_manifest_digest.to_owned())?;
    let deployment = get(target, &["deployment", &target.host_deployment]).await?;
    let replicas = get(target, &["replicasets"]).await?;
    let pods = get(target, &["pods"]).await?;
    let live_pods = validate_host(
        &deployment,
        &replicas,
        &pods,
        &target.namespace,
        expected_manifest_digest,
    )?;
    let mut packages = BTreeMap::new();
    // A package with no SQL needs host convergence but has no runtime schema to inspect.
    if !target.package_workloads.is_empty() {
        let hosts = get(target, &["hosts.runtime.wasmcloud.dev", "--all-namespaces"]).await?;
        let workloads = get(target, &["workloads.runtime.wasmcloud.dev"]).await?;
        let workload_replicas = get(target, &["workloadreplicasets.runtime.wasmcloud.dev"]).await?;
        for (package, name) in &target.package_workloads {
            ensure!(!package.is_empty(), "package workload identity is empty");
            resource_name(name)?;
            let declared =
                get(target, &["workloaddeployments.runtime.wasmcloud.dev", name]).await?;
            let current = text(&declared, "/status/currentReplicaSet/name")?;
            resource_name(current)?;
            let replica = items(&workload_replicas)?
                .iter()
                .find(|replica| replica["metadata"]["name"] == current)
                .context("current workload replica set is absent")?;
            for old in items(&workload_replicas)?.iter().filter(|old| {
                owned_by(old, &declared, "WorkloadDeployment")
                    && old["metadata"]["uid"] != replica["metadata"]["uid"]
            }) {
                ensure!(
                    !items(&workloads)?.iter().any(|workload| owned_by(
                        workload,
                        old,
                        "WorkloadReplicaSet"
                    )),
                    "previous workload replica set still has live replicas"
                );
            }
            let observation = validate_package(
                &declared,
                replica,
                &workloads,
                &hosts,
                &live_pods,
                &deployment,
                &target.namespace,
                tenant,
                environment,
            )
            .with_context(|| format!("observe serving workload for package {package}"))?;
            packages.insert(package.clone(), observation);
        }
    }
    let host_deployment = identity(&deployment)?;
    ensure!(
        identity(&get(target, &["deployment", &target.host_deployment]).await?)? == host_deployment,
        "host deployment changed during workload observation"
    );
    Ok(ServingWorkloads {
        host_deployment,
        manifest_digest: expected_manifest_digest.to_owned(),
        packages,
    })
}

async fn get(target: &WorkloadTarget, resources: &[&str]) -> anyhow::Result<Value> {
    let mut command = Command::new("kubectl");
    command
        .arg("--kubeconfig")
        .arg(&target.kubeconfig)
        .args([
            "--context",
            &target.context,
            "--namespace",
            &target.namespace,
        ])
        .arg("--request-timeout=20s")
        .arg("get")
        .args(resources)
        .args(["-o", "json"]);
    let output = crate::owned_command::execute(
        &mut command,
        Duration::from_secs(30),
        Duration::from_secs(5),
    )
    .await?;
    ensure!(
        output.status.success(),
        "read Kubernetes {}: {}",
        resources.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).context("parse observed Kubernetes resource")
}

fn validate_host<'a>(
    deployment: &Value,
    replicas: &Value,
    pods: &'a Value,
    namespace: &str,
    digest: &str,
) -> anyhow::Result<Vec<&'a Value>> {
    object(deployment, "Deployment", namespace)?;
    let desired = count(deployment, "/spec/replicas")?;
    ensure!(desired > 0, "host deployment has no serving replicas");
    ensure!(
        count(deployment, "/status/observedGeneration")?
            >= count(deployment, "/metadata/generation")?,
        "host deployment controller has not observed the deployed generation"
    );
    for field in [
        "replicas",
        "updatedReplicas",
        "readyReplicas",
        "availableReplicas",
    ] {
        ensure!(
            count(deployment, &format!("/status/{field}"))? == desired,
            "host deployment is not fully rolled out: {field}"
        );
    }
    ensure!(
        deployment["status"]["unavailableReplicas"]
            .as_u64()
            .unwrap_or(0)
            == 0,
        "host deployment has unavailable replicas"
    );
    let selector = deployment
        .pointer("/spec/selector/matchLabels")
        .and_then(Value::as_object)
        .context("host deployment requires selector labels")?;
    ensure!(!selector.is_empty(), "host deployment selector is empty");
    ensure!(
        deployment
            .pointer("/spec/selector/matchExpressions")
            .is_none_or(|value| { value.as_array().is_some_and(Vec::is_empty) }),
        "host deployment requires a matchLabels selector"
    );
    let host_replica_uids = items(replicas)?
        .iter()
        .filter(|replica| owned_by(replica, deployment, "Deployment"))
        .map(|replica| text(replica, "/metadata/uid"))
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    let selected = items(pods)?
        .iter()
        .filter(|pod| {
            selector
                .iter()
                .all(|(key, value)| pod["metadata"]["labels"][key] == *value)
        })
        .collect::<Vec<_>>();
    ensure!(
        selected.len() as u64 == desired,
        "host pod count has not converged"
    );
    let declared_container = host_container(&deployment["spec"]["template"]["spec"])?;
    require_digest(declared_container, digest)?;
    for pod in &selected {
        object(pod, "Pod", namespace)?;
        ensure!(
            owners(pod)?.iter().any(|owner| {
                owner["kind"] == "ReplicaSet"
                    && owner["controller"] == true
                    && owner["uid"]
                        .as_str()
                        .is_some_and(|uid| host_replica_uids.contains(uid))
            }),
            "selected host pod does not belong to the host deployment"
        );
        ensure!(
            pod["status"]["phase"] == "Running",
            "host pod is not running"
        );
        ready(&pod["status"], "Ready")?;
        let container = host_container(&pod["spec"])?;
        require_digest(container, digest)?;
        ensure!(
            container["image"] == declared_container["image"]
                && container["args"] == declared_container["args"],
            "host pod is running a different image or arguments from its deployment"
        );
        ensure!(
            pod["status"]["containerStatuses"]
                .as_array()
                .is_some_and(|statuses| {
                    statuses
                        .iter()
                        .any(|status| status["name"] == "host" && status["ready"] == true)
                }),
            "host container is not ready"
        );
    }
    Ok(selected)
}

#[expect(
    clippy::too_many_arguments,
    reason = "one observation joins the actual resource owner chain"
)]
fn validate_package(
    deployment: &Value,
    replica: &Value,
    workloads: &Value,
    hosts: &Value,
    pods: &[&Value],
    host_deployment: &Value,
    namespace: &str,
    tenant: &str,
    environment: &str,
) -> anyhow::Result<PackageWorkload> {
    object(deployment, "WorkloadDeployment", namespace)?;
    object(replica, "WorkloadReplicaSet", namespace)?;
    ensure!(
        owned_by(replica, deployment, "WorkloadDeployment"),
        "current workload replica set has a foreign owner"
    );
    ensure!(
        text(deployment, "/status/currentReplicaSet/name")? == text(replica, "/metadata/name")?,
        "workload replica set is not current"
    );
    ready(&deployment["status"], "Ready")?;
    let desired = count(deployment, "/spec/replicas")?;
    ensure!(desired > 0, "package workload has no serving replicas");
    for pointer in [
        "/status/currentReplicas",
        "/status/replicas/expected",
        "/status/replicas/current",
        "/status/replicas/ready",
    ] {
        ensure!(
            count(deployment, pointer)? == desired,
            "package workload rollout has not converged: {pointer}"
        );
    }
    ensure!(
        deployment
            .pointer("/status/replicas/unavailable")
            .is_none_or(|value| value.is_null()
                || value == &Value::from(0)
                || value == &Value::from(false)),
        "package workload has unavailable replicas"
    );
    let selected = items(workloads)?
        .iter()
        .filter(|workload| owned_by(workload, replica, "WorkloadReplicaSet"))
        .collect::<Vec<_>>();
    ensure!(
        selected.len() as u64 == desired,
        "actual workload replica count differs from the deployment"
    );
    let mut schemas = BTreeSet::new();
    let mut identities = BTreeMap::new();
    for workload in selected {
        object(workload, "Workload", namespace)?;
        for condition in ["Ready", "Config", "HostSelection", "Placement", "Sync"] {
            ready(&workload["status"], condition)?;
        }
        ensure!(
            workload["spec"]["environment"] == namespace
                && workload["status"]["environment"] == namespace,
            "workload is placed in a different runtime environment"
        );
        require_template(&deployment["spec"]["template"]["spec"], &workload["spec"])?;
        let selector = workload["spec"]["hostSelector"]
            .as_object()
            .context("workload hostSelector is required")?;
        ensure!(!selector.is_empty(), "workload hostSelector is empty");
        for (key, value) in selector {
            let label = if key == "hostgroup" {
                "wasmcloud.com/hostgroup"
            } else {
                key.as_str()
            };
            ensure!(
                host_deployment["spec"]["template"]["metadata"]["labels"][label] == *value,
                "workload host selector is not tied to the observed host deployment: {key}"
            );
        }
        let host_id = text(workload, "/status/hostId")?;
        let native = items(hosts)?
            .iter()
            .filter(|host| host["hostId"] == host_id)
            .collect::<Vec<_>>();
        ensure!(
            native.len() == 1,
            "workload placement does not identify exactly one native host"
        );
        let host = native[0];
        ready(&host["status"], "Ready")?;
        ensure!(
            host["environment"] == namespace
                && selector
                    .iter()
                    .all(|(key, value)| host["metadata"]["labels"][key] == *value),
            "native host does not match the workload placement"
        );
        let hostname = text(host, "/hostname")?;
        ensure!(
            pods.iter().any(
                |pod| pod["metadata"]["name"] == hostname || pod["status"]["podIP"] == hostname
            ),
            "workload native host is not one of the observed ready host pods"
        );
        schemas.insert(workload_schema(workload, tenant, environment, namespace)?);
        let identity = identity(workload)?;
        identities.insert(identity.name.clone(), identity);
    }
    ensure!(
        schemas.len() == 1,
        "serving workload replicas disagree on the application schema"
    );
    Ok(PackageWorkload {
        deployment: identity(deployment)?,
        replica_set: identity(replica)?,
        workloads: identities,
        schema: schemas
            .into_iter()
            .next()
            .context("serving schema is absent")?,
    })
}

fn workload_schema(
    workload: &Value,
    tenant: &str,
    environment: &str,
    namespace: &str,
) -> anyhow::Result<String> {
    let mut schemas = BTreeSet::new();
    let http_routing = workload["spec"]["hostInterfaces"]
        .as_array()
        .is_some_and(|interfaces| {
            interfaces.iter().any(|interface| {
                interface["namespace"] == "wamn" && interface["package"] == "flow-http-routing"
            })
        });
    let components = workload["spec"]["components"]
        .as_array()
        .into_iter()
        .flatten();
    let service = workload["spec"].get("service");
    for resource in components.chain(service) {
        let config = &resource["localResources"]["config"];
        // GCP HTTP claims name the runtime namespace; kind HTTP claims name
        // the release environment. The loaded manifest supplies the operation
        // environment in both layouts. Other resources retain their release claim.
        let schema_environment = config.get("wamn.schema").is_some() && http_routing;
        let matching_environment =
            |value: &Value| value == environment || (schema_environment && value == namespace);
        ensure!(
            config
                .get("wamn.tenant")
                .is_none_or(|value| value == tenant)
                && config
                    .get("wamn.environment")
                    .is_none_or(matching_environment),
            "serving workload configuration names a different tenant or environment"
        );
        if config.get("wamn.schema").is_none() {
            continue;
        }
        ensure!(
            config["wamn.tenant"] == tenant && matching_environment(&config["wamn.environment"]),
            "serving workload configuration names a different tenant or environment"
        );
        let schema = config["wamn.schema"]
            .as_str()
            .context("serving wamn.schema must be text")?;
        wamn_schema_control::BareSchemaName::new(schema)?;
        schemas.insert(schema.to_owned());
    }
    ensure!(
        schemas.len() == 1,
        "workload must carry exactly one application schema"
    );
    schemas
        .into_iter()
        .next()
        .context("serving schema is absent")
}

fn require_template(expected: &Value, actual: &Value) -> anyhow::Result<()> {
    match expected {
        Value::Object(fields) => {
            ensure!(
                actual.is_object(),
                "workload spec does not match its deployment template"
            );
            for (key, value) in fields {
                // The runtime omits empty optional collections when copying templates.
                if actual.get(key).is_none()
                    && (value.as_array().is_some_and(Vec::is_empty)
                        || value.as_object().is_some_and(serde_json::Map::is_empty))
                {
                    continue;
                }
                require_template(value, &actual[key])?;
            }
        }
        Value::Array(values) => {
            let observed = actual
                .as_array()
                .context("workload template requires an array")?;
            ensure!(
                values.len() == observed.len(),
                "workload template resource count differs"
            );
            for (value, observed) in values.iter().zip(observed) {
                require_template(value, observed)?;
            }
        }
        _ => ensure!(
            expected == actual,
            "actual workload differs from its deployment template"
        ),
    }
    Ok(())
}

fn host_container(spec: &Value) -> anyhow::Result<&Value> {
    let hosts = spec["containers"]
        .as_array()
        .context("host containers are absent")?
        .iter()
        .filter(|container| container["name"] == "host")
        .collect::<Vec<_>>();
    ensure!(
        hosts.len() == 1,
        "deployment must contain one named host container"
    );
    Ok(hosts[0])
}

fn require_digest(container: &Value, expected: &str) -> anyhow::Result<()> {
    let args = container["args"]
        .as_array()
        .context("host arguments are absent")?;
    let mut found = Vec::new();
    for (index, arg) in args.iter().enumerate() {
        let arg = arg.as_str().context("host argument is not text")?;
        if let Some(value) = arg.strip_prefix("--release-manifest-digest=") {
            found.push(value);
        } else if arg == "--release-manifest-digest" {
            found.push(
                args.get(index + 1)
                    .and_then(Value::as_str)
                    .context("host release digest argument has no value")?,
            );
        }
    }
    ensure!(
        found == [expected],
        "serving host release digest differs from the selected release"
    );
    Ok(())
}

fn identity(value: &Value) -> anyhow::Result<ObjectIdentity> {
    let generation = count(value, "/metadata/generation")?;
    ensure!(generation > 0, "observed generation must be positive");
    let spec = value
        .get("spec")
        .filter(|spec| spec.is_object())
        .context("observed resource has no spec")?;
    Ok(ObjectIdentity {
        name: text(value, "/metadata/name")?.to_owned(),
        uid: text(value, "/metadata/uid")?.to_owned(),
        generation,
        spec_sha256: wamn_execution_contract::canonical_json_sha256(spec),
    })
}

fn object(value: &Value, kind: &str, namespace: &str) -> anyhow::Result<()> {
    ensure!(
        value["kind"] == kind && value["metadata"]["namespace"] == namespace,
        "observed {kind} has the wrong kind or namespace"
    );
    ensure!(
        value["metadata"]["deletionTimestamp"].is_null(),
        "observed {kind} is terminating"
    );
    Ok(())
}

fn owned_by(child: &Value, parent: &Value, kind: &str) -> bool {
    let Some(uid) = parent["metadata"]["uid"]
        .as_str()
        .filter(|uid| !uid.is_empty())
    else {
        return false;
    };
    child["metadata"]["ownerReferences"]
        .as_array()
        .is_some_and(|owners| {
            owners.iter().any(|owner| {
                owner["controller"] == true
                    && owner["kind"] == kind
                    && owner["uid"] == uid
                    && owner["name"] == parent["metadata"]["name"]
            })
        })
}

fn ready(status: &Value, name: &str) -> anyhow::Result<()> {
    ensure!(
        status["conditions"]
            .as_array()
            .is_some_and(|conditions| conditions
                .iter()
                .any(|condition| condition["type"] == name && condition["status"] == "True")),
        "observed resource condition {name} is not true"
    );
    Ok(())
}

fn resource_name(value: &str) -> anyhow::Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 253
            && value.bytes().all(|byte| byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'.'))
            && value.as_bytes()[0].is_ascii_alphanumeric(),
        "invalid Kubernetes resource name {value:?}"
    );
    Ok(())
}

fn text<'a>(value: &'a Value, pointer: &str) -> anyhow::Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("observed resource requires text at {pointer}"))
}

fn count(value: &Value, pointer: &str) -> anyhow::Result<u64> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .with_context(|| format!("observed resource requires an integer at {pointer}"))
}

fn items(value: &Value) -> anyhow::Result<&Vec<Value>> {
    value["items"]
        .as_array()
        .context("observed resource list has no items")
}

fn owners(value: &Value) -> anyhow::Result<&Vec<Value>> {
    value["metadata"]["ownerReferences"]
        .as_array()
        .context("observed resource has no owner references")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct Fixture {
        host: Value,
        replicas: Value,
        pods: Value,
        deployment: Value,
        replica: Value,
        workloads: Value,
        hosts: Value,
    }

    fn resource(kind: &str, name: &str, spec: Value) -> Value {
        let mut resource = json!({
            "kind": kind,
            "metadata": {"name": name, "namespace": "hosts", "uid": name, "generation": 1}
        });
        resource["spec"] = spec;
        resource
    }

    fn owner(kind: &str, name: &str) -> Value {
        json!([{"kind": kind, "name": name, "uid": name, "controller": true}])
    }

    fn conditions(names: &[&str]) -> Value {
        Value::Array(
            names
                .iter()
                .map(|name| json!({"type": name, "status": "True"}))
                .collect(),
        )
    }

    fn fixture() -> Fixture {
        let labels = json!({"wasmcloud.com/hostgroup": "fixture"});
        let host_spec = json!({"containers": [{
            "name": "host", "image": "host-image",
            "args": [format!("--release-manifest-digest={DIGEST}")]
        }]});
        let mut host = resource(
            "Deployment",
            "host",
            json!({
                "replicas": 1, "selector": {"matchLabels": labels},
                "template": {"metadata": {"labels": labels}, "spec": host_spec}
            }),
        );
        host["status"] = json!({"observedGeneration": 1, "replicas": 1, "updatedReplicas": 1, "readyReplicas": 1, "availableReplicas": 1});
        let mut host_replica = resource("ReplicaSet", "host-replica", json!({}));
        host_replica["metadata"]["ownerReferences"] = owner("Deployment", "host");
        let mut pod = resource("Pod", "host-pod", host_spec);
        pod["metadata"]["ownerReferences"] = owner("ReplicaSet", "host-replica");
        pod["metadata"]["labels"] = labels;
        pod["status"] = json!({"phase": "Running", "podIP": "10.0.0.1", "conditions": conditions(&["Ready"]), "containerStatuses": [{"name": "host", "ready": true}]});
        let spec = json!({
            "environment": "hosts", "hostSelector": {"hostgroup": "fixture"},
            "hostInterfaces": [{"namespace": "wamn", "package": "flow-http-routing", "interfaces": ["routing"]}],
            "components": [{"name": "http", "image": "http-image", "localResources": {
                "config": {"wamn.tenant": "tenant", "wamn.environment": "dev", "wamn.schema": "inventory"}
            }}]
        });
        let mut deployment = resource(
            "WorkloadDeployment",
            "http",
            json!({"replicas": 1, "template": {"spec": spec}}),
        );
        deployment["status"] = json!({"conditions": conditions(&["Ready"]), "currentReplicaSet": {"name": "http-replica"}, "currentReplicas": 1, "replicas": {"expected": 1, "current": 1, "ready": 1}});
        let mut replica = resource(
            "WorkloadReplicaSet",
            "http-replica",
            json!({"replicas": 1, "template": {"spec": spec}}),
        );
        replica["metadata"]["ownerReferences"] = owner("WorkloadDeployment", "http");
        let mut workload = resource("Workload", "http-workload", spec);
        workload["metadata"]["ownerReferences"] = owner("WorkloadReplicaSet", "http-replica");
        workload["status"] = json!({"environment": "hosts", "hostId": "native-host", "conditions": conditions(&["Ready", "Config", "HostSelection", "Placement", "Sync"])});
        Fixture {
            host,
            replicas: json!({"items": [host_replica]}),
            pods: json!({"items": [pod]}),
            deployment,
            replica,
            workloads: json!({"items": [workload]}),
            hosts: json!({"items": [{"hostId": "native-host", "hostname": "10.0.0.1", "environment": "hosts", "metadata": {"labels": {"hostgroup": "fixture"}}, "status": {"conditions": conditions(&["Ready"])}}]}),
        }
    }

    fn inspect(fixture: &Fixture) -> anyhow::Result<PackageWorkload> {
        let pods = validate_host(
            &fixture.host,
            &fixture.replicas,
            &fixture.pods,
            "hosts",
            DIGEST,
        )?;
        validate_package(
            &fixture.deployment,
            &fixture.replica,
            &fixture.workloads,
            &fixture.hosts,
            &pods,
            &fixture.host,
            "hosts",
            "tenant",
            "dev",
        )
    }

    #[test]
    fn actual_schema_and_owner_chain_are_required() {
        let original = fixture();
        let evidence = inspect(&original).unwrap();
        assert_eq!(evidence.schema, "inventory");
        for (pointer, replacement) in [
            ("/items/0/metadata/ownerReferences/0/uid", json!("foreign")),
            ("/items/0/metadata/deletionTimestamp", json!("terminating")),
            ("/items/0/status/hostId", json!("foreign")),
            ("/items/0/status/environment", json!("foreign")),
            ("/items/0/status/conditions/1/status", json!("False")),
            (
                "/items/0/spec/components/0/localResources/config/wamn.schema",
                json!("another_schema"),
            ),
            (
                "/items/0/spec/components/0/localResources/config/wamn.tenant",
                json!("another_tenant"),
            ),
        ] {
            let mut changed = fixture();
            if pointer.ends_with("deletionTimestamp") {
                changed.workloads["items"][0]["metadata"]["deletionTimestamp"] = replacement;
            } else {
                *changed.workloads.pointer_mut(pointer).unwrap() = replacement;
            }
            assert!(inspect(&changed).is_err(), "{pointer}");
        }
        let mut changed = fixture();
        changed.replica["metadata"]["ownerReferences"][0]["uid"] = json!("foreign");
        assert!(inspect(&changed).is_err());
        let mut changed = fixture();
        changed.hosts["items"][0]["hostname"] = json!("10.0.0.99");
        assert!(inspect(&changed).is_err());
    }

    #[test]
    fn http_runtime_namespace_does_not_replace_selected_release_environment() {
        let mut gcp = fixture();
        for spec in [
            &mut gcp.deployment["spec"]["template"]["spec"],
            &mut gcp.replica["spec"]["template"]["spec"],
            &mut gcp.workloads["items"][0]["spec"],
        ] {
            spec["components"][0]["localResources"]["config"]["wamn.environment"] = json!("hosts");
        }
        assert_eq!(inspect(&gcp).unwrap().schema, "inventory");
        let original = gcp.workloads["items"][0].clone();
        for (key, value) in [("wamn.environment", "foreign"), ("wamn.tenant", "foreign")] {
            let mut changed = original.clone();
            changed["spec"]["components"][0]["localResources"]["config"][key] = json!(value);
            assert!(workload_schema(&changed, "tenant", "dev", "hosts").is_err());
        }
        let mut changed = original.clone();
        changed["spec"]["hostInterfaces"] = json!([]);
        assert!(workload_schema(&changed, "tenant", "dev", "hosts").is_err());
        let mut changed = original.clone();
        changed["spec"]["components"]
            .as_array_mut()
            .unwrap()
            .push(json!({"localResources": {"config": {"wamn.tenant": "tenant", "wamn.environment": "hosts"}}}));
        assert!(workload_schema(&changed, "tenant", "dev", "hosts").is_err());
        gcp.hosts["items"][0]["environment"] = json!("foreign");
        assert!(inspect(&gcp).is_err());
    }

    #[test]
    fn selected_digest_must_be_running_in_ready_owned_host_pods() {
        for (pointer, replacement) in [
            (
                "/items/0/spec/containers/0/args/0",
                json!("--release-manifest-digest=stale"),
            ),
            ("/items/0/status/containerStatuses/0/ready", json!(false)),
            ("/items/0/metadata/ownerReferences/0/uid", json!("foreign")),
        ] {
            let mut changed = fixture();
            *changed.pods.pointer_mut(pointer).unwrap() = replacement;
            assert!(inspect(&changed).is_err(), "{pointer}");
        }
        let mut changed = fixture();
        changed.host["status"]["updatedReplicas"] = json!(0);
        assert!(inspect(&changed).is_err());
        let mut changed = fixture();
        changed.host["status"]["observedGeneration"] = json!(0);
        assert!(inspect(&changed).is_err());
        let evidence = inspect(&fixture()).unwrap();
        let mut replacement = fixture();
        replacement.pods["items"][0]["metadata"]["uid"] = json!("replacement-pod");
        assert_eq!(inspect(&replacement).unwrap(), evidence);
    }

    #[test]
    fn component_and_service_configuration_require_one_valid_schema() {
        let original = fixture().workloads["items"][0].clone();
        let mut service = original.clone();
        service["spec"]["service"] = service["spec"]["components"][0].clone();
        service["spec"]
            .as_object_mut()
            .unwrap()
            .remove("components");
        assert_eq!(
            workload_schema(&service, "tenant", "dev", "hosts").unwrap(),
            "inventory"
        );
        for replacement in [
            Value::Null,
            json!("invalid.schema"),
            json!("public,inventory"),
        ] {
            let mut changed = original.clone();
            changed["spec"]["components"][0]["localResources"]["config"]["wamn.schema"] =
                replacement;
            assert!(workload_schema(&changed, "tenant", "dev", "hosts").is_err());
        }
        let mut changed = original.clone();
        changed["spec"]["service"] = original["spec"]["components"][0].clone();
        changed["spec"]["service"]["localResources"]["config"]["wamn.schema"] = json!("other");
        assert!(workload_schema(&changed, "tenant", "dev", "hosts").is_err());
    }
}

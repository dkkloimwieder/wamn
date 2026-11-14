//! Platform resource rendering, Helm execution, and bounded Kubernetes application.

use super::document::{ClusterDocument, Provider};
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;
use wamn_kubernetes::kubectl;
use wamn_release_control::release_chart::Target;

/// Render operator event grants in the watched namespace.
///
/// # Errors
/// Returns an error when the manifest cannot be read or decoded.
pub fn runtime_events(
    repository: &Path,
    namespace: &str,
    operator_namespace: &str,
) -> anyhow::Result<Vec<Value>> {
    let mut objects = super::manifests::read_objects(
        repository,
        "deploy/platform/runtime-operator-events-rbac.example.yaml",
    )?;
    for object in &mut objects {
        object["metadata"]["namespace"] = namespace.into();
        if let Some(subjects) = object.get_mut("subjects").and_then(Value::as_array_mut) {
            for subject in subjects {
                subject["namespace"] = operator_namespace.into();
            }
        }
    }
    Ok(objects)
}

/// Render prerequisite objects with the provider's placement rules.
///
/// # Errors
/// Returns an error when the manifest cannot be read or decoded.
pub fn postgres_operator_objects(
    repository: &Path,
    provider: Provider,
) -> anyhow::Result<Vec<Value>> {
    super::manifests::prerequisite_objects(repository, "deploy/infra/cnpg-operator.yaml", provider)
}

/// Render a PostgreSQL Cluster at its owning name and namespace.
///
/// # Errors
/// Returns an error when the manifest cannot be read or decoded.
pub fn postgres_cluster_inputs(
    repository: &Path,
    manifest: &str,
    provider: Provider,
    name: &str,
    namespace: &str,
) -> anyhow::Result<Vec<Value>> {
    let mut objects = super::manifests::prerequisite_objects(repository, manifest, provider)?;
    for object in &mut objects {
        if object["kind"] == "Cluster" {
            object["metadata"]["name"] = name.into();
            object["metadata"]["namespace"] = namespace.into();
        }
    }
    Ok(objects)
}

pub fn runtime_namespaces() -> Vec<Value> {
    ["platform", "identity", "hosts"]
        .map(namespace_object)
        .into()
}

/// Keep the pinned operator chart and its values in one install input.
///
/// # Errors
/// Returns an error when the dependency or its local archive is missing.
pub fn operator_entry(chart: &Path, namespace: &str, values: &Value) -> anyhow::Result<Value> {
    let file = chart.join("Chart.yaml");
    let metadata: Value = serde_yaml::from_slice(
        &std::fs::read(&file)
            .with_context(|| format!("read operator dependency metadata {}", file.display()))?,
    )
    .with_context(|| format!("decode operator dependency metadata {}", file.display()))?;
    let dependency = metadata["dependencies"]
        .as_array()
        .and_then(|dependencies| {
            dependencies
                .iter()
                .find(|entry| entry["name"] == "runtime-operator")
        })
        .context("release chart has no runtime-operator dependency")?;
    let version = dependency["version"]
        .as_str()
        .context("operator dependency has no version")?;
    let archive = chart
        .join("charts")
        .join(format!("runtime-operator-{version}.tgz"));
    ensure!(
        archive.is_file(),
        "missing pinned operator archive {}",
        archive.display()
    );
    Ok(
        json!({"chart": archive, "version": version, "name":"wamn", "namespace":namespace,
        "values": values}),
    )
}

/// Render prerequisite objects with the provider's placement rules.
///
/// # Errors
/// Returns an error when the manifest cannot be read or decoded.
pub fn cert_manager_objects(repository: &Path, provider: Provider) -> anyhow::Result<Vec<Value>> {
    super::manifests::prerequisite_objects(repository, "deploy/infra/cert-manager.yaml", provider)
}

fn webhook_unavailable(message: &str) -> bool {
    message.lines().any(|line| {
        let Some((_, failure)) = line.split_once("failed calling webhook") else {
            return false;
        };
        [
            "connection refused",
            "no endpoints available for service",
            "context deadline exceeded",
            "i/o timeout",
            "TLS handshake timeout",
            "Client.Timeout exceeded",
            "request timed out",
        ]
        .iter()
        .any(|reason| failure.contains(reason))
    })
}

async fn apply_with_webhook_retry<F>(mut apply: impl FnMut() -> F) -> anyhow::Result<()>
where
    F: std::future::Future<Output = anyhow::Result<()>>,
{
    // A finished rollout can precede a serving webhook endpoint. Allow one
    // minute from the initial apply, with two seconds between retries.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut result = tokio::time::timeout_at(deadline, apply())
        .await
        .context("webhook apply deadline expired after 60s")?;
    let mut attempt = 1;
    loop {
        let error = match result {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        let retry_at = tokio::time::Instant::now() + Duration::from_secs(2);
        if !webhook_unavailable(&format!("{error:#}")) || retry_at >= deadline {
            return Err(error);
        }
        attempt += 1;
        tracing::warn!(attempt, error = %error, "Webhook is unavailable, retrying apply");
        tokio::time::sleep_until(retry_at).await;
        if tokio::time::Instant::now() >= deadline {
            return Err(error);
        }
        result = match tokio::time::timeout_at(deadline, apply()).await {
            Ok(result) => result,
            Err(_) => return Err(error.context("webhook apply retry deadline expired after 60s")),
        };
    }
}

pub(crate) async fn helm(
    target: &Target,
    arguments: &[&std::ffi::OsStr],
) -> anyhow::Result<Vec<u8>> {
    let mut args = vec![
        std::ffi::OsStr::new("--kubeconfig"),
        target.kubeconfig.as_os_str(),
        std::ffi::OsStr::new("--kube-context"),
        std::ffi::OsStr::new(&target.context),
    ];
    args.extend_from_slice(arguments);
    wamn_kubernetes::helm::run(&args).await
}

/// Apply one service release and wait for readiness.
pub(super) async fn apply_release(
    target: &Target,
    entry: &Value,
    directory: &Path,
) -> anyhow::Result<()> {
    let name = entry["name"].as_str().context("service release name")?;
    let owner = Target {
        namespace: entry["namespace"]
            .as_str()
            .context("service namespace")?
            .into(),
        ..target.clone()
    };
    wamn_release_control::release_chart::recover_pending(&owner, name).await?;
    upgrade_release(&owner, entry, directory).await
}

/// Upgrade one release after the caller recovers pending operations.
pub(super) async fn upgrade_release(
    target: &Target,
    entry: &Value,
    directory: &Path,
) -> anyhow::Result<()> {
    let name = entry["name"].as_str().context("service release name")?;
    let chart = entry["chart"].as_str().context("service chart")?;
    let values = directory.join(format!("values-{name}.yaml"));
    std::fs::write(&values, serde_yaml::to_string(&entry["values"])?)?;
    wamn_release_control::release_chart::upgrade(
        target,
        Path::new(chart),
        name,
        &values,
        Duration::from_secs(600),
        "platform service",
    )
    .await
}

/// Require the references rendered by this release before changing any resource.
pub(super) async fn require_inputs(target: &Target, entry: &Value) -> anyhow::Result<()> {
    let objects = wamn_kubernetes::helm::template(entry)?;
    crate::environment::runtime::require_references(
        entry["namespace"].as_str().context("service namespace")?,
        &objects,
        async |namespace, type_, name| {
            kubectl::get(
                &target.kubeconfig,
                &target.context,
                namespace,
                &type_.to_ascii_lowercase(),
                name,
            )
            .await
        },
    )
    .await
}

/// Read the values of an existing release, including failed or pending releases.
pub(crate) async fn release_values(
    target: &Target,
    namespace: &str,
    name: &str,
) -> anyhow::Result<Option<Value>> {
    let filter = format!("^{name}$");
    let listed = helm(
        target,
        &[
            "list".as_ref(),
            "--namespace".as_ref(),
            namespace.as_ref(),
            "--filter".as_ref(),
            filter.as_ref(),
            "-o".as_ref(),
            "json".as_ref(),
        ],
    )
    .await?;
    let releases: Vec<Value> = serde_json::from_slice(&listed).context("decode Helm releases")?;
    if !releases.iter().any(|release| release["name"] == name) {
        return Ok(None);
    }
    let values = helm(
        target,
        &[
            "get".as_ref(),
            "values".as_ref(),
            name.as_ref(),
            "--namespace".as_ref(),
            namespace.as_ref(),
            "-o".as_ref(),
            "json".as_ref(),
        ],
    )
    .await?;
    let mut values: Value = serde_json::from_slice(&values).context("decode Helm values")?;
    if values.is_null() {
        values = json!({});
    }
    ensure!(
        values.is_object(),
        "installed Helm values must be an object"
    );
    Ok(Some(values))
}

/// Build a namespace prerequisite.
pub fn namespace_object(namespace: &str) -> Value {
    json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": namespace}})
}

/// Submit the desired object, retrying only transient webhook transport failures.
///
/// # Errors
/// Returns a Kubernetes error with the resource name and namespace.
pub async fn apply_object(target: &Target, object: &Value) -> anyhow::Result<()> {
    let namespace = object["metadata"]["namespace"]
        .as_str()
        .unwrap_or(&target.namespace);
    let name = object["metadata"]["name"]
        .as_str()
        .context("manifest object has no metadata.name")?;
    let type_ = object["kind"]
        .as_str()
        .context("manifest object has no type")?;
    apply_with_webhook_retry(|| {
        kubectl::apply(&target.kubeconfig, &target.context, namespace, object)
    })
    .await
    .with_context(|| format!("apply {type_} {namespace}/{name}"))
}

/// Render operator values from the selected chart's resource payload.
///
/// # Errors
/// Returns unreadable or invalid values and provider substitution errors.
pub fn operator_payload_values(
    payload: &Path,
    document: &ClusterDocument,
) -> anyhow::Result<Value> {
    let mut values = json!({});
    for name in ["values-wamn.yaml", "values-operator.yaml"] {
        let path = payload.join(name);
        let overlay = serde_yaml::from_slice(
            &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        )?;
        super::manifests::merge(&mut values, overlay);
    }
    super::manifests::provider_edits(&mut values, document)?;
    Ok(values)
}

/// The native authority declared by deploy/kind/cluster.k.
pub const LOCAL_REGISTRY: &str = "localhost:5001";

/// The OCI authority used by pods to reach the local registry.
pub const POD_REGISTRY: &str = "wamn-registry.platform.svc.cluster.local:5000";

/// Render the local registry Service and its Docker endpoint.
///
/// # Errors
/// Returns an error when the selected registry manifest cannot be read.
pub fn registry_objects(
    payload: &Path,
    registry: &super::provider::LocalRegistry,
) -> anyhow::Result<Vec<Value>> {
    let mut objects = super::manifests::read_objects(payload, "registry.yaml")?;
    for object in &mut objects {
        if object["kind"] == "EndpointSlice" {
            object["addressType"] = if registry.address.is_ipv4() {
                "IPv4"
            } else {
                "IPv6"
            }
            .into();
            object["ports"][0]["port"] = registry.port.into();
            object["endpoints"] = json!([{"addresses": [registry.address.to_string()], "conditions": {"ready": true}}]);
        }
        if object["kind"] == "Service" {
            object["spec"]["ipFamilies"] = json!([if registry.address.is_ipv4() {
                "IPv4"
            } else {
                "IPv6"
            }]);
        }
    }
    Ok(objects)
}

#[cfg(test)]
mod tests {
    use super::*;
    const WEBHOOK_REFUSED: &str = "failed calling webhook \"webhook.cert-manager.io\" ... dial tcp 10.96.164.197:443: connect: connection refused";
    #[test]
    fn operator_archive_owns_only_the_operator_and_its_targeted_permissions() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let values = super::super::manifests::operator_base_values(&repository).unwrap();
        let entry = operator_entry(
            &repository.join("deploy/platform/release"),
            "platform",
            &values,
        )
        .unwrap();
        let objects = wamn_kubernetes::helm::template(&entry).unwrap();
        let metadata: Value = serde_yaml::from_slice(
            &std::process::Command::new("helm")
                .args(["show", "chart", entry["chart"].as_str().unwrap()])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert_eq!(metadata["version"], entry["version"]);
        let controllers: Vec<_> = objects
            .iter()
            .filter(|object| object["kind"] == "Deployment")
            .collect();
        assert_eq!(
            controllers.len(),
            1,
            "one runtime operator owns the cluster"
        );
        assert_eq!(controllers[0]["metadata"]["namespace"], "platform");
        for object in &objects {
            assert!(
                !matches!(
                    object["kind"].as_str(),
                    Some("StatefulSet" | "WorkloadDeployment")
                ),
                "no NATS or application owner in the operator release"
            );
            if matches!(
                object["kind"].as_str(),
                Some("CustomResourceDefinition" | "ClusterRole" | "ClusterRoleBinding")
            ) {
                assert!(object["metadata"]["namespace"].is_null());
            } else {
                assert!(matches!(
                    object["metadata"]["namespace"].as_str(),
                    Some("platform" | "hosts")
                ));
            }
        }
        let nats =
            super::super::manifests::read_objects(&repository, "deploy/infra/nats-jetstream.yaml")
                .unwrap();
        assert_eq!(
            nats.iter()
                .filter(|object| object["kind"] == "StatefulSet")
                .count(),
            1,
            "direct apply owns the one NATS"
        );
        assert!(
            nats.iter()
                .all(|object| object["metadata"]["namespace"] == "platform")
        );
    }

    #[test]
    fn registry_service_routes_to_the_discovered_docker_endpoint() {
        let payload = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/kind");
        for address in ["172.18.0.4", "fd00::4"] {
            let registry = super::super::provider::LocalRegistry {
                container: "existing-registry".into(),
                address: address.parse().unwrap(),
                port: 5000,
            };
            let objects = registry_objects(&payload, &registry).unwrap();
            let service = objects
                .iter()
                .find(|object| object["kind"] == "Service")
                .unwrap();
            let endpoint = objects
                .iter()
                .find(|object| object["kind"] == "EndpointSlice")
                .unwrap();
            assert_eq!(
                endpoint["metadata"]["labels"]["kubernetes.io/service-name"],
                service["metadata"]["name"]
            );
            assert_eq!(
                endpoint["metadata"]["namespace"],
                service["metadata"]["namespace"]
            );
            assert_eq!(
                endpoint["ports"][0]["name"],
                service["spec"]["ports"][0]["name"]
            );
            assert_eq!(endpoint["ports"][0]["port"], registry.port);
            assert_eq!(endpoint["endpoints"][0]["addresses"], json!([address]));
            assert_eq!(endpoint["addressType"], service["spec"]["ipFamilies"][0]);
            assert!(service["spec"].get("selector").is_none());
            assert_eq!(
                POD_REGISTRY,
                format!(
                    "{}.{}.svc.cluster.local:{}",
                    service["metadata"]["name"].as_str().unwrap(),
                    service["metadata"]["namespace"].as_str().unwrap(),
                    service["spec"]["ports"][0]["port"]
                )
            );
        }
    }

    #[test]
    fn offline_chart_render_refuses_remote_paths_and_wrong_versions() {
        let remote = json!({"chart":"oci://registry.invalid/chart", "version":"1", "name":"test", "namespace":"test", "values":{}});
        assert!(wamn_kubernetes::helm::template(&remote).is_err());
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let operator = operator_entry(
            &repository.join("deploy/platform/release"),
            "platform",
            &json!({}),
        )
        .unwrap();
        let chart = Path::new(operator["chart"].as_str().unwrap());
        let wrong = json!({"chart":chart, "version":"wrong", "name":"test", "namespace":"test", "values":{}});
        assert!(wamn_kubernetes::helm::template(&wrong).is_err());
    }

    #[test]
    fn only_webhook_transport_failures_retry() {
        for message in [
            WEBHOOK_REFUSED,
            // Train 0.1.121 excerpt supplied in the task.
            "failed calling webhook webhook.cert-manager.io: connection refused",
            "Error from server (InternalError): error when creating \"-\": Internal error occurred: failed calling webhook \"mcluster.cnpg.io\": failed to call webhook: Post \"https://cnpg-webhook-service.cnpg-system.svc:443/mutate-postgresql-cnpg-io-v1-cluster?timeout=10s\": no endpoints available for service \"cnpg-webhook-service\"",
            "failed calling webhook \"webhook.cert-manager.io\": failed to call webhook: Post \"https://cert-manager-webhook.cert-manager.svc:443/validate?timeout=30s\": context deadline exceeded",
            "failed calling webhook \"webhook.cert-manager.io\": dial tcp 10.96.164.197:443: i/o timeout",
            "failed calling webhook \"webhook.cert-manager.io\": net/http: TLS handshake timeout",
            "failed calling webhook \"webhook.cert-manager.io\": net/http: request canceled (Client.Timeout exceeded while awaiting headers)",
            "failed calling webhook \"webhook.cert-manager.io\": request timed out",
        ] {
            assert!(webhook_unavailable(message), "{message}");
        }
        for message in [
            "Unable to connect to the server: dial tcp 127.0.0.1:6443: connect: connection refused",
            "Error from server (Forbidden): certificates.cert-manager.io is forbidden",
            "admission webhook \"webhook.cert-manager.io\" denied the request: invalid Certificate",
            "no matches for kind \"Cluster\" in version \"postgresql.cnpg.io/v1\"",
            "failed calling webhook \"webhook.cert-manager.io\": Post \"https://cert-manager-webhook.cert-manager.svc:443/validate?timeout=30s\": x509: certificate signed by unknown authority",
            "connection refused: failed calling webhook \"webhook.cert-manager.io\": EOF",
            "failed calling webhook \"webhook.cert-manager.io\": EOF\nconnection refused",
        ] {
            assert!(!webhook_unavailable(message), "{message}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn webhook_apply_retries_then_succeeds() {
        let started = tokio::time::Instant::now();
        let mut attempts = 0;
        apply_with_webhook_retry(|| {
            attempts += 1;
            std::future::ready(if attempts < 3 {
                Err(anyhow::anyhow!(WEBHOOK_REFUSED).context("apply ClusterIssuer"))
            } else {
                Ok(())
            })
        })
        .await
        .unwrap();
        assert_eq!(attempts, 3);
        assert_eq!(started.elapsed(), Duration::from_secs(4));
    }

    #[tokio::test(start_paused = true)]
    async fn webhook_apply_returns_success_and_other_errors_immediately() {
        let started = tokio::time::Instant::now();
        let mut attempts = 0;
        apply_with_webhook_retry(|| {
            attempts += 1;
            std::future::ready(Ok(()))
        })
        .await
        .unwrap();
        assert_eq!(attempts, 1);
        for transient_first in [false, true] {
            let mut attempts = 0;
            apply_with_webhook_retry(|| {
                attempts += 1;
                std::future::ready(Err(anyhow::anyhow!(if transient_first && attempts == 1 {
                    WEBHOOK_REFUSED
                } else {
                    "Error from server (Forbidden): certificates.cert-manager.io is forbidden"
                })))
            })
            .await
            .unwrap_err();
            assert_eq!(attempts, if transient_first { 2 } else { 1 });
        }
        assert_eq!(started.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn webhook_apply_stops_before_the_retry_deadline() {
        let started = tokio::time::Instant::now();
        let mut attempts = 0;
        apply_with_webhook_retry(|| {
            attempts += 1;
            std::future::ready(Err(anyhow::anyhow!(WEBHOOK_REFUSED)))
        })
        .await
        .unwrap_err();
        assert_eq!(attempts, 30);
        assert_eq!(started.elapsed(), Duration::from_secs(58));
    }

    #[tokio::test(start_paused = true)]
    async fn webhook_apply_deadline_includes_command_time_and_cancels_a_stalled_retry() {
        let started = tokio::time::Instant::now();
        let mut attempts = 0;
        apply_with_webhook_retry(|| {
            attempts += 1;
            let attempt = attempts;
            async move {
                if attempt > 1 {
                    std::future::pending::<()>().await;
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
                Err(anyhow::anyhow!(WEBHOOK_REFUSED))
            }
        })
        .await
        .unwrap_err();
        assert_eq!(attempts, 2);
        assert_eq!(started.elapsed(), Duration::from_secs(60));
    }
}

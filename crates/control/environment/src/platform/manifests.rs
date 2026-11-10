//! Repository manifests and provider edits.

use std::path::Path;

use anyhow::Context as _;
use serde::Deserialize as _;
use serde_json::{Value, json};

use super::document::{ClusterDocument, Provider};

/// Read the runbook's objects and apply the provider's edits.
///
/// # Errors
///
/// Returns an error naming an unreadable or invalid manifest.
pub fn objects(
    repository: &Path,
    path: &str,
    document: &ClusterDocument,
) -> anyhow::Result<Vec<Value>> {
    let hosts = matches!(
        Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
        "wamn-host-serviceaccount.yaml" | "control-status-rbac.yaml"
    );
    let mut objects = read_objects(repository, path)?;
    for object in &mut objects {
        // These are the two substitutions in the runbook's environment templates.
        if hosts || object["metadata"]["namespace"] == "__ENVIRONMENT_NAMESPACE__" {
            object["metadata"]["namespace"] = "hosts".into();
        }
        provider_edits(object, document).with_context(|| format!("render manifest {path}"))?;
        *object = apply_namespace(object.take(), "platform");
    }
    Ok(objects)
}

/// Read Kubernetes objects without namespace or provider substitutions.
///
/// # Errors
/// Returns an error when a manifest cannot be read or decoded.
pub(super) fn read_objects(repository: &Path, path: &str) -> anyhow::Result<Vec<Value>> {
    let path = repository.join(path);
    let source = std::fs::read_to_string(&path)
        .with_context(|| format!("read manifest {}", path.display()))?;
    serde_yaml::Deserializer::from_str(&source)
        .map(|yaml| {
            Value::deserialize(yaml).with_context(|| format!("decode manifest {}", path.display()))
        })
        .filter(|value| !matches!(value, Ok(value) if value.is_null()))
        .collect()
}

/// Read prerequisites with the provider's placement rules.
///
/// # Errors
/// Returns an error when a manifest cannot be read or decoded.
pub(super) fn prerequisite_objects(
    repository: &Path,
    path: &str,
    provider: Provider,
) -> anyhow::Result<Vec<Value>> {
    let mut objects = read_objects(repository, path)?;
    for object in &mut objects {
        if provider == Provider::Kind {
            kind_placement(object);
        }
        *object = apply_namespace(object.take(), "platform");
    }
    Ok(objects)
}

/// The one NATS and its certificates for a cluster that runs the platform in
/// one `namespace`, as the kind tests do (docs/plan/alignment.md R1-R4).
///
/// # Errors
///
/// Returns an error naming an unreadable or invalid manifest.
pub fn one_nats(repository: &Path, namespace: &str) -> anyhow::Result<Vec<Value>> {
    let mut objects = Vec::new();
    for path in [
        "deploy/infra/nats-certificates.yaml",
        "deploy/infra/nats-jetstream.yaml",
    ] {
        let path = repository.join(path);
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("read manifest {}", path.display()))?;
        for yaml in serde_yaml::Deserializer::from_str(&source) {
            let object = Value::deserialize(yaml)
                .with_context(|| format!("decode manifest {}", path.display()))?;
            if !object.is_null() {
                objects.push(in_namespace(object, namespace)?);
            }
        }
    }
    Ok(objects)
}

/// Move a platform, hosts or identity object to `namespace`, and the NATS
/// server name with it. The CA stays in cert-manager, where its ClusterIssuer
/// reads it.
///
/// # Errors
///
/// Returns an error when the object does not encode.
pub fn in_namespace(mut object: Value, namespace: &str) -> anyhow::Result<Value> {
    if matches!(
        object["metadata"]["namespace"].as_str(),
        Some("platform" | "hosts" | "identity")
    ) {
        object["metadata"]["namespace"] = namespace.into();
    }
    let text = serde_json::to_string(&object)?
        .replace("wamn-nats.platform", &format!("wamn-nats.{namespace}"));
    Ok(serde_json::from_str(&text)?)
}

/// Render the platform-owned host account in its release namespace.
///
/// # Errors
/// Returns an error when the manifest cannot be read or decoded.
pub fn host_account_objects(repository: &Path, namespace: &str) -> anyhow::Result<Vec<Value>> {
    Ok(
        read_objects(repository, "deploy/platform/wamn-host-serviceaccount.yaml")?
            .into_iter()
            .map(|object| apply_namespace(object, namespace))
            .collect(),
    )
}

/// Merge the two operator values files in the runbook's order.
///
/// # Errors
///
/// Returns an error naming an unreadable or invalid values file.
pub fn operator_values(repository: &Path, document: &ClusterDocument) -> anyhow::Result<Value> {
    let mut values = operator_base_values(repository)?;
    provider_edits(&mut values, document)?;
    Ok(values)
}

/// Read the shared operator values before namespace and provider substitutions.
///
/// # Errors
/// Returns an error when a values file cannot be read or decoded.
pub fn operator_base_values(repository: &Path) -> anyhow::Result<Value> {
    let mut values = json!({});
    for relative in [
        "deploy/infra/values-wamn.yaml",
        "deploy/platform/values-operator.yaml",
    ] {
        let path = repository.join(relative);
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("read operator values {}", path.display()))?;
        let overlay: Value = serde_yaml::from_str(&source)
            .with_context(|| format!("decode operator values {}", path.display()))?;
        merge(&mut values, overlay);
    }
    Ok(values)
}

pub(super) fn merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                merge(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, overlay) => *base = overlay,
    }
}

pub(super) fn provider_edits(value: &mut Value, document: &ClusterDocument) -> anyhow::Result<()> {
    if value["kind"] == "Certificate" && value["metadata"]["name"] == "operator-dkk" {
        let name = format!("operator-{}", document.org);
        value["metadata"]["name"] = name.clone().into();
        value["spec"]["commonName"] = name.clone().into();
        value["spec"]["secretName"] = name.into();
    }
    if value["kind"] == "ConfigMap" && value["metadata"]["name"] == "wamn-registry-helper" {
        let host = document.registry.split('/').next().unwrap_or_default();
        value["data"]["config.json"] = match document.provider {
            Provider::Gcp => json!({"credHelpers": {host: "wamn"}}),
            Provider::Kind => json!({}),
        }
        .to_string()
        .into();
    }
    if value["kind"] == "Certificate" && value["metadata"]["name"] == "wamn-edge-tls" {
        let domain = document
            .route_hosts
            .control
            .split_once('.')
            .map_or(document.route_hosts.control.as_str(), |(_, domain)| domain);
        value["spec"]["dnsNames"] = json!([format!("*.{domain}")]);
    }
    match document.provider {
        Provider::Gcp => {
            if value["kind"] == "ServiceAccount" && value["metadata"]["name"] == "otel-collector" {
                value["metadata"]["annotations"]["iam.gke.io/gcp-service-account"] =
                    format!("wamn-otel@{}.iam.gserviceaccount.com", document.project_id).into();
            }
        }
        Provider::Kind => {
            kind_placement(value);
            if value["kind"] == "ClusterIssuer" && value["metadata"]["name"] == "letsencrypt" {
                value["metadata"]["name"] = "edge-self-signed".into();
                value["spec"] = json!({"selfSigned": {}});
            }
            if value["kind"] == "Certificate" && value["metadata"]["name"] == "wamn-edge-tls" {
                value["spec"]["issuerRef"]["name"] = "edge-self-signed".into();
            }
            if value["kind"] == "ConfigMap" && value["metadata"]["name"] == "otel-collector" {
                let source = value["data"]["config.yaml"]
                    .as_str()
                    .context("collector ConfigMap has no config.yaml")?;
                let mut config: Value =
                    serde_yaml::from_str(source).context("decode collector config.yaml")?;
                let exporters = config["exporters"]
                    .as_object_mut()
                    .context("collector config.yaml has no exporters")?;
                let bucket_exporters: Vec<_> = exporters
                    .keys()
                    .filter(|name| name.starts_with("google_cloud_storage"))
                    .cloned()
                    .collect();
                for name in &bucket_exporters {
                    exporters.remove(name);
                }
                exporters.insert("debug".into(), json!({}));
                let pipelines = config["service"]["pipelines"]
                    .as_object_mut()
                    .context("collector config.yaml has no pipelines")?;
                for pipeline in pipelines.values_mut() {
                    if let Some(exporters) = pipeline["exporters"].as_array_mut() {
                        for exporter in exporters {
                            if exporter
                                .as_str()
                                .is_some_and(|name| bucket_exporters.iter().any(|key| key == name))
                            {
                                *exporter = "debug".into();
                            }
                        }
                    }
                }
                value["data"]["config.yaml"] = serde_yaml::to_string(&config)
                    .context("encode collector config.yaml")?
                    .into();
            }
        }
    }
    Ok(())
}

fn pool_label(key: &str, value: &Value) -> bool {
    key == "cloud.google.com/gke-nodepool"
        || (key == "role"
            && matches!(
                value.as_str(),
                Some("postgres" | "bench-host" | "main" | "pg")
            ))
}

fn names_pool(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            fields.iter().any(|(key, value)| pool_label(key, value))
                || value["key"].as_str().is_some_and(|key| {
                    pool_label(key, &value["value"])
                        || value["values"]
                            .as_array()
                            .is_some_and(|values| values.iter().any(|v| pool_label(key, v)))
                })
                || fields.values().any(names_pool)
        }
        Value::Array(values) => values.iter().any(names_pool),
        _ => false,
    }
}

fn kind_placement(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.remove("iam.gke.io/gcp-service-account");
            for key in ["storageClass", "storageClassName"] {
                if fields.get(key).is_some_and(|value| value == "premium-rwo") {
                    fields.insert(key.into(), "standard".into());
                }
            }
            if let Some(Value::Object(selector)) = fields.get_mut("nodeSelector") {
                selector.retain(|key, value| !pool_label(key, value));
                if selector.is_empty() {
                    fields.remove("nodeSelector");
                }
            }
            if fields.get("nodeAffinity").is_some_and(names_pool) {
                fields.remove("nodeAffinity");
            }
            if let Some(Value::Array(tolerations)) = fields.get_mut("tolerations") {
                tolerations.retain(|value| !names_pool(value));
                if tolerations.is_empty() {
                    fields.remove("tolerations");
                }
            }
            for value in fields.values_mut() {
                kind_placement(value);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(kind_placement),
        _ => {}
    }
}

/// Retain the namespace supplied by apply for a namespaced object.
pub(super) fn apply_namespace(mut object: Value, namespace: &str) -> Value {
    if object["metadata"]["namespace"].is_null()
        && !matches!(
            object["kind"].as_str(),
            Some(
                "Namespace"
                    | "CustomResourceDefinition"
                    | "ClusterRole"
                    | "ClusterRoleBinding"
                    | "ClusterIssuer"
                    | "MutatingWebhookConfiguration"
                    | "ValidatingWebhookConfiguration"
                    | "PriorityClass"
                    | "StorageClass"
                    | "APIService"
            )
        )
    {
        object["metadata"]["namespace"] = namespace.into();
    }
    object
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(provider: Provider) -> ClusterDocument {
        serde_json::from_value(json!({
            "provider": provider, "project_id": "test-project", "zone": "us-central1-a",
            "cluster_name": "test", "org": "test", "owner_email": "owner@example.com",
            "registry": "localhost:5000", "buckets": {"web": "", "otel": "", "state": ""},
            "route_hosts": {"control": "control.example.com"}, "resend_from": "sender@example.com"
        }))
        .unwrap()
    }

    #[test]
    fn kind_edge_certificate_covers_application_route_names() -> anyhow::Result<()> {
        use tokio_rustls::rustls::{
            client::verify_server_name, pki_types::ServerName, server::ParsedCertificate,
        };

        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let document = ClusterDocument::compile(&repository.join("deploy/kind/cluster.k"))?;
        let mut certificate = objects(&repository, "deploy/gcp/letsencrypt.yaml", &document)?
            .into_iter()
            .find(|object| {
                object["kind"] == "Certificate" && object["metadata"]["name"] == "wamn-edge-tls"
            })
            .context("kind edge certificate")?;
        let dns_names: Vec<String> =
            serde_json::from_value(certificate["spec"]["dnsNames"].take())?;
        let certificate = rcgen::generate_simple_self_signed(dns_names)?;
        let parsed = ParsedCertificate::try_from(certificate.cert.der())?;
        let (_, domain) = document
            .route_hosts
            .control
            .split_once('.')
            .context("control route domain")?;
        let application = format!("application.{domain}");
        // WebPKI enforces the wildcard rule, including the two-label suffix.
        for hostname in [document.route_hosts.control.as_str(), application.as_str()] {
            verify_server_name(&parsed, &ServerName::try_from(hostname)?)
                .with_context(|| format!("kind edge certificate must cover {hostname}"))?;
        }
        Ok(())
    }

    #[test]
    fn kind_removes_pool_placement_but_keeps_other_constraints() {
        let mut value = json!({
            "storageClassName": "premium-rwo",
            "nodeSelector": {"role": "postgres", "kubernetes.io/os": "linux"},
            "affinity": {
                "nodeAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": {"nodeSelectorTerms": [{
                    "matchExpressions": [{"key": "cloud.google.com/gke-nodepool", "operator": "In", "values": ["pg"]}]
                }]}},
                "podAntiAffinity": {}
            },
            "tolerations": [{"key": "role", "value": "postgres"}, {"key": "other", "value": "keep"}],
            "annotations": {"iam.gke.io/gcp-service-account": "remove", "other": "keep"}
        });
        provider_edits(&mut value, &document(Provider::Kind)).unwrap();
        assert_eq!(
            value,
            json!({
                "storageClassName": "standard", "nodeSelector": {"kubernetes.io/os": "linux"},
                "affinity": {"podAntiAffinity": {}}, "tolerations": [{"key": "other", "value": "keep"}],
                "annotations": {"other": "keep"}
            })
        );
    }
}

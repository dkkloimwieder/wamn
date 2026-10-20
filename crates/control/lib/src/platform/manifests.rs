//! Repository manifests and provider edits for install steps 4.3 through 4.5.

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
    let path = repository.join(path);
    let source = std::fs::read_to_string(&path)
        .with_context(|| format!("read manifest {}", path.display()))?;
    let mut objects = Vec::new();
    for yaml in serde_yaml::Deserializer::from_str(&source) {
        let mut object = Value::deserialize(yaml)
            .with_context(|| format!("decode manifest {}", path.display()))?;
        if object.is_null() {
            continue;
        }
        // These are the two substitutions in the runbook's environment templates.
        if object["metadata"]["namespace"] == "__ENVIRONMENT_NAMESPACE__" {
            object["metadata"]["namespace"] = "hosts".into();
        }
        if object["kind"] == "RoleBinding"
            && let Some(subjects) = object["subjects"].as_array_mut()
        {
            for subject in subjects {
                if subject["namespace"] == "wamn-system" {
                    subject["namespace"] = "platform".into();
                }
            }
        }
        provider_edits(&mut object, document)
            .with_context(|| format!("render manifest {}", path.display()))?;
        objects.push(object);
    }
    Ok(objects)
}

/// Merge the two operator values files in the runbook's order.
///
/// # Errors
///
/// Returns an error naming an unreadable or invalid values file.
pub fn operator_values(repository: &Path, document: &ClusterDocument) -> anyhow::Result<Value> {
    let mut values = json!({});
    for relative in [
        "deploy/infra/values-wamn.yaml",
        "deploy/gcp/values-operator.yaml",
    ] {
        let path = repository.join(relative);
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("read operator values {}", path.display()))?;
        let overlay: Value = serde_yaml::from_str(&source)
            .with_context(|| format!("decode operator values {}", path.display()))?;
        merge(&mut values, overlay);
    }
    provider_edits(&mut values, document)?;
    Ok(values)
}

fn merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                merge(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn provider_edits(value: &mut Value, document: &ClusterDocument) -> anyhow::Result<()> {
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

/// Whether all desired fields match, ignoring object fields added by the server.
pub fn already_applied(desired: &Value, live: &Value) -> bool {
    match (desired, live) {
        (Value::Object(desired), Value::Object(live)) => desired.iter().all(|(key, value)| {
            live.get(key)
                .is_some_and(|live| already_applied(value, live))
        }),
        (Value::Array(desired), Value::Array(live)) => {
            desired.len() == live.len()
                && desired.iter().zip(live).all(|(a, b)| already_applied(a, b))
        }
        _ => desired == live,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(provider: Provider) -> ClusterDocument {
        serde_json::from_value(json!({
            "provider": provider, "project_id": "test-project", "zone": "us-central1-a",
            "cluster_name": "test", "org": "test", "owner_email": "owner@example.com",
            "registry": "localhost:5000", "buckets": {"web": "", "otel": ""},
            "route_hosts": {"control": "control.example.com"}, "resend_from": "sender@example.com"
        }))
        .unwrap()
    }

    fn repository() -> &'static Path {
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."))
    }

    fn object<'a>(objects: &'a [Value], kind: &str, name: &str) -> &'a Value {
        objects
            .iter()
            .find(|o| o["kind"] == kind && o["metadata"]["name"] == name)
            .unwrap()
    }

    #[test]
    fn both_providers_render_every_install_manifest() {
        for provider in [Provider::Gcp, Provider::Kind] {
            let document = document(provider);
            let render = |path| objects(repository(), path, &document).unwrap();
            for (path, deployment) in [
                ("deploy/infra/cert-manager.yaml", "cert-manager"),
                ("deploy/infra/cnpg-operator.yaml", "cnpg-controller-manager"),
            ] {
                let objects = render(path);
                assert_eq!(
                    object(&objects, "Deployment", deployment)["spec"]["replicas"],
                    1
                );
            }
            let edge = render("deploy/gcp/letsencrypt.yaml");
            let issuer = if provider == Provider::Kind {
                "edge-self-signed"
            } else {
                "letsencrypt"
            };
            assert_eq!(
                object(&edge, "Certificate", "wamn-edge-tls")["spec"]["issuerRef"]["name"],
                issuer
            );
            let issuer = object(&edge, "ClusterIssuer", issuer);
            if provider == Provider::Kind {
                assert_eq!(issuer["spec"], json!({"selfSigned": {}}));
            } else {
                assert_eq!(
                    issuer["spec"]["acme"]["server"],
                    "https://acme-v02.api.letsencrypt.org/directory"
                );
            }
            let rbac = render("deploy/platform/runtime-operator-events-rbac.example.yaml");
            assert!(rbac.iter().all(|o| o["metadata"]["namespace"] == "hosts"));
            assert_eq!(
                object(&rbac, "RoleBinding", "wamn-runtime-operator-events")["subjects"][0]["namespace"],
                "platform"
            );
            let ca = render("deploy/infra/wasmcloud-ca-issuer.yaml");
            assert_eq!(
                object(&ca, "ClusterIssuer", "wasmcloud-ca")["spec"]["ca"]["secretName"],
                "wasmcloud-ca"
            );
            let certs = render("deploy/platform/host-environment-certs.example.yaml");
            assert_eq!(certs.len(), 2);
            assert!(certs.iter().all(|o| o["metadata"]["namespace"] == "hosts"
                && o["spec"]["issuerRef"]["name"] == "wasmcloud-ca"));
            let collector = render("deploy/platform/otel-collector.yaml");
            let account = object(&collector, "ServiceAccount", "otel-collector");
            let config: Value = serde_yaml::from_str(
                object(&collector, "ConfigMap", "otel-collector")["data"]["config.yaml"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            let pg = render("deploy/gcp/cnpg-cluster.yaml");
            let pg = object(&pg, "Cluster", "wamn-pg");
            if provider == Provider::Kind {
                assert!(
                    account["metadata"]["annotations"]
                        .get("iam.gke.io/gcp-service-account")
                        .is_none()
                );
                for signal in ["traces", "logs"] {
                    assert_eq!(
                        config["service"]["pipelines"][signal]["exporters"],
                        json!(["debug"])
                    );
                }
                assert_eq!(config["exporters"], json!({"debug": {}, "nop": {}}));
                assert_eq!(pg["spec"]["storage"]["storageClass"], "standard");
                assert!(pg["spec"]["affinity"].get("nodeSelector").is_none());
                assert!(pg["spec"]["affinity"].get("tolerations").is_none());
                assert!(!names_pool(&Value::Array(collector.clone())));
            } else {
                assert_eq!(
                    account["metadata"]["annotations"]["iam.gke.io/gcp-service-account"],
                    "wamn-otel@test-project.iam.gserviceaccount.com"
                );
                for signal in ["traces", "logs"] {
                    assert_eq!(
                        config["service"]["pipelines"][signal]["exporters"],
                        json!([format!("google_cloud_storage/{signal}")])
                    );
                }
                assert_eq!(pg["spec"]["storage"]["storageClass"], "premium-rwo");
                assert_eq!(pg["spec"]["affinity"]["nodeSelector"]["role"], "postgres");
            }
            assert_eq!(
                config["service"]["pipelines"]["metrics"]["exporters"],
                json!(["nop"])
            );
            let collector_values = render("deploy/gcp/otel-collector-values.yaml");
            assert_eq!(
                object(&collector_values, "ConfigMap", "otel-collector-values")["data"]["BUCKET"],
                "wamn-dev-otel"
            );
            let values = operator_values(repository(), &document).unwrap();
            assert_eq!(values["operator"]["watchNamespaces"], json!(["hosts"]));
            assert_eq!(values["operator"]["hostNamespaces"], json!(["hosts"]));
            assert_eq!(values["operator"]["allowSharedHosts"], false);
            assert_eq!(values["runtime"]["hostGroups"], json!([]));
            assert_eq!(values["gateway"]["enabled"], false);
            let written = serde_yaml::to_string(&values).unwrap();
            assert_eq!(serde_yaml::from_str::<Value>(&written).unwrap(), values);
        }
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

    #[test]
    fn comparison_ignores_server_fields_but_requires_each_desired_value() {
        let desired = json!({"metadata": {"name": "test"}, "spec": {"containers": [{"name": "test", "image": "v1"}]}});
        let mut live = json!({"metadata": {"name": "test", "uid": "server"}, "spec": {"containers": [{"name": "test", "image": "v1", "imagePullPolicy": "Always"}]}, "status": {"ready": true}});
        assert!(already_applied(&desired, &live));
        live["spec"]["containers"][0]["image"] = "v2".into();
        assert!(!already_applied(&desired, &live));
        assert!(!already_applied(&json!({"missing": null}), &json!({})));
        assert!(!already_applied(&json!([]), &json!([1])));
    }

    #[test]
    fn missing_manifest_names_the_path() {
        let error = objects(
            repository(),
            "deploy/missing-install-manifest.yaml",
            &document(Provider::Kind),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("deploy/missing-install-manifest.yaml")
        );
    }
}

//! Install the Rust web edge with access to the chart-owned release rows.

use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};
use serde::Serialize;
use serde_json::{Value, json};

use super::document::{ClusterDocument, Provider};
use crate::environment::document::EnvironmentDocument;

const NAME: &str = "wamn-edge-web";

#[derive(Serialize)]
struct Configuration<'a> {
    listen: &'static str,
    bucket: String,
    releases: &'a BTreeMap<String, String>,
}

fn routes(
    document: &ClusterDocument,
    environments: &[EnvironmentDocument],
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut routes = BTreeMap::from([(
        document.route_hosts.control.clone(),
        format!("c-{}", document.org),
    )]);
    for environment in environments {
        let release = crate::release_chart::release_name(
            &environment.org,
            &environment.project,
            &environment.env,
        )?;
        ensure!(
            routes
                .insert(environment.route_host.clone(), release)
                .is_none(),
            "duplicate edge route host {}",
            environment.route_host
        );
    }
    Ok(routes)
}

fn objects(
    document: &ClusterDocument,
    routes: &BTreeMap<String, String>,
    chart: &Value,
) -> anyhow::Result<Vec<Value>> {
    let image = chart["platform"]["edge"]["image"]
        .as_str()
        .filter(|image| image.contains("@sha256:"))
        .context("the chart has no pinned edge image")?;
    let configuration = serde_json::to_string(&Configuration {
        listen: "0.0.0.0:8080",
        bucket: if document.buckets.web.is_empty() {
            String::new()
        } else {
            format!("gs://{}", document.buckets.web)
        },
        releases: routes,
    })?;
    let checksum = hex::encode(ring::digest::digest(
        &ring::digest::SHA256,
        configuration.as_bytes(),
    ));
    let mut account = json!({"apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"name": NAME, "namespace": "edge"}});
    if document.provider == Provider::Gcp {
        account["metadata"]["annotations"] = json!({"iam.gke.io/gcp-service-account": format!("wamn-edge@{}.iam.gserviceaccount.com", document.project_id)});
    }
    Ok(vec![
        account,
        json!({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
            "metadata": {"name": NAME, "namespace": "hosts"},
            "rules": [{"apiGroups": [""], "resources": ["configmaps"], "verbs": ["get"], "resourceNames": routes.values().collect::<Vec<_>>()}]}),
        json!({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
            "metadata": {"name": NAME, "namespace": "hosts"},
            "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": NAME},
            "subjects": [{"kind": "ServiceAccount", "name": NAME, "namespace": "edge"}]}),
        json!({"apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": NAME, "namespace": "edge"}, "data": {"web.json": configuration}}),
        json!({"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"name": NAME, "namespace": "edge"},
            "spec": {"replicas": 1, "selector": {"matchLabels": {"app": NAME}},
                "template": {"metadata": {"labels": {"app": NAME}, "annotations": {"checksum/config": checksum}},
                    "spec": {"serviceAccountName": NAME, "containers": [{"name": "edge", "image": image,
                        "args": ["--config", "/etc/wamn-edge/web.json", "web"],
                        "ports": [{"name": "http", "containerPort": 8080}],
                        "readinessProbe": {"httpGet": {"path": "/healthz", "port": "http"}},
                        "resources": {"requests": {"cpu": "50m", "memory": "64Mi"}, "limits": {"memory": "512Mi"}},
                        "volumeMounts": [{"name": "config", "mountPath": "/etc/wamn-edge", "readOnly": true}]}],
                        "volumes": [{"name": "config", "configMap": {"name": NAME}}]}}}}),
        json!({"apiVersion": "v1", "kind": "Service", "metadata": {"name": NAME, "namespace": "edge"},
            "spec": {"selector": {"app": NAME}, "ports": [{"name": "http", "port": 80, "targetPort": "http"}]}}),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> ClusterDocument {
        serde_json::from_value(json!({"provider": "gcp", "project_id": "project", "zone": "zone", "cluster_name": "test",
            "org": "acme", "owner_email": "owner@example.com", "registry": "registry.example.com",
            "buckets": {"web": "web-bucket", "otel": "", "state": ""},
            "route_hosts": {"control": "control.example.com"}, "resend_from": "sender@example.com"})).unwrap()
    }

    #[test]
    fn edge_uses_the_stamped_image_and_can_only_get_its_release_configmaps() {
        let document = document();
        let routes = routes(&document, &[]).unwrap();
        let objects = objects(
            &document,
            &routes,
            &json!({"platform": {"edge": {"image": "registry/edge@sha256:abc"}}}),
        )
        .unwrap();
        assert_eq!(
            objects[0]["metadata"]["annotations"]["iam.gke.io/gcp-service-account"],
            "wamn-edge@project.iam.gserviceaccount.com"
        );
        assert_eq!(objects[1]["metadata"]["namespace"], "hosts");
        assert_eq!(
            objects[1]["rules"],
            json!([{"apiGroups": [""], "resources": ["configmaps"], "verbs": ["get"], "resourceNames": ["c-acme"]}])
        );
        assert_eq!(objects[2]["subjects"][0]["namespace"], "edge");
        let config: Value =
            serde_json::from_str(objects[3]["data"]["web.json"].as_str().unwrap()).unwrap();
        assert_eq!(config["bucket"].as_str(), Some("gs://web-bucket"));
        assert_eq!(
            config["releases"]["control.example.com"].as_str(),
            Some("c-acme")
        );
        let pod = &objects[4]["spec"]["template"]["spec"];
        assert_eq!(pod["containers"][0]["image"], "registry/edge@sha256:abc");
        assert_eq!(pod["serviceAccountName"], NAME);
        assert_eq!(
            objects[5]["spec"]["selector"],
            objects[4]["spec"]["template"]["metadata"]["labels"]
        );
    }
}

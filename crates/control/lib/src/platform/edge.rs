//! Install the Rust web edge with access to the chart-owned release rows.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize as _, Serialize};
use serde_json::{Value, json};

use super::document::{ClusterDocument, Provider};
use super::job::{self, JobRequest};
use crate::environment::document::EnvironmentDocument;
use crate::{kubectl, release_chart::Target};

const NAME: &str = "wamn-edge-web";
const NEG: &str = "wamn-edge";
const NEG_STATUS: &str = "cloud.google.com/neg-status";

fn proxy_values(document: &ClusterDocument, routes: &BTreeMap<String, String>) -> Value {
    json!({
        "tlsSecret": "wamn-edge-tls",
        "identity": "https://identity.identity.svc.cluster.local",
        "identityCaConfigMap": "identity-ca",
        "neg": document.provider == Provider::Gcp,
        "applications": routes.iter().map(|(host, release)| json!({
            "host": host, "api": format!("http://{release}-http.hosts.svc.cluster.local")
        })).collect::<Vec<_>>()
    })
}

/// Reconcile nginx and read the endpoint group that GKE created for its Service.
pub(super) async fn proxy(
    target: &Target,
    document: &ClusterDocument,
    environments: &Path,
    repository: &Path,
    directory: &Path,
    mut progress: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    let environments = super::broker::documents(environments)?
        .into_iter()
        .map(|(_, document)| document)
        .collect::<Vec<_>>();
    let routes = routes(document, &environments)?;
    let values = directory.join("values-edge.yaml");
    std::fs::write(
        &values,
        serde_yaml::to_string(&proxy_values(document, &routes))?,
    )?;
    let chart = repository.join("deploy/platform/edge");
    let edge = Target {
        namespace: "edge".into(),
        ..target.clone()
    };
    let rendered = super::install::helm(
        &edge,
        &[
            "template".as_ref(),
            NEG.as_ref(),
            chart.as_os_str(),
            "--namespace".as_ref(),
            "edge".as_ref(),
            "-f".as_ref(),
            values.as_os_str(),
        ],
    )
    .await?;
    let source = std::str::from_utf8(&rendered)?;
    let objects = serde_yaml::Deserializer::from_str(source)
        .map(Value::deserialize)
        .collect::<Result<Vec<_>, _>>()?;
    let mut changed = false;
    for object in objects.iter().filter(|object| !object.is_null()) {
        let kind = object["kind"].as_str().context("edge manifest kind")?;
        let name = object["metadata"]["name"]
            .as_str()
            .context("edge manifest name")?;
        let live: Option<Value> =
            kubectl::get(&edge.kubeconfig, &edge.context, "edge", kind, name).await?;
        changed |= live
            .as_ref()
            .is_none_or(|live| !super::manifests::already_applied(object, live));
    }
    if changed {
        crate::release_chart::upgrade(
            &edge,
            &chart,
            NEG,
            &values,
            std::time::Duration::from_secs(600),
            "platform edge",
        )
        .await?;
    }
    kubectl::kubectl(
        &edge.kubeconfig,
        &edge.context,
        "edge",
        &[
            "rollout",
            "status",
            "deployment/wamn-edge",
            "--timeout=600s",
        ],
        None,
    )
    .await?;
    if document.provider == Provider::Kind {
        progress("4.20 Edge NEG: skipped: kind")?;
    } else {
        let service: Value = kubectl::get(&edge.kubeconfig, &edge.context, "edge", "service", NEG)
            .await?
            .context("missing edge/wamn-edge Service")?;
        for zone in neg_zones(&service)? {
            progress(&format!("4.20 Edge NEG: {NEG} zone {zone}"))?;
        }
    }
    Ok(changed)
}

fn neg_zones(service: &Value) -> anyhow::Result<Vec<String>> {
    let annotation = service["metadata"]["annotations"][NEG_STATUS]
        .as_str()
        .with_context(|| {
            format!("edge/wamn-edge Service has no {NEG_STATUS} annotation after rollout")
        })?;
    let status: Value = serde_json::from_str(annotation)
        .with_context(|| format!("decode edge/wamn-edge {NEG_STATUS}"))?;
    ensure!(
        status["network_endpoint_groups"]["443"] == NEG,
        "edge/wamn-edge {NEG_STATUS} does not name {NEG} for HTTPS port 443"
    );
    let zones = status["zones"]
        .as_array()
        .context("edge NEG status has no zones")?
        .iter()
        .map(|zone| {
            zone.as_str()
                .map(str::to_owned)
                .context("edge NEG zone is not a string")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure!(!zones.is_empty(), "edge/wamn-edge {NEG_STATUS} has no zone");
    Ok(zones)
}

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

/// Reconcile the rendered objects, then wait even when they were unchanged.
pub(super) async fn apply(
    target: &Target,
    document: &ClusterDocument,
    environments: &Path,
    directory: &Path,
) -> anyhow::Result<bool> {
    let environments = super::broker::documents(environments)?
        .into_iter()
        .map(|(_, document)| document)
        .collect::<Vec<_>>();
    let routes = routes(document, &environments)?;
    let chart: Value =
        serde_yaml::from_slice(&std::fs::read(directory.join("release-chart/values.yaml"))?)?;
    let mut changed = false;
    for object in objects(document, &routes, &chart)? {
        changed |= super::install::apply_object(target, &object).await?;
    }
    kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        "edge",
        &[
            "rollout",
            "status",
            "deployment/wamn-edge-web",
            "--timeout=600s",
        ],
        None,
    )
    .await?;
    Ok(changed)
}

pub(super) async fn check(
    target: &Target,
    document: &ClusterDocument,
    environments: &Path,
    directory: &Path,
    mut progress: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if document.buckets.web.is_empty() {
        return progress("4.20 Edge bucket check: skipped: no bucket");
    }
    let environments = super::broker::documents(environments)?
        .into_iter()
        .map(|(_, document)| document)
        .collect::<Vec<_>>();
    let routes = routes(document, &environments)?;
    let chart: Value =
        serde_yaml::from_slice(&std::fs::read(directory.join("release-chart/values.yaml"))?)?;
    let rendered = objects(document, &routes, &chart)?;
    let configuration = rendered[3]["data"]["web.json"]
        .as_str()
        .context("edge configuration")?;
    let image = chart["platform"]["edge"]["image"]
        .as_str()
        .context("edge image")?;
    let request = JobRequest {
        name: "platform-edge-check".into(),
        image: image.into(),
        refresh: true,
        command: vec!["/usr/local/bin/wamn-edge".into()],
        arguments: ["--config", "/inputs/web.json", "web-check"]
            .map(str::to_owned)
            .into(),
        files: [("web.json".into(), configuration.into())].into(),
        ..JobRequest::default()
    };
    let edge = Target {
        namespace: "edge".into(),
        ..target.clone()
    };
    let started = std::time::Instant::now();
    job::run(&edge, directory, &request, &json!({"routes": routes}), &[]).await?;
    progress(&format!(
        "4.20 Edge bucket check: done {:.1}s",
        started.elapsed().as_secs_f64()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_uses_routes_and_only_gcp_requests_a_neg() {
        let mut document = document();
        let routes = routes(&document, &[]).unwrap();
        let values = proxy_values(&document, &routes);
        assert_eq!(
            values["applications"],
            json!([{
                "host": "control.example.com", "api": "http://c-acme-http.hosts.svc.cluster.local"
            }])
        );
        assert_eq!(values["neg"], true);
        assert_eq!(values["tlsSecret"], "wamn-edge-tls");
        document.provider = Provider::Kind;
        assert_eq!(proxy_values(&document, &routes)["neg"], false);
    }

    #[test]
    #[ignore = "requires: helm"]
    fn proxy_chart_resolves_declared_routes_at_runtime() {
        wamn_test_postgres::require_prerequisites(&["helm"]);
        let document = document();
        let environment = serde_json::from_value(json!({
            "org": "acme", "project": "wms", "env": "partial", "target": "test",
            "release": "none", "route_host": "partial.example.com", "policy": "default",
            "applications": {}
        }))
        .unwrap();
        let routes = routes(&document, &[environment]).unwrap();
        let values = proxy_values(&document, &routes);
        let output = std::process::Command::new("helm")
            .args(["template", NEG])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/platform/edge"))
            .args(["--namespace", "edge", "--set-json", &values.to_string()])
            .output()
            .expect("render edge chart");
        assert!(
            output.status.success(),
            "helm template: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let objects = serde_yaml::Deserializer::from_slice(&output.stdout)
            .map(Value::deserialize)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let config = objects
            .iter()
            .find(|object| object["kind"] == "ConfigMap")
            .unwrap()["data"]["nginx.conf"]
            .as_str()
            .unwrap();
        assert!(config.contains("resolver kube-dns.kube-system.svc.cluster.local;"));
        for (index, (host, release)) in routes.iter().enumerate() {
            assert!(config.contains(&format!(
                "upstream api_{index} {{\n    zone api_{index} 64k;\n    server {release}-http.hosts.svc.cluster.local resolve;"
            )));
            let server = config
                .split("  server {")
                .find(|server| server.contains(&format!("server_name {host};")))
                .unwrap();
            assert!(server.contains("location /api/ {"));
            assert!(server.contains(&format!("proxy_pass http://api_{index}/;")));
            assert!(!server.contains("rewrite "));
            assert!(server.contains(&format!(
                "proxy_redirect http://{release}-http.hosts.svc.cluster.local/ /api/;"
            )));
            assert!(server.contains("proxy_set_header Host $host;"));
            assert!(server.contains("proxy_pass https://identity.identity.svc.cluster.local;"));
            assert!(server.contains("proxy_ssl_verify on;"));
            assert!(server.contains("proxy_pass http://wamn-edge-web.edge.svc.cluster.local;"));
            assert!(server.contains("location = /healthz {\n      return 200;"));
        }
    }

    #[test]
    fn neg_status_requires_the_https_group_and_a_zone() {
        assert!(neg_zones(&json!({})).is_err());
        let mut status =
            json!({"network_endpoint_groups": {"443": "wamn-edge"}, "zones": ["us-central1-a"]});
        let service =
            |status: &Value| json!({"metadata": {"annotations": {NEG_STATUS: status.to_string()}}});
        assert_eq!(neg_zones(&service(&status)).unwrap(), ["us-central1-a"]);
        status["zones"] = json!([]);
        assert!(neg_zones(&service(&status)).is_err());
        status["zones"] = json!(["us-central1-a"]);
        status["network_endpoint_groups"]["443"] = "other".into();
        assert!(neg_zones(&service(&status)).is_err());
    }

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
    #[tokio::test]
    async fn empty_bucket_reports_the_skip_without_reading_inputs() {
        let mut document = document();
        document.provider = Provider::Kind;
        document.buckets.web.clear();
        let target = Target {
            kubeconfig: "/absent".into(),
            context: "absent".into(),
            namespace: "platform".into(),
        };
        let mut lines = Vec::new();
        check(
            &target,
            &document,
            Path::new("/absent"),
            Path::new("/absent"),
            |line| {
                lines.push(line.to_owned());
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(lines, ["4.20 Edge bucket check: skipped: no bucket"]);
        let objects = objects(
            &document,
            &routes(&document, &[]).unwrap(),
            &json!({"platform": {"edge": {"image": "registry/edge@sha256:abc"}}}),
        )
        .unwrap();
        assert!(objects[0]["metadata"]["annotations"].is_null());
        let configuration: Value =
            serde_json::from_str(objects[3]["data"]["web.json"].as_str().unwrap()).unwrap();
        assert_eq!(configuration["bucket"], "");
        let mut live = objects[4].clone();
        live["metadata"]["resourceVersion"] = "123".into();
        assert!(super::super::manifests::already_applied(&objects[4], &live));
        live["spec"]["template"]["spec"]["containers"][0]["image"] = "old".into();
        assert!(!super::super::manifests::already_applied(
            &objects[4],
            &live
        ));
    }
}

//! Focused contracts of retained platform resources.

use serde_json::json;
use std::path::Path;
use wamn_environment_control::platform::{identity, resources};

#[test]
fn identity_chart_mounts_only_the_selected_credentials() -> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    for mode in [
        identity::Mode::Foundation,
        identity::Mode::Session {
            targets: vec!["fixture-target".into()],
            nats: None,
        },
    ] {
        let foundation = matches!(mode, identity::Mode::Foundation);
        let entry = identity::chart_input(&identity::ChartInput {
            chart: &repository.join("deploy/platform/identity"),
            name: "fixture-identity",
            namespace: "fixture-identity",
            image: "wamn-identity:fixture",
            pull_policy: "Never",
            issuer: "https://fixture-identity.fixture-identity.svc.cluster.local",
            database_secret: "fixture-db",
            tls_secret: "fixture-tls",
            operator_ca_secret: "",
            resend_secret: "",
            resend_from: "",
            invite_url: "",
            mode,
        })?;
        let rendered = resources::helm_template(&entry)?;
        assert_eq!(entry["values"]["authNats"]["url"], "");
        assert_eq!(
            entry["values"]["sessionTargetSecrets"],
            if foundation {
                json!([])
            } else {
                json!(["fixture-target"])
            }
        );
        let deployment = rendered
            .iter()
            .find(|object| object["kind"] == "Deployment")
            .unwrap();
        let volumes = deployment["spec"]["template"]["spec"]["volumes"]
            .as_array()
            .unwrap();
        let variables = deployment["spec"]["template"]["spec"]["containers"][0]["env"]
            .as_array()
            .unwrap();
        assert!(
            variables
                .iter()
                .any(|variable| variable["name"] == "WAMN_IDENTITY_DATABASE_URL"
                    && variable["valueFrom"]["secretKeyRef"]["name"] == "fixture-db")
        );
        assert!(
            volumes
                .iter()
                .any(|volume| volume["secret"]["secretName"] == "fixture-tls")
        );
        assert_eq!(
            volumes
                .iter()
                .any(|volume| volume["secret"]["secretName"] == "fixture-target"),
            !foundation
        );
    }
    Ok(())
}

#[test]
fn platform_edge_uses_the_selected_image_and_existing_certificate() -> anyhow::Result<()> {
    use wamn_environment_control::platform::{
        document::{ClusterDocument, Provider},
        edge,
    };

    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut document = ClusterDocument::compile(&repository.join("deploy/kind/cluster.k"))?;
    for provider in [Provider::Kind, Provider::Gcp] {
        document.provider = provider;
        document.buckets.web = if provider == Provider::Gcp {
            "fixture-web".into()
        } else {
            String::new()
        };
        let entry = edge::chart_input(
            &repository.join("deploy/platform/edge"),
            "registry.invalid/edge@sha256:fixture",
            &document,
        )?;
        let objects = resources::helm_template(&entry)?;
        let certificates = edge::certificates(&repository.join("deploy/gcp"), &document)?;
        let certificate = certificates
            .iter()
            .find(|object| object["kind"] == "Certificate")
            .unwrap();
        assert_eq!(certificate["metadata"]["namespace"], "hosts");
        assert!(!objects.iter().any(|object| object["kind"] == "Certificate"));
        let deployments: Vec<_> = objects
            .iter()
            .filter(|object| object["kind"] == "Deployment")
            .collect();
        assert_eq!(deployments.len(), 1);
        let deployment = deployments[0];
        assert_eq!(deployment["metadata"]["namespace"], "hosts");
        let pod = &deployment["spec"]["template"]["spec"];
        assert!(
            pod["volumes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|volume| volume["secret"]["secretName"] == certificate["spec"]["secretName"])
        );
        let containers = pod["containers"].as_array().unwrap();
        let routes = containers
            .iter()
            .find(|container| container["image"] == entry["values"]["web"]["image"])
            .unwrap();
        assert_eq!(routes["readinessProbe"]["httpGet"]["path"], "/healthz");
        let role = objects
            .iter()
            .find(|object| object["kind"] == "Role")
            .unwrap();
        assert_eq!(role["metadata"]["namespace"], "hosts");
        assert_eq!(
            role["rules"],
            json!([{"apiGroups":[""], "resources":["configmaps"], "verbs":["get", "list"]}])
        );
        let binding = objects
            .iter()
            .find(|object| object["kind"] == "RoleBinding")
            .unwrap();
        assert_eq!(binding["subjects"][0]["name"], pod["serviceAccountName"]);
        assert_eq!(binding["roleRef"]["name"], role["metadata"]["name"]);
        let config = objects
            .iter()
            .find(|object| object["kind"] == "ConfigMap")
            .unwrap();
        let web: serde_json::Value =
            serde_json::from_str(config["data"]["web.json"].as_str().unwrap())?;
        assert_eq!(
            web,
            json!({"listen":"0.0.0.0:8080", "bucket": if document.buckets.web.is_empty() { "" } else { "gs://fixture-web" }})
        );
        let nginx = config["data"]["nginx.conf"].as_str().unwrap();
        assert!(nginx.contains("proxy_pass http://127.0.0.1:8080;"));
        assert!(nginx.contains("proxy_ssl_verify on;"));
        assert!(nginx.contains("proxy_ssl_trusted_certificate /etc/wamn-edge/identity-ca/ca.crt;"));
        let service = objects
            .iter()
            .find(|object| object["kind"] == "Service")
            .unwrap();
        assert_eq!(
            service["metadata"]["annotations"]["cloud.google.com/neg"].is_string(),
            provider == Provider::Gcp
        );
    }
    Ok(())
}

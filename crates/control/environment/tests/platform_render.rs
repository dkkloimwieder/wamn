//! Focused contracts of retained platform resources.

use serde_json::json;
use std::path::Path;
use wamn_environment_control::platform::identity;

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
        let rendered = wamn_kubernetes::helm::template(&entry)?;
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
        let objects = wamn_kubernetes::helm::template(&entry)?;
        let certificates = edge::certificates(&repository.join("deploy/gcp"), &document)?;
        let certificate = certificates
            .iter()
            .find(|object| object["kind"] == "Certificate")
            .unwrap();
        assert!(!objects.iter().any(|object| object["kind"] == "Certificate"));
        let deployments: Vec<_> = objects
            .iter()
            .filter(|object| object["kind"] == "Deployment")
            .collect();
        assert_eq!(deployments.len(), 1);
        let deployment = deployments[0];
        assert_eq!(deployment["metadata"]["namespace"], "hosts");
        assert_eq!(
            certificate["metadata"]["namespace"],
            deployment["metadata"]["namespace"]
        );
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
        let account = objects
            .iter()
            .find(|object| object["kind"] == "ServiceAccount")
            .unwrap();
        assert_eq!(account["metadata"]["name"], pod["serviceAccountName"]);
        assert_eq!(
            account["metadata"]["namespace"],
            deployment["metadata"]["namespace"]
        );
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
        if provider == Provider::Gcp {
            let project = wamn_environment_control::platform::project::ProjectDocument::compile(
                &repository.join("deploy/gcp/project.k"),
            )?;
            let infrastructure: serde_json::Value =
                serde_json::from_slice(&wamn_schema_generator::render_project_tofu(
                    &project.canonical_json()?,
                    "state",
                    "fixture",
                )?)?;
            let neg: serde_json::Value = serde_json::from_str(
                service["metadata"]["annotations"]["cloud.google.com/neg"]
                    .as_str()
                    .unwrap(),
            )?;
            let group = &infrastructure["data"]["google_compute_network_endpoint_group"]["edge"];
            for port in service["spec"]["ports"].as_array().unwrap() {
                assert_eq!(
                    group["name"],
                    neg["exposed_ports"][port["port"].to_string()]["name"]
                );
            }
            assert_eq!(group["zone"], project.zone);
        }
    }
    Ok(())
}

#[test]
fn operation_worker_bindings_use_their_own_accounts_and_namespace_resources() -> anyhow::Result<()>
{
    use wamn_environment_control::{
        environment::document::EnvironmentDocument, platform::operations,
    };
    let document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
        "org":"acme","project":"billing","env":"dev","target":"cloud","policy":"dev","applications":{}
    }))?)?;
    let cluster = "database-owner";
    let database = "recorded-environment-database";
    let objects = operations::target_objects(&document, cluster, database)?;
    for account in objects
        .iter()
        .filter(|object| object["kind"] == "ServiceAccount")
    {
        assert_eq!(account["metadata"]["namespace"], "platform");
        let name = account["metadata"]["name"].as_str().unwrap();
        let privileged = name.ends_with("-sql");
        for binding in objects
            .iter()
            .filter(|object| object["kind"] == "RoleBinding" && object["metadata"]["name"] == name)
        {
            assert_eq!(binding["subjects"][0]["name"], name);
            assert_eq!(
                binding["subjects"][0]["namespace"],
                account["metadata"]["namespace"]
            );
            let role = objects
                .iter()
                .find(|object| {
                    object["kind"] == "Role"
                        && object["metadata"]["name"] == binding["roleRef"]["name"]
                        && object["metadata"]["namespace"] == binding["metadata"]["namespace"]
                })
                .unwrap();
            assert!(matches!(
                role["metadata"]["namespace"].as_str(),
                Some("platform" | "hosts")
            ));
            for rule in role["rules"].as_array().unwrap() {
                if role["metadata"]["namespace"] == "hosts" {
                    let resources = rule["resources"].as_array().unwrap();
                    let verbs = rule["verbs"].as_array().unwrap();
                    if resources.iter().any(|resource| resource == "secrets") {
                        assert!(!verbs.iter().any(|verb| verb == "watch"));
                        if verbs.iter().any(|verb| verb == "patch") {
                            assert!(privileged, "only SQL credential handoff patches Secrets");
                        }
                    }
                    if resources.iter().any(|resource| resource == "configmaps")
                        && verbs.iter().any(|verb| verb == "update")
                    {
                        assert!(!privileged, "SQL preserves the canonical selection");
                        assert_eq!(
                            rule["resourceNames"],
                            json!([name.strip_suffix("-code").unwrap()])
                        );
                    }
                }
                for field in ["resources", "verbs", "apiGroups"] {
                    assert!(
                        !rule[field]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|value| value == "*")
                    );
                }
                if rule["apiGroups"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|group| group == "postgresql.cnpg.io")
                {
                    assert_eq!(rule["resourceNames"], json!([database]));
                    assert!(
                        !rule["verbs"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|verb| verb == "create" || verb == "delete")
                    );
                }
                if role["metadata"]["namespace"] == "platform"
                    && rule["resources"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|value| value == "secrets")
                {
                    assert!(
                        privileged,
                        "a code worker can read platform administrator Secrets"
                    );
                    assert!(
                        rule["resourceNames"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|value| value == &format!("{cluster}-superuser"))
                    );
                }
            }
        }
        assert_eq!(
            objects.iter().any(|object| object["kind"] == "Role"
                && object["metadata"]["namespace"] == "platform"
                && object["metadata"]["name"] == name),
            privileged
        );
    }
    Ok(())
}

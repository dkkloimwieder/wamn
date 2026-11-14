//! Focused contracts of retained platform resources.

use serde_json::json;
use std::path::Path;
use wamn_environment::platform::identity;

#[test]
fn identity_chart_mounts_only_the_selected_credentials() -> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    for mode in [
        identity::Mode::Foundation,
        identity::Mode::Session {
            targets: vec![identity::SessionTargetMount {
                name: "fixture-target".into(),
                connection: wamn_postgres::ConnectionConfig {
                    host: "dedicated-rw.fixture-identity.svc".into(),
                    port: 5432,
                    database: "environment".into(),
                    username: "reader".into(),
                    credentials: wamn_postgres::SecretRef {
                        namespace: "fixture-identity".into(),
                        name: "reader-credential".into(),
                    },
                    server_ca: wamn_postgres::SecretRef {
                        namespace: "fixture-identity".into(),
                        name: "dedicated-ca".into(),
                    },
                },
            }],
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
        let role = rendered
            .iter()
            .find(|object| object["kind"] == "Role")
            .unwrap();
        let names = role["rules"][0]["resourceNames"].as_array().unwrap();
        assert_eq!(names.iter().any(|name| name == "dedicated-ca"), !foundation);
        assert_eq!(
            names.iter().any(|name| name == "reader-credential"),
            !foundation
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
                    && variable["valueFrom"]["configMapKeyRef"]["name"]
                        == "fixture-db-configuration")
        );
        assert!(
            volumes
                .iter()
                .any(|volume| volume["secret"]["secretName"] == "fixture-tls")
        );
        assert_eq!(
            volumes
                .iter()
                .any(|volume| volume["configMap"]["name"] == "fixture-target"),
            !foundation
        );
    }
    Ok(())
}

#[test]
fn platform_edge_uses_the_selected_image_and_existing_certificate() -> anyhow::Result<()> {
    use wamn_environment::platform::{
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
        if provider == Provider::Kind {
            let ingress = objects
                .iter()
                .find(|object| object["kind"] == "Ingress")
                .unwrap();
            assert_eq!(
                ingress["spec"]["tls"][0]["secretName"],
                certificate["spec"]["secretName"]
            );
            assert_eq!(
                ingress["spec"]["defaultBackend"]["service"]["name"],
                deployment["metadata"]["name"]
            );
        }
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
        assert_eq!(web["identity"], entry["values"]["identity"]);
        assert!(web["identity_ca"].is_string());
        let service = objects
            .iter()
            .find(|object| object["kind"] == "Service")
            .unwrap();
        assert_eq!(
            service["metadata"]["annotations"]["cloud.google.com/neg"].is_string(),
            provider == Provider::Gcp
        );
        if provider == Provider::Gcp {
            let project = wamn_environment::platform::project::ProjectDocument::compile(
                &repository.join("deploy/gcp/project.k"),
            )?;
            let infrastructure: serde_json::Value =
                serde_json::from_slice(&wamn_environment::platform::tofu::render_project_tofu(
                    &project, "state", "fixture",
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
    use wamn_environment::{environment::document::EnvironmentDocument, platform::operations};
    let document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
        "org":"acme","project":"billing","env":"dev","target":"cloud","policy":"dev","revocation_seconds":60,"applications":{}
    }))?)?;
    let cluster = "database-owner";
    let database = "recorded-environment-database";
    let objects = operations::target_objects(&document, cluster, database)?;
    let mut identities = std::collections::BTreeSet::new();
    for object in &objects {
        assert!(
            identities.insert((
                object["apiVersion"].to_string(),
                object["kind"].to_string(),
                object["metadata"]["namespace"].to_string(),
                object["metadata"]["name"].to_string()
            )),
            "duplicate resource identity would replace its rules"
        );
    }
    for account in objects
        .iter()
        .filter(|object| object["kind"] == "ServiceAccount")
    {
        assert_eq!(account["metadata"]["namespace"], document.org);
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
            assert_eq!(role["metadata"]["namespace"], document.org);
            for rule in role["rules"].as_array().unwrap() {
                if role["metadata"]["namespace"] == document.org {
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
                    assert!(privileged);
                    let target = if rule["resources"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|value| value == "databases")
                    {
                        database
                    } else {
                        cluster
                    };
                    assert_eq!(rule["resourceNames"], json!([target]));
                    assert!(
                        !rule["verbs"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|verb| verb == "create" || verb == "delete")
                    );
                }
                if rule["resourceNames"].as_array().is_some()
                    && rule["resources"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|value| value == "secrets")
                {
                    assert!(privileged, "only a SQL worker reads administrator Secrets");
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
                && object["metadata"]["name"] == name
                && object["rules"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|rule| rule["apiGroups"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|value| value == "postgresql.cnpg.io"))),
            privileged
        );
    }
    Ok(())
}

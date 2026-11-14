//! Focused contracts of retained platform resources.

use crate::platform::identity;
use serde_json::json;
use std::path::Path;

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
    use crate::platform::{
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
        let certificate_entry = crate::environment::entry(
            &repository.join("deploy/platform/installation"),
            "platform",
            "wamn",
            json!({"org":document.org,"registry":document.registry,"edgeDnsNames":["*.example.com"]}),
        )?;
        let certificates = wamn_kubernetes::helm::template(&certificate_entry)?;
        let certificate = certificates
            .iter()
            .find(|object| {
                object["kind"] == "Certificate"
                    && object["spec"]["secretName"] == entry["values"]["tlsSecret"]
            })
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
            .find(|object| object["kind"] == "ClusterRole")
            .unwrap();
        assert!(role["metadata"]["namespace"].is_null());
        assert_eq!(
            role["rules"],
            json!([{"apiGroups":[""], "resources":["configmaps"], "verbs":["get", "list"]}])
        );
        let binding = objects
            .iter()
            .find(|object| object["kind"] == "ClusterRoleBinding")
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
    }
    Ok(())
}

#[test]
fn operation_worker_bindings_use_their_own_accounts_and_namespace_resources() -> anyhow::Result<()>
{
    use crate::platform::operations;
    let document = wamn_registry::Triple::new("acme", "dev", "billing");
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let cluster = "database-owner";
    let database = "recorded-environment-database";
    let entry = crate::environment::entry(
        &repository.join("deploy/platform/environment"),
        &document.org,
        "fixture",
        operations::values(&document, cluster, database)?,
    )?;
    let objects = wamn_kubernetes::helm::template(&entry)?;
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

#[test]
fn org_chart_uses_each_recovery_owners_policy_and_preserves_role_references() -> anyhow::Result<()>
{
    let template = wamn_control_provision::Template::standard();
    let (org, _) = template.stamp("acme", "wamn-pg");
    let mut values = crate::org::values(&org, &template.policies)?;
    let cluster = values["clusters"][0]["name"].as_str().unwrap().to_owned();
    let role = wamn_control_provision::connection::managed_role(
        "application".into(),
        "wamn_app",
        "application-login".into(),
    );
    values["managedRoles"] = json!({cluster: [role]});
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut entry = crate::environment::entry(
        &repository.join("deploy/platform/org"),
        &org.id,
        "org-acme",
        values,
    )?;
    let prepared = crate::platform::artifact_plan::test_util::prepared_chart(&repository)?;
    let chart = crate::platform::artifact_plan::select(&prepared.0)?;
    let mut document = crate::platform::document::ClusterDocument::compile(
        &repository.join("deploy/kind/cluster.k"),
    )?;
    document.org.clone_from(&org.id);
    let entry = crate::platform::installation::organization_entry(
        &document,
        &chart,
        Some(entry["values"].take()),
    )?;
    let objects = wamn_kubernetes::helm::template(&entry)?;
    assert!(
        objects
            .iter()
            .filter(|object| object["kind"] == "Cluster")
            .flat_map(|object| object["spec"]["managed"]["roles"].as_array().unwrap())
            .all(|role| role["name"] != "application")
    );
    for input in entry["values"]["clusters"].as_array().unwrap() {
        let cluster = objects
            .iter()
            .find(|object| {
                object["kind"] == "Cluster" && object["metadata"]["name"] == input["name"]
            })
            .unwrap();
        assert_eq!(cluster["metadata"]["namespace"], org.id);
        assert_eq!(cluster["spec"]["instances"], input["policy"]["instances"]);
        assert_eq!(
            cluster["spec"]["storage"]["size"],
            input["policy"]["storage"]
        );
        if let Some(roles) =
            entry["values"]["managedRoles"][input["name"].as_str().unwrap()].as_array()
        {
            for role in roles {
                assert!(
                    cluster["spec"]["managed"]["roles"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|declared| declared["name"] == role["name"]
                            && declared["passwordSecret"] == role["passwordSecret"])
                );
            }
        }
    }
    Ok(())
}

#[test]
fn recovery_refuses_the_source_shared_clusters_and_missing_policies() -> anyhow::Result<()> {
    let template = wamn_control_provision::Template::standard();
    let (org, _) = template.stamp("acme", "wamn-pg");
    let prod = wamn_registry::Env::new("prod");
    let source = wamn_registry::name::cluster(&org.id, &prod);
    assert!(crate::org::recovery_values(&org, &prod, &template.policies, &source, None).is_err());
    assert!(
        crate::org::recovery_values(
            &org,
            &wamn_registry::Env::new("absent"),
            &template.policies,
            "restored",
            None
        )
        .is_err()
    );
    let (pooled, _) = wamn_control_provision::Template::trials().stamp("acme", "wamn-pg");
    assert!(
        crate::org::recovery_values(&pooled, &prod, &template.policies, "restored", None).is_err()
    );
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut values = crate::org::values(&org, &template.policies)?;
    let mut recovery =
        crate::org::recovery_values(&org, &prod, &template.policies, "restored", None)?;
    values["clusters"]
        .as_array_mut()
        .unwrap()
        .append(recovery["clusters"].as_array_mut().unwrap());
    let entry = crate::environment::entry(
        &repository.join("deploy/platform/org"),
        &org.id,
        "recovery",
        values,
    )?;
    let objects = wamn_kubernetes::helm::template(&entry)?;
    for cluster in objects.iter().filter(|object| object["kind"] == "Cluster") {
        if cluster["metadata"]["name"] == "restored" {
            assert_eq!(cluster["spec"]["bootstrap"]["recovery"]["source"], source);
            assert_eq!(cluster["spec"]["externalClusters"][0]["name"], source);
        } else {
            assert!(cluster["spec"]["bootstrap"]["recovery"].is_null());
            assert!(cluster["spec"]["bootstrap"]["initdb"].is_object());
        }
    }
    Ok(())
}

#[test]
fn non_gcp_ingress_has_a_default_class_and_ports_reachable_from_kind() -> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let entry = crate::environment::entry(
        &repository.join("deploy/infra/chart"),
        "platform",
        "wamn-infrastructure",
        json!({"traefik":{"enabled":true}}),
    )?;
    let objects = wamn_kubernetes::helm::template(&entry)?;
    let class = objects
        .iter()
        .find(|object| {
            object["kind"] == "IngressClass"
                && object["metadata"]["annotations"]["ingressclass.kubernetes.io/is-default-class"]
                    == "true"
        })
        .unwrap();
    let service = objects
        .iter()
        .find(|object| {
            object["kind"] == "Service" && object["metadata"]["name"] == class["metadata"]["name"]
        })
        .unwrap();
    let cluster = crate::platform::provider::kind_configuration(Path::new("/var/tmp/registry"))?;
    let mappings = cluster["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|node| node["extraPortMappings"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    for port in service["spec"]["ports"].as_array().unwrap() {
        assert!(
            mappings
                .iter()
                .any(|mapping| mapping["containerPort"] == port["nodePort"]
                    && mapping["hostPort"] == port["port"])
        );
    }
    Ok(())
}

#[test]
fn colliding_coordinates_read_only_their_own_credentials() -> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let coordinates = [("a", "b-c", "d"), ("a", "b", "c-d")];
    let accounts = coordinates
        .map(|(org, _project, env)| wamn_registry::name::environment_service_account(org, env));
    let secrets = coordinates
        .map(|(org, project, env)| wamn_registry::credential_secret_name(org, project, env));
    assert_ne!(accounts[0], accounts[1]);
    assert_ne!(secrets[0], secrets[1]);
    for index in 0..coordinates.len() {
        let entry = crate::environment::entry(
            &repository.join("deploy/platform/environment"),
            coordinates[index].0,
            &accounts[index],
            json!({"credentialAccess":{"account":accounts[index],"secrets":[secrets[index]]}}),
        )?;
        let objects = wamn_kubernetes::helm::template(&entry)?;
        let binding = objects
            .iter()
            .find(|object| object["kind"] == "RoleBinding")
            .unwrap();
        let role = objects
            .iter()
            .find(|object| {
                object["kind"] == "Role" && object["metadata"]["name"] == binding["roleRef"]["name"]
            })
            .unwrap();
        for subject in binding["subjects"].as_array().unwrap() {
            assert_eq!(subject["name"], accounts[index]);
            assert_eq!(subject["namespace"], role["metadata"]["namespace"]);
        }
        for rule in role["rules"].as_array().unwrap().iter().filter(|rule| {
            rule["resources"]
                .as_array()
                .is_some_and(|resources| resources.iter().any(|resource| resource == "secrets"))
        }) {
            let allowed = rule["resourceNames"].as_array().unwrap();
            assert!(allowed.iter().any(|name| name == &secrets[index]));
            assert!(!allowed.iter().any(|name| name == &secrets[1 - index]));
            assert!(
                rule["verbs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|verb| verb == "get")
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn installation_preserves_broker_mounts_and_gcp_telemetry() -> anyhow::Result<()> {
    use crate::platform::{artifact_plan, document::ClusterDocument, installation};
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let prepared = artifact_plan::test_util::prepared_chart(&repository)?;
    let chart = artifact_plan::select(&prepared.0)?;
    let document = ClusterDocument::compile(&repository.join("deploy/gcp/cluster.k"))?;
    let values = installation::values(&document, &chart).await?;
    let objects = wamn_kubernetes::helm::template(&installation::platform_entry(&chart, values)?)?;
    let broker = objects
        .iter()
        .find(|object| object["kind"] == "StatefulSet" && object["metadata"]["name"] == "wamn-nats")
        .unwrap();
    let pod = &broker["spec"]["template"]["spec"];
    for container in pod["containers"].as_array().unwrap() {
        for mount in container["volumeMounts"].as_array().unwrap() {
            assert!(
                pod["volumes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|volume| volume["name"] == mount["name"])
                    || broker["spec"]["volumeClaimTemplates"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|claim| claim["metadata"]["name"] == mount["name"])
            );
        }
    }
    let collector = objects
        .iter()
        .find(|object| {
            object["kind"] == "Deployment" && object["metadata"]["name"] == "otel-collector"
        })
        .unwrap();
    let account = objects
        .iter()
        .find(|object| {
            object["kind"] == "ServiceAccount"
                && object["metadata"]["name"]
                    == collector["spec"]["template"]["spec"]["serviceAccountName"]
        })
        .unwrap();
    assert!(
        account["metadata"]["annotations"]["iam.gke.io/gcp-service-account"]
            .as_str()
            .is_some_and(
                |name| name.ends_with(&format!("@{}.iam.gserviceaccount.com", document.project_id))
            )
    );
    let config: serde_json::Value = serde_yaml::from_str(
        objects
            .iter()
            .find(|object| {
                object["kind"] == "ConfigMap" && object["metadata"]["name"] == "otel-collector"
            })
            .unwrap()["data"]["relay"]
            .as_str()
            .unwrap(),
    )?;
    for signal in ["traces", "logs"] {
        for exporter in config["service"]["pipelines"][signal]["exporters"]
            .as_array()
            .unwrap()
        {
            let exporter = &config["exporters"][exporter.as_str().unwrap()];
            assert_eq!(exporter["sending_queue"]["storage"], "file_storage");
            assert!(exporter["bucket"]["name"].is_string());
        }
    }
    let queue = objects
        .iter()
        .find(|object| {
            object["kind"] == "PersistentVolumeClaim"
                && collector["spec"]["template"]["spec"]["volumes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|volume| {
                        volume["persistentVolumeClaim"]["claimName"] == object["metadata"]["name"]
                    })
        })
        .unwrap();
    assert_eq!(
        queue["metadata"]["namespace"],
        collector["metadata"]["namespace"]
    );
    let operator = objects.iter().find(|object| {
        object["kind"] == "Deployment"
            && object["spec"]["template"]["spec"]["containers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|container| {
                    container["args"].as_array().is_some_and(|args| {
                        args.iter().any(|arg| arg == "-allow-shared-hosts=false")
                    })
                })
    });
    assert!(operator.is_some());
    Ok(())
}

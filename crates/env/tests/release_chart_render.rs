//! Renders the app, org, project and platform charts to check their release objects.
//!
//! These tests need only the local Helm executable.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/pod_resources.rs"]
mod pod_resources;
#[path = "support/pod_security.rs"]
mod pod_security;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("wamn-release-chart-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn helm(arguments: &[&str]) -> std::process::Output {
    Command::new("helm")
        .args(arguments)
        .output()
        .expect("run helm")
}

#[test]
fn project_role_names_its_secrets_and_release_resources_and_binds_helm_storage()
-> anyhow::Result<()> {
    use serde_json::json;
    use wamn_env::env::chart_values::set_operation_values;
    use wamn_kubernetes::{helm, platform_env::entry};
    use wamn_registry::{Triple, name};

    let triple = Triple::new("acme".parse()?, "dev".parse()?, "billing".parse()?);
    let namespace = name::env_key(&triple.org, &triple.env);
    let storage = helm::org_release_namespace(&namespace);
    let connection =
        wamn_postgres::database::connection_for(&triple, &"abcdefgh".parse()?, &namespace);
    let cluster = name::project_cluster(&connection.database);
    let app = "shop".parse()?;
    let mut values = json!({"namespace": namespace});
    set_operation_values(
        &mut values,
        &triple,
        &cluster,
        &connection.database,
        [&app].into_iter(),
    )?;
    let directory = repository_root().join("deploy/charts/project");
    let entry = entry(&directory, &storage, "fixture", values)?;
    let objects = helm::template(&entry)?;
    let storage_binding = objects
        .iter()
        .find(|object| {
            object["kind"] == "RoleBinding" && object["metadata"]["namespace"] == storage.as_ref()
        })
        .expect("project account must be bound in Helm storage");
    assert_eq!(storage_binding["roleRef"]["name"], "wamn-helm-releases");
    let subject = &storage_binding["subjects"][0];
    assert_eq!(subject["kind"], "ServiceAccount");
    assert_eq!(subject["namespace"], namespace);
    let workload_binding = objects
        .iter()
        .find(|object| {
            object["kind"] == "RoleBinding"
                && object["metadata"]["namespace"] == namespace
                && object["subjects"][0] == *subject
        })
        .expect("the same project account must be bound in the workload namespace");
    let role = objects
        .iter()
        .find(|object| {
            object["kind"] == "Role"
                && object["metadata"]["name"] == workload_binding["roleRef"]["name"]
        })
        .expect("the project Role must exist");
    let rules = role["rules"].as_array().unwrap();
    assert!(rules.iter().all(|rule| {
        rule["resources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|resource| {
                !matches!(
                    resource.as_str(),
                    Some("roles" | "rolebindings" | "deployments")
                )
            })
    }));
    let operation = &entry["values"]["operations"];
    let rules_for = |resource: String| {
        rules.iter().filter(move |rule| {
            rule["resources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == resource.as_str())
        })
    };
    let names = |resource: &str, verb: &str| {
        rules_for(resource.to_owned())
            .filter(|rule| rule["verbs"].as_array().unwrap().iter().any(|v| v == verb))
            .map(|rule| rule["resourceNames"].clone())
            .collect::<Vec<_>>()
    };
    let secret_rules = rules_for("secrets".into()).collect::<Vec<_>>();
    assert_eq!(secret_rules.len(), 1);
    assert_eq!(secret_rules[0]["verbs"], json!(["get"]));
    assert_eq!(secret_rules[0]["resourceNames"], operation["loginSecrets"]);
    assert_eq!(
        names("workloaddeployments", "patch"),
        [operation["workloads"].clone()]
    );
    assert_eq!(names("services", "patch"), [operation["workloads"].clone()]);
    assert_eq!(
        names("networkpolicies", "delete"),
        [operation["appNetworkPolicies"].clone()]
    );
    assert_eq!(
        names("configmaps", "patch"),
        [json!([operation["phaseConfigMap"]])]
    );
    for verb in ["create", "list", "watch", "delete"] {
        assert!(names("configmaps", verb).is_empty());
    }
    assert!(objects.iter().any(|object| object["kind"] == "ConfigMap"
        && object["metadata"]["name"] == operation["phaseConfigMap"]));
    Ok(())
}

#[test]
fn an_unstamped_chart_does_not_render() {
    let scratch = Scratch::new("unstamped");
    let chart = scratch.0.join("chart");
    let status = Command::new("cp")
        .args(["-r"])
        .arg(repository_root().join("deploy/charts/app"))
        .arg(&chart)
        .status()
        .expect("copy the chart");
    assert!(status.success());
    let output = helm(&["template", "x", chart.to_str().expect("utf-8 path")]);
    assert!(!output.status.success());
}

#[test]
fn the_control_route_uses_service_port_80_and_the_pod_uses_8080() -> anyhow::Result<()> {
    use wamn_kubernetes::web_release::{LiveRelease, WEB_RELEASE_ANNOTATION};

    let root = repository_root();
    let ui = wamn_digest::bytes_sha256(b"platform UI");
    let org: wamn_registry::OrgId = "a"
        .repeat(wamn_registry::identifiers::MAX_PROJECT_ID_LEN)
        .parse()?;
    let rendered = objects(&helm(&[
        "template",
        "acme",
        root.join("deploy/charts/org").to_str().unwrap(),
        "-f",
        root.join("crates/env/tests/support/org-chart-values.json")
            .to_str()
            .unwrap(),
        "--set-string",
        &format!("control.platformUi={ui}"),
        "--set-string",
        &format!("org={org}"),
    ]))?;
    let name = format!("{}-http", wamn_registry::name::control_resource(&org));
    let service = object(&rendered, "Service", &name)?;
    let namespace = service["metadata"]["namespace"]
        .as_str()
        .expect("Service namespace");
    let route: LiveRelease = serde_json::from_str(
        service["metadata"]["annotations"][WEB_RELEASE_ANNOTATION]
            .as_str()
            .expect("control Service route"),
    )?;
    assert_eq!(
        route.api,
        format!("http://{name}.{namespace}.svc.cluster.local")
    );
    assert_eq!(route.host, format!("{org}.wamn.localhost"));
    assert_eq!(route.org, org);
    assert_eq!(route.platform_ui, Some(ui));
    assert!(route.project.is_none());
    assert!(
        rendered
            .iter()
            .filter(|object| object["kind"] == "ConfigMap")
            .all(|object| {
                object["data"].get("route.json").is_none()
                    && object["data"].get("release.json").is_none()
            })
    );

    let port = &service["spec"]["ports"][0];
    assert_eq!(port["port"].as_u64(), Some(80));
    assert_eq!(port["targetPort"].as_str(), Some("http"));
    let pod = &object(
        &rendered,
        "Deployment",
        &wamn_registry::name::control_resource(&org),
    )?["spec"]["template"];
    let port = pod["spec"]["containers"][0]["ports"]
        .as_array()
        .expect("control pod ports")
        .iter()
        .find(|container_port| container_port["name"] == port["targetPort"])
        .expect("Service target port");
    assert_eq!(port["containerPort"].as_u64(), Some(8080));
    Ok(())
}

#[test]
fn control_host_and_capture_render_in_their_namespaces() -> anyhow::Result<()> {
    use wamn_env::env::{chart_values, lifecycle};
    use wamn_env::platform_env::database_values;
    use wamn_kubernetes::{helm, platform_env::entry};
    use wamn_postgres::database::{capture_configuration, connection_for};
    use wamn_registry::{Triple, name};

    let charts = repository_root().join("deploy/charts");
    let org_values = serde_json::from_str(include_str!("support/org-chart-values.json"))?;
    let org = helm::template(&entry(&charts.join("org"), "acme", "acme", org_values)?)?;
    pod_resources::assert_pod_resources(&org);
    pod_security::assert_restricted_pods(
        org.iter().filter(|object| object["kind"] == "Deployment"),
    );

    let triple = Triple::new("acme".parse()?, "dev".parse()?, "billing".parse()?);
    let namespace = name::env_key(&triple.org, &triple.env);
    let instance = "abcdefgh".parse()?;
    let connection = connection_for(&triple, &instance, &namespace);
    let storage = helm::org_release_namespace(&namespace);
    let host = wamn_env::env::ensure::host_values(
        &triple.org,
        &triple.env,
        &namespace,
        &format!("registry.example/wamn-host:test@sha256:{}", "a".repeat(64)),
        &serde_json::Map::new(),
        300,
        &std::iter::once((
            triple.project.to_string(),
            wamn_env::env::ensure::host_project_values(&triple, &connection)?,
        ))
        .collect(),
        "tls://wamn-nats.platform.svc:4222",
        "https://identity.platform.svc",
        false,
        true,
        &[],
        &serde_json::json!({"provider":"local"}),
        "workflow",
        "ingress",
    );
    let host = helm::template(&entry(&charts.join("env"), &storage, "fixture", host)?)?;
    let deployment = host
        .iter()
        .find(|object| object["kind"] == "Deployment")
        .expect("host group Deployment");
    let container = &deployment["spec"]["template"]["spec"]["containers"][0];
    let namespace_variable = container["env"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|env| {
            if env["valueFrom"]["fieldRef"]["fieldPath"] == "metadata.namespace" {
                env["name"].as_str()
            } else {
                None
            }
        })
        .expect("pod namespace environment variable");
    assert!(
        container["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg == &format!("--environment=$({namespace_variable})"))
    );
    for object in host {
        assert_eq!(
            object["metadata"]["namespace"], namespace,
            "{}",
            object["kind"]
        );
        if let Some(settings) = object["data"]["settings.json"].as_str() {
            let settings: serde_json::Value = serde_json::from_str(settings)?;
            assert_eq!(settings["namespace"], namespace);
        }
    }
    let cluster = name::project_cluster(&connection.database);
    let publication = name::cdc_object_name(&triple.org, &triple.project, &triple.env, &instance);
    let mut project = database_values(
        &connection,
        1,
        serde_json::json!({
            "instances": 1, "storage": "2Gi", "cpu": "100m", "memory": "256Mi",
            "backup": {"retention": "7d", "cadence": "0 0 0 * * *"}
        }),
        &serde_json::json!({
            "image": "registry.example/postgres:test",
            "backup": {
                "endpoint": "https://storage.example", "bucket": "backup", "secret": "s3-login"
            }
        }),
    )?;
    project["publication"] = chart_values::publication_values(
        &triple,
        &instance,
        &cluster,
        &publication,
        &["app".parse()?],
    )["publication"]
        .take();
    chart_values::set_operation_values(
        &mut project,
        &triple,
        &cluster,
        &connection.database,
        std::iter::empty(),
    )?;
    project["cdcReader"] = lifecycle::reader_values(
        &triple,
        &capture_configuration(connection)?,
        "registry.example/wamn-host:test",
        "nats://nats.platform.svc:4222",
        false,
    );
    let nats_public_key = nkeys::KeyPair::new_user().public_key();
    project["cdcReader"]["natsPublicKey"] = nats_public_key.as_str().into();
    let project = helm::template(&entry(
        &charts.join("project"),
        &storage,
        "fixture",
        project,
    )?)?;
    let cdc_reader = &project
        .iter()
        .find(|object| {
            object["kind"] == "Deployment"
                && object["metadata"]["name"]
                    == name::cdc_reader_name(&triple.org, &triple.project, &triple.env)
        })
        .expect("CDC reader Deployment")["spec"]["template"];
    assert_eq!(
        cdc_reader["metadata"]["annotations"]["wamn.dev/cdc-nats-user"],
        nats_public_key
    );
    pod_resources::assert_pod_resources(&project);
    let store = project
        .iter()
        .find(|object| object["kind"] == "ObjectStore")
        .expect("a backup-enabled project must render an ObjectStore");
    pod_resources::assert_compute_resources(
        &store["spec"]["instanceSidecarConfiguration"]["resources"],
        "backup sidecar",
    );
    for object in &project {
        let storage_binding =
            object["kind"] == "RoleBinding" && object["roleRef"]["name"] == "wamn-helm-releases";
        let expected = if storage_binding {
            storage.as_ref()
        } else {
            namespace.as_str()
        };
        assert_eq!(
            object["metadata"]["namespace"], expected,
            "{}",
            object["kind"]
        );
        if object["kind"] == "RoleBinding"
            && object["subjects"][0]["name"] == name::control_service_account(&triple.org)
        {
            assert_eq!(object["subjects"][0]["namespace"], triple.org.as_str());
        }
        if let Some(settings) = object["data"]["settings.json"].as_str() {
            let settings: serde_json::Value = serde_json::from_str(settings)?;
            let database = settings["database"]["database"]
                .as_str()
                .expect("CDC database");
            assert_eq!(
                settings["database"]["host"],
                name::project_database_host(database, &namespace)
            );
        }
    }
    pod_security::assert_restricted_pods(
        project
            .iter()
            .filter(|object| object["kind"] == "Deployment"),
    );
    Ok(())
}

#[test]
fn an_app_service_carries_its_workload_route_and_static_pins() -> anyhow::Result<()> {
    use serde_json::json;
    use wamn_env::app_chart::plan::{release_name, workload_name};
    use wamn_kubernetes::{
        helm,
        platform_env::entry,
        web_release::{LiveRelease, WEB_RELEASE_ANNOTATION},
    };
    use wamn_registry::{Triple, name};

    let triple = Triple::new("acme".parse()?, "dev".parse()?, "shop".parse()?);
    let app = "app".parse()?;
    let release = release_name(&triple.org, &triple.project, &triple.env, &app);
    let namespace = name::env_key(&triple.org, &triple.env);
    let host = name::app_host("app", &triple.org, &"example.com".parse()?);
    let digest = wamn_digest::bytes_sha256(b"release");
    let web = wamn_digest::bytes_sha256(b"bundle");
    let values = json!({
        "namespace":namespace, "org":triple.org, "project":triple.project,
        "platform":{
            "roles":{"ingress":{"image":format!("registry.example/ingress@{digest}")}},
            "workflow":{"image":format!("registry.example/workflow@{digest}")},
        },
        "release":{"manifestDigest":digest}, "env":name::host_status(&triple.org, &triple.project, &triple.env),
        "routeHost":host, "apps":{(app.as_str()):{}}, "route":{"config":{"wamn.project_env_key":"key"}},
        "webClients":{(host.as_str()):"client"}, "webDigests":{(host.as_str()):web}, "hostGroup":"host",
    });
    let chart = repository_root().join("deploy/charts/app");
    let rendered = helm::template(&entry(&chart, &namespace, &release, values)?)?;
    let service = object(&rendered, "Service", &workload_name(&release))?;
    let metadata = &service["metadata"];
    let route: LiveRelease = serde_json::from_str(
        metadata["annotations"][WEB_RELEASE_ANNOTATION]
            .as_str()
            .expect("app Service route"),
    )?;
    assert_eq!(
        route.api,
        format!(
            "http://{}.{}.svc.cluster.local",
            workload_name(&release),
            metadata["namespace"].as_str().unwrap()
        ),
    );
    assert_eq!(route.host, host);
    assert_eq!(route.digest, digest);
    assert_eq!(route.org, triple.org);
    assert_eq!(route.project.as_ref(), Some(&triple.project));
    assert_eq!(route.clients[&host].as_str(), "client");
    assert_eq!(route.webs[&host], web);
    assert!(rendered.iter().all(|object| object["kind"] != "ConfigMap"));
    Ok(())
}

/// The integration egress policy opens exactly the sibling `egressPort`s and
/// the `connectionPorts`, and renders only when one of them is declared.
#[test]
fn the_egress_policy_opens_each_declared_port() {
    let scratch = Scratch::new("egress");
    let chart = repository_root().join("deploy/charts/app");
    let digest = format!("sha256:{}", "a".repeat(64));
    let sibling = || {
        serde_json::json!({
            "name": "sibling",
            "image": format!("registry.example/sibling@{digest}"),
            "authority": "api.example.com:9443",
            "egressPort": 9443,
        })
    };
    let cases = [
        (vec![], vec![]),
        (vec![], vec![443, 8443]),
        (vec![sibling()], vec![]),
        (vec![sibling()], vec![443, 9443]),
    ];
    for (components, connection_ports) in cases {
        let expected: BTreeSet<u64> = components
            .iter()
            .map(|component| component["egressPort"].as_u64().expect("egressPort"))
            .chain(connection_ports.iter().copied())
            .collect();
        let values = serde_json::json!({
            "namespace": "acme--dev",
            "platform": {
                "roles": {"ingress": {"image": format!("registry.example/ingress@{digest}")}},
                "workflow": {"image": format!("registry.example/workflow@{digest}")},
            },
            "release": {"manifestDigest": digest},
            "env": "acme/shop/dev",
            "routeHost": "app--acme.example.com",
            "apps": {"app": {}},
            "route": {"config": {"wamn.project_env_key": "key"}},
            "components": components,
            "connectionPorts": connection_ports,
            "hostGroup": "host",
        });
        let file = scratch.0.join("values.json");
        std::fs::write(&file, values.to_string()).expect("write values");
        let output = helm(&[
            "template",
            "x",
            chart.to_str().expect("utf-8 path"),
            "-f",
            file.to_str().expect("utf-8 path"),
        ]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let policies: Vec<serde_yaml::Value> = serde_yaml::Deserializer::from_slice(&output.stdout)
            .map(|document| serde::Deserialize::deserialize(document).expect("YAML document"))
            .filter(|document: &serde_yaml::Value| document["kind"] == "NetworkPolicy")
            .collect();
        let ports: BTreeSet<u64> = policies
            .iter()
            .flat_map(|policy| policy["spec"]["egress"].as_sequence().expect("egress"))
            .flat_map(|rule| rule["ports"].as_sequence().expect("ports"))
            .map(|port| port["port"].as_u64().expect("port"))
            .collect();
        assert_eq!(ports, expected);
        assert_eq!(policies.len(), usize::from(!expected.is_empty()));
    }
}

#[test]
fn platform_and_org_releases_wire_the_public_roots() -> anyhow::Result<()> {
    use anyhow::Context as _;
    use serde_json::{Value, json};

    let root = repository_root();
    let scratch = Scratch::new("roots");
    let chart = root.join("deploy/charts/platform");
    let digest = format!("sha256:{}", "a".repeat(64));
    for provider in ["kind", "gcp"] {
        let values = chart.join(format!("values-{provider}.yaml"));
        let output = helm(&[
            "template",
            "wamn",
            chart.to_str().unwrap(),
            "-n",
            "platform",
            "-f",
            values.to_str().unwrap(),
            "--set-json",
            r#"registryEgress=[{"cidr":"192.0.2.1/32","port":5000}]"#,
            "--set-string",
            &format!("appChart=oci://example.invalid/chart@{digest}"),
            "--set-string",
            &format!("systemSql.image=example.invalid/control@{digest}"),
        ]);
        let platform = objects(&output)?;
        let edge = object(&platform, "ClusterRole", "wamn-edge")?;
        let rules = edge["rules"].as_array().context("edge rules")?;
        for resource in ["namespaces", "services"] {
            let rule = rules
                .iter()
                .find(|rule| {
                    rule["apiGroups"] == json!([""])
                        && rule["resources"]
                            .as_array()
                            .is_some_and(|resources| resources.iter().any(|name| name == resource))
                })
                .with_context(|| format!("edge {resource} rule"))?;
            assert_eq!(rule["verbs"], json!(["list", "watch"]));
        }
        assert!(rules.iter().all(
            |rule| rule["resources"].as_array().is_some_and(|resources| {
                resources
                    .iter()
                    .all(|name| name != "configmaps" && name != "*")
            })
        ));
        let database = object(&platform, "ConfigMap", "wamn-system-sql")?["data"]["database.json"]
            .as_str()
            .context("system SQL configuration")?;
        let database = serde_json::from_str(database)?;
        let inputs = &object(&platform, "ConfigMap", "wamn-org-inputs")?["data"];
        let org = "acme".parse()?;
        let namespace = wamn_registry::name::env_key(&org, &"dev".parse()?);
        let login = "wamn_apply_env_0000000000000000000000000000000000000000".parse()?;
        let [worker, _] = wamn_platform_install::env_install::worker_configuration(
            inputs,
            &database,
            &org,
            &namespace,
            &login,
            &[json!({"cidr":"192.0.2.2/32","port":6443})],
        )?;
        assert_eq!(worker["metadata"]["namespace"], namespace);
        let worker_settings: Value = serde_json::from_str(
            worker["data"]["settings.json"]
                .as_str()
                .context("worker settings")?,
        )?;
        assert_eq!(
            worker_settings["apply_database"]["username"],
            login.as_str()
        );
        assert_eq!(
            worker_settings["apply_database"]["credentials"]["namespace"],
            namespace
        );
        assert!(
            worker_settings.get("database").is_none(),
            "worker settings expose control credentials"
        );
        let org_values = wamn_platform_install::org::org_values(
            inputs,
            &database,
            &org,
            &[json!({"cidr":"192.0.2.2/32","port":6443})],
        )?;
        let values = scratch.0.join("org.json");
        std::fs::write(&values, serde_json::to_vec(&org_values)?)?;
        let org_chart = root.join("deploy/charts/org");
        let org = objects(&helm(&[
            "template",
            "wamn-org",
            org_chart.to_str().unwrap(),
            "-n",
            "wamn-helm-acme",
            "-f",
            values.to_str().unwrap(),
        ]))?;
        let settings = &org_values["control"]["configuration"];
        assert!(settings.get("apply_database").is_none());
        assert!(settings.get("apply_transaction_host").is_none());
        for (host, client, mode, namespace_label) in [
            (
                &settings["database"]["host"],
                "control",
                "transaction",
                "wamn.dev/org",
            ),
            (
                &worker_settings["apply_database"]["host"],
                "worker",
                "session",
                "wamn.dev/org-workloads",
            ),
            (
                &worker_settings["apply_transaction_host"],
                "worker",
                "transaction",
                "wamn.dev/org-workloads",
            ),
        ] {
            let host = host.as_str().context("system database host")?;
            let (name, _) = host.split_once('.').context("Pooler service hostname")?;
            let pooler = object(&platform, "Pooler", name)?;
            assert_eq!(pooler["spec"]["pgbouncer"]["poolMode"], mode);
            let tls = pooler["spec"]["pgbouncer"]["clientTLSSecret"]["name"]
                .as_str()
                .context("Pooler TLS certificate")?;
            let dns = object(&platform, "Certificate", tls)?["spec"]["dnsNames"]
                .as_array()
                .context("Pooler certificate DNS names")?;
            assert!(dns.contains(&json!(host)));
            let ingress = platform
                .iter()
                .find(|object| {
                    object["kind"] == "NetworkPolicy"
                        && object["spec"]["podSelector"]["matchLabels"]["cnpg.io/poolerName"]
                            == name
                })
                .context("Pooler ingress policy")?;
            assert_eq!(
                ingress["spec"]["ingress"][0]["from"][0]["podSelector"]["matchLabels"]["app.kubernetes.io/component"],
                format!("wamn-{client}")
            );
            assert_eq!(
                ingress["spec"]["ingress"][0]["from"][0]["namespaceSelector"]["matchLabels"]
                    [namespace_label],
                "true"
            );
            if client == "control" {
                let egress = object(&org, "NetworkPolicy", "allow-control-to-postgres")?;
                assert!(
                    egress["spec"]["egress"]
                        .as_array()
                        .context("SQL egress")?
                        .iter()
                        .any(|rule| {
                            rule["to"][0]["podSelector"]["matchLabels"]["cnpg.io/poolerName"]
                                == name
                        })
                );
            }
        }
        let hosts = object(&platform, "Namespace", "hosts")?;
        let namespace = org
            .iter()
            .find(|object| object["kind"] == "Namespace")
            .context("org namespace")?;
        let root_secrets: BTreeSet<_> = platform
            .iter()
            .filter(|object| object["kind"] == "Certificate" && object["spec"]["isCA"] == true)
            .map(|certificate| {
                assert_eq!(certificate["metadata"]["namespace"], "platform");
                let issuer = certificate["spec"]["issuerRef"]["name"]
                    .as_str()
                    .context("root issuer name")?;
                assert!(object(&platform, "Issuer", issuer)?["spec"]["selfSigned"].is_object());
                certificate["spec"]["secretName"]
                    .as_str()
                    .context("CA Secret name")
            })
            .collect::<anyhow::Result<_>>()?;
        assert!(!root_secrets.is_empty());
        let mut bundle_sources = BTreeSet::new();
        let mut host_roots = BTreeSet::new();
        let mut org_roots = BTreeSet::new();
        for bundle in platform.iter().filter(|object| object["kind"] == "Bundle") {
            let name = bundle["metadata"]["name"].as_str().context("Bundle name")?;
            let sources = bundle["spec"]["sources"]
                .as_array()
                .context("Bundle sources")?;
            let [source] = sources.as_slice() else {
                anyhow::bail!("Bundle {name} must trust exactly one CA");
            };
            assert_eq!(source["secret"]["key"], "tls.crt");
            bundle_sources.insert(source["secret"]["name"].as_str().context("Bundle source")?);
            let target = &bundle["spec"]["target"];
            assert_eq!(target["configMap"]["key"], "ca.crt");
            assert!(target["secret"].is_null());
            let selector = target["namespaceSelector"]["matchLabels"]
                .as_object()
                .context("root namespace selector")?;
            assert!(!selector.is_empty());
            let selected = |namespace: &Value| {
                selector
                    .iter()
                    .all(|(label, value)| namespace["metadata"]["labels"][label] == *value)
            };
            let env = json!({"metadata":{"labels":{"wamn.dev/org-workloads":"true","wamn.dev/identity-trust":"true"}}});
            assert!(selected(&env), "{name} must reach the env");
            assert!(selected(namespace), "{name} must reach the org");
            org_roots.insert(name);
            if selected(hosts) {
                host_roots.insert(name);
            }
        }
        assert_eq!(bundle_sources, root_secrets);
        assert_eq!(host_roots, BTreeSet::from(["identity-ca"]));
        let control = org
            .iter()
            .find(|object| object["kind"] == "Deployment")
            .context("org control Deployment")?;
        let volumes = control["spec"]["template"]["spec"]["volumes"]
            .as_array()
            .context("control volumes")?;
        let database_mount = volumes
            .iter()
            .find(|volume| volume["name"] == "database")
            .context("control database volume")?;
        let sources = database_mount["projected"]["sources"]
            .as_array()
            .context("control database sources")?;
        let database_ca = sources
            .iter()
            .find(|source| {
                source["configMap"]["name"]
                    == org_values["control"]["configuration"]["database"]["server_ca"]["name"]
            })
            .context("database CA projection from the org ConfigMap")?;
        let name = database_ca["configMap"]["name"]
            .as_str()
            .context("root ConfigMap name")?;
        assert!(org_roots.contains(name));
        for object in platform.iter().chain(&org) {
            let name = object["metadata"]["name"].as_str().unwrap_or_default();
            match object["kind"].as_str() {
                Some("Secret") => {
                    assert!(!root_secrets.contains(name));
                }
                Some("ConfigMap") => assert!(!org_roots.contains(name)),
                _ => {}
            }
        }
    }
    Ok(())
}

fn objects(output: &std::process::Output) -> anyhow::Result<Vec<serde_json::Value>> {
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_yaml::Deserializer::from_slice(&output.stdout)
        .map(serde::Deserialize::deserialize)
        .collect::<Result<_, _>>()
        .map_err(Into::into)
}

fn object<'a>(
    objects: &'a [serde_json::Value],
    kind: &str,
    name: &str,
) -> anyhow::Result<&'a serde_json::Value> {
    use anyhow::Context as _;
    objects
        .iter()
        .find(|object| object["kind"] == kind && object["metadata"]["name"] == name)
        .with_context(|| format!("find {kind} {name}"))
}

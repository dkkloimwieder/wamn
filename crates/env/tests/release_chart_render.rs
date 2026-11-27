//! Renders the app, org and project charts to check their release objects.
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
    let root = repository_root();
    let output = helm(&[
        "template",
        "acme",
        root.join("deploy/charts/org").to_str().unwrap(),
        "-f",
        root.join("crates/env/tests/support/org-chart-values.json")
            .to_str()
            .unwrap(),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let objects: Vec<serde_yaml::Value> = serde_yaml::Deserializer::from_slice(&output.stdout)
        .map(serde::Deserialize::deserialize)
        .collect::<Result<_, _>>()?;
    let route = objects
        .iter()
        .find_map(|object| object["data"]["route.json"].as_str())
        .expect("control route");
    let route: serde_json::Value = serde_json::from_str(route)?;
    let service = objects
        .iter()
        .find(|object| object["kind"] == "Service")
        .expect("control Service");
    let name = service["metadata"]["name"].as_str().expect("Service name");
    let namespace = service["metadata"]["namespace"]
        .as_str()
        .expect("Service namespace");
    assert_eq!(
        route["api"],
        format!("http://{name}.{namespace}.svc.cluster.local")
    );

    let port = &service["spec"]["ports"][0];
    assert_eq!(port["port"].as_u64(), Some(80));
    assert_eq!(port["targetPort"].as_str(), Some("http"));
    let pod = &objects
        .iter()
        .find(|object| object["kind"] == "Deployment")
        .expect("control Deployment")["spec"]["template"];
    let port = pod["spec"]["containers"][0]["ports"]
        .as_sequence()
        .expect("control pod ports")
        .iter()
        .find(|container_port| container_port["name"] == port["targetPort"])
        .expect("Service target port");
    assert_eq!(port["containerPort"].as_u64(), Some(8080));
    Ok(())
}

#[test]
fn org_and_project_render_restricted_pods_with_resources() -> anyhow::Result<()> {
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
    let instance = "abcdefgh".parse()?;
    let connection = connection_for(&triple, &instance);
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
    let project = helm::template(&entry(&charts.join("project"), "acme", "fixture", project)?)?;
    pod_resources::assert_pod_resources(&project);
    let store = project
        .iter()
        .find(|object| object["kind"] == "ObjectStore")
        .expect("a backup-enabled project must render an ObjectStore");
    pod_resources::assert_compute_resources(
        &store["spec"]["instanceSidecarConfiguration"]["resources"],
        "backup sidecar",
    );
    pod_security::assert_restricted_pods(
        project
            .iter()
            .filter(|object| object["kind"] == "Deployment"),
    );
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
            "namespace": "acme",
            "platform": {
                "roles": {"ingress": {"image": format!("registry.example/ingress@{digest}")}},
                "workflow": {"image": format!("registry.example/workflow@{digest}")},
            },
            "release": {"manifestDigest": digest},
            "env": "acme/shop/dev",
            "routeHost": "app.example.com",
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
        let database = object(&platform, "ConfigMap", "wamn-system-sql")?["data"]["database.json"]
            .as_str()
            .context("system SQL configuration")?;
        let database = serde_json::from_str(database)?;
        let inputs = &object(&platform, "ConfigMap", "wamn-org-inputs")?["data"];
        let org = "acme".parse()?;
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
        for (host, client, mode) in [
            (&settings["database"]["host"], "control", "transaction"),
            (&settings["apply_database"]["host"], "worker", "session"),
            (&settings["apply_transaction_host"], "worker", "transaction"),
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
            let egress = object(
                &org,
                "NetworkPolicy",
                &format!("allow-{client}-to-postgres"),
            )?;
            assert!(
                egress["spec"]["egress"]
                    .as_array()
                    .context("SQL egress")?
                    .iter()
                    .any(|rule| {
                        rule["to"][0]["podSelector"]["matchLabels"]["cnpg.io/poolerName"] == name
                    })
            );
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

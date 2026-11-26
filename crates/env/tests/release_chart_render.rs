//! Renders the app and org charts with `helm template` to check their release objects.
//!
//! These tests need only the local Helm executable.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

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

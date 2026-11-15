//! Renders `deploy/platform/release` with `helm template` and tests what an
//! environment's Helm release holds
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! These tests need only the local Helm executable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize as _;
use serde_yaml::{Mapping, Value};
use wamn_release_control::release_chart::{
    ImageSet, Role, RoleName, ValuesInput, release_name, stamp, values,
};

const DIGEST: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const WEB_DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NAMESPACE: &str = "wamn-system";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
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

fn image_set_fixture() -> ImageSet {
    ImageSet {
        host: format!("localhost:5000/wamn-host:dev@sha256:{}", "1".repeat(64)),
        http: format!("localhost:5000/wamn/flow-http@sha256:{}", "2".repeat(64)),
    }
}

/// Stamp a copy of the chart with its committed subchart archive.
fn stamped_chart(scratch: &Scratch) -> PathBuf {
    let chart = stamp(
        &repository_root().join("deploy/platform/release"),
        &image_set_fixture(),
        &scratch.0.join("chart"),
    )
    .expect("stamp the chart");
    let path = chart.join("values.yaml");
    let mut defaults: Value = serde_yaml::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    defaults["platformUi"] = WEB_DIGEST.into();
    std::fs::write(path, serde_yaml::to_string(&defaults).unwrap()).unwrap();
    chart
}

fn input(env: &str) -> ValuesInput {
    let mut config = Mapping::new();
    config.insert("wamn.project_env_key".into(), "acme".into());
    ValuesInput {
        org: "acme".into(),
        project: "wms".into(),
        env: env.into(),
        manifest_digest: DIGEST.into(),
        artifact_base: "registry.example/releases".into(),
        route_host: "wms.acme.example".into(),
        applications: [(
            "wms".to_owned(),
            wamn_catalog::ApplicationDefinition {
                connections: std::collections::BTreeMap::new(),
                packages: ["wamn_wms".to_owned()].into(),
            },
        )]
        .into(),
        roles: vec![Role {
            name: RoleName::Http,
            entry: Some(format!("entries@sha256:{}", "a".repeat(64))),
            config,
        }],
        actor: "render".into(),
        component_base: "registry.example/components".into(),
    }
}

fn render_values(env: &str) -> Value {
    let input = input(env);
    let mut rendered = values(&input).expect("values");
    rendered["webClients"] = serde_yaml::to_value(std::collections::BTreeMap::from([(
        input.route_host.clone(),
        "wamn_wms",
    )]))
    .unwrap();
    rendered["webDigests"] = serde_yaml::to_value(std::collections::BTreeMap::from([(
        input.route_host,
        WEB_DIGEST,
    )]))
    .unwrap();
    rendered
}

fn render_yaml(chart: &Path, scratch: &Scratch, env: &str, values: &Value) -> String {
    let application = values["applications"]
        .as_mapping()
        .and_then(|applications| applications.keys().next())
        .and_then(Value::as_str)
        .unwrap_or("wms");
    let name = release_name("acme", "wms", env, application).unwrap();
    let path = scratch.0.join(format!("values-{env}.yaml"));
    std::fs::write(&path, serde_yaml::to_string(values).expect("yaml")).expect("write values");
    let output = helm(&[
        "template",
        &name,
        chart.to_str().expect("utf-8 path"),
        "--namespace",
        NAMESPACE,
        "--skip-crds",
        "-f",
        path.to_str().expect("utf-8 path"),
    ]);
    assert!(
        output.status.success(),
        "helm template: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("rendered UTF-8")
}

fn kind_name(document: &Value) -> (String, String) {
    (
        document["kind"].as_str().expect("kind").to_owned(),
        document["metadata"]["name"]
            .as_str()
            .expect("name")
            .to_owned(),
    )
}

#[test]
fn applications_in_one_environment_have_independent_release_resources() {
    let scratch = Scratch::new("applications");
    let chart = stamped_chart(&scratch);
    let mut seen = BTreeSet::new();
    for application in ["orders", "receiving"] {
        let mut input = input("prod");
        input.applications = BTreeMap::from([(
            application.to_owned(),
            wamn_catalog::ApplicationDefinition {
                packages: [application.to_owned()].into(),
                connections: BTreeMap::new(),
            },
        )]);
        input.route_host = format!("{application}.example");
        let entry = format!(
            "entries@{}",
            wamn_digest::bytes_sha256(application.as_bytes())
        );
        input.roles[0].entry = Some(entry.clone());
        let values = values(&input).unwrap();
        let name = release_name(&input.org, &input.project, &input.env, application).unwrap();
        let yaml = render_yaml(&chart, &scratch, &input.env, &values);
        let documents = serde_yaml::Deserializer::from_str(&yaml)
            .map(Value::deserialize)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for object in documents.iter().filter(|object| !object.is_null()) {
            assert!(
                seen.insert(kind_name(object)),
                "shared object: {:?}",
                kind_name(object)
            );
            match object["kind"].as_str().unwrap() {
                "WorkloadDeployment" => {
                    let spec = &object["spec"]["template"]["spec"];
                    assert_eq!(
                        spec["hostSelector"]["hostgroup"],
                        wamn_registry::name::environment_host_name(&input.org, &input.env).unwrap()
                    );
                    assert_eq!(
                        spec["components"][0]["image"],
                        format!("registry.example/components/{entry}")
                    );
                    let route = spec["hostInterfaces"]
                        .as_sequence()
                        .unwrap()
                        .iter()
                        .find(|interface| {
                            interface["namespace"] == "wasi" && interface["package"] == "http"
                        })
                        .unwrap();
                    assert_eq!(route["config"]["host"], input.route_host);
                    let service = &spec["kubernetes"]["service"]["name"];
                    assert!(documents.iter().any(|object| object["kind"] == "Service"
                        && &object["metadata"]["name"] == service));
                }
                "Service" if object["spec"]["selector"].is_mapping() => {
                    assert_eq!(
                        object["spec"]["selector"]["wasmcloud.com/hostgroup"],
                        wamn_registry::name::environment_host_name(&input.org, &input.env).unwrap()
                    );
                }
                "ConfigMap" if object["metadata"]["name"] == name => {
                    let carrier: serde_json::Value =
                        serde_json::from_str(object["data"]["release.json"].as_str().unwrap())
                            .unwrap();
                    assert_eq!(carrier["host"], input.route_host);
                    assert_eq!(carrier["digest"], input.manifest_digest);
                }
                _ => {}
            }
        }
    }
}

#[test]
fn the_platform_ui_digest_and_org_reach_the_release_row() {
    let scratch = Scratch::new("platform-ui-digest");
    let chart = stamped_chart(&scratch);
    let mut values = render_values("prod");
    values["applications"] = Value::Mapping(Mapping::new());
    values["platformUi"] = WEB_DIGEST.into();
    values["org"] = "acme".into();
    let yaml = render_yaml(&chart, &scratch, "prod", &values);
    let row = serde_yaml::Deserializer::from_str(&yaml)
        .map(Value::deserialize)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .find(|object| object["data"]["release.json"].is_string())
        .unwrap();
    let row: serde_json::Value =
        serde_json::from_str(row["data"]["release.json"].as_str().unwrap()).unwrap();
    assert_eq!(row["platformUi"], WEB_DIGEST);
    assert_eq!(row["org"], values["org"].as_str().unwrap());
}

#[test]
fn an_unstamped_chart_does_not_render() {
    let scratch = Scratch::new("unstamped");
    let chart = scratch.0.join("chart");
    let status = Command::new("cp")
        .args(["-r"])
        .arg(repository_root().join("deploy/platform/release"))
        .arg(&chart)
        .status()
        .expect("copy the chart");
    assert!(status.success());
    let output = helm(&["template", "x", chart.to_str().expect("utf-8 path")]);
    assert!(!output.status.success());
}

#[test]
fn two_environments_in_one_namespace_share_no_object_name() {
    let scratch = Scratch::new("names");
    let chart = stamped_chart(&scratch);
    let long = "a".repeat(30);
    let longer = format!("{}b", "a".repeat(29));
    let envs = ["prod", "prod2", long.as_str(), longer.as_str()];
    let mut seen = BTreeSet::new();
    for env in envs {
        let yaml = render_yaml(&chart, &scratch, env, &render_values(env));
        for document in serde_yaml::Deserializer::from_str(&yaml) {
            let document = Value::deserialize(document).unwrap();
            if !document.is_null() {
                assert!(
                    seen.insert(kind_name(&document)),
                    "shared object: {:?}",
                    kind_name(&document)
                );
            }
        }
    }
}

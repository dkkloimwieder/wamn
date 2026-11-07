//! Renders `deploy/platform/release` with `helm template` and checks what an
//! environment's Helm release holds
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! The subchart is fetched from OCI by `helm dependency build`, so the test is
//! ignored and needs `helm` and the network.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize as _;
use serde_yaml::{Mapping, Value};
use wamn_engine::release_manifest::release_label;
use wamn_release_control::release_chart::{
    ImageSet, QualificationImages, Role, RoleName, ValuesInput, image_set, qualification_images,
    release_name, stamp, values,
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
        materializer: format!("localhost:5000/wamn/materializer@sha256:{}", "3".repeat(64)),
    }
}

fn qualification_images_fixture() -> QualificationImages {
    QualificationImages {
        runner: format!("localhost:5000/wamn-runner:dev@sha256:{}", "4".repeat(64)),
        gates: format!("localhost:5000/wamn-gates:dev@sha256:{}", "5".repeat(64)),
        identity: format!("localhost:5000/wamn-identity:dev@sha256:{}", "6".repeat(64)),
    }
}

/// Stamp a copy of the chart with its committed subchart archive.
fn stamped_chart(scratch: &Scratch) -> PathBuf {
    stamp(
        &repository_root().join("deploy/platform/release"),
        &image_set_fixture(),
        Some(&qualification_images_fixture()),
        &scratch.0.join("chart"),
    )
    .expect("stamp the chart")
}

fn input(env: &str) -> ValuesInput {
    let mut group = Mapping::new();
    group.insert("replicas".into(), 3.into());
    let http: Value = serde_yaml::from_str("{enabled: true, port: 80}").expect("yaml");
    group.insert("http".into(), http);
    let base: Value = serde_yaml::from_str(
        "[{name: WAMN_COMPONENT_ARTIFACT_BASE, value: registry.example/components}]",
    )
    .expect("yaml");
    group.insert("env".into(), base);
    let mut config = Mapping::new();
    config.insert("wamn.env_key".into(), "acme".into());
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
            statements: vec!["sql-0".to_owned(), "sql-1".to_owned()],
            config,
        }],
        drain_bound_seconds: 300,
        actor: "render".into(),
        host_group: group,
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
    let name = release_name("acme", "wms", env).expect("valid coordinate");
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

/// `helm template` one environment; the documents it renders.
fn render(chart: &Path, scratch: &Scratch, env: &str) -> Vec<Value> {
    let yaml = render_yaml(chart, scratch, env, &render_values(env));
    let documents: Vec<Value> = serde_yaml::Deserializer::from_str(&yaml)
        .map(Value::deserialize)
        .collect::<Result<Vec<_>, _>>()
        .expect("rendered YAML")
        .into_iter()
        .filter(|document| !document.is_null())
        .collect();
    let hosts: Vec<_> = documents
        .iter()
        .filter(|document| document["kind"] == "Deployment")
        .collect();
    assert!(!hosts.is_empty(), "the release renders a host Deployment");
    for deployment in hosts {
        let name = deployment["metadata"]["name"].as_str().expect("host name");
        let pod = &deployment["spec"]["template"]["spec"];
        let containers = pod["containers"].as_sequence().expect("host containers");
        let host = containers
            .iter()
            .find(|container| container["name"] == "host")
            .expect("host container");
        let variables = host["env"].as_sequence().expect("host variables");
        let seconds = |key| {
            let entries: Vec<_> = variables
                .iter()
                .filter(|variable| variable["name"] == key)
                .collect();
            assert_eq!(entries.len(), 1, "{name}: exactly one {key}");
            entries[0]["value"]
                .as_str()
                .expect("seconds string")
                .parse::<u64>()
                .expect("seconds integer")
        };
        let drain = seconds("WAMN_DRAIN_BOUND_SECONDS");
        let grace = seconds("WAMN_MATERIALIZER_DRAIN_BOUND_SECONDS");
        assert!(
            grace < drain,
            "{name}: workload grace {grace} must be shorter than drain bound {drain}"
        );
        assert_eq!(
            pod["terminationGracePeriodSeconds"].as_u64(),
            Some(drain),
            "{name}: pod grace equals the drain bound"
        );
    }
    documents
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
#[ignore = "requires: bin:helm"]
fn web_only_changes_leave_all_workloads_byte_equal() {
    wamn_test_postgres::require_ignored_prerequisites("requires: bin:helm");
    let scratch = Scratch::new("web-only");
    let chart = stamped_chart(&scratch);
    let name = release_name("acme", "wms", "prod").unwrap();
    let first = render_values("prod");
    let before = render_yaml(&chart, &scratch, "prod", &first);
    let documents = |yaml: &str| -> BTreeMap<_, _> {
        yaml.split("---\n")
            .filter(|document| !document.trim().is_empty())
            .map(|bytes| {
                let object: Value = serde_yaml::from_str(bytes).expect("rendered YAML");
                (kind_name(&object), bytes.to_owned())
            })
            .collect()
    };
    let before = documents(&before);
    assert!(before.keys().any(|(kind, _)| kind == "Deployment"));
    assert!(before.keys().any(|(kind, _)| kind == "WorkloadDeployment"));
    // Each edit changes one web map only. All other objects, including every
    // Deployment, StatefulSet and workload pod template, keep their exact bytes.
    for key in ["webDigests", "webClients"] {
        let mut second = first.clone();
        second[key]["wms.acme.example"] = if key == "webDigests" {
            format!("sha256:{}", "b".repeat(64)).into()
        } else {
            "wamn_receiving".into()
        };
        let after = documents(&render_yaml(&chart, &scratch, "prod", &second));
        assert_eq!(
            before.keys().collect::<Vec<_>>(),
            after.keys().collect::<Vec<_>>()
        );
        let mut changed = Vec::new();
        for (object, bytes) in &before {
            if object == &("ConfigMap".into(), name.clone()) {
                let old: Value = serde_yaml::from_str(bytes).unwrap();
                let mut new: Value = serde_yaml::from_str(&after[object]).unwrap();
                let old_release: serde_json::Value =
                    serde_json::from_str(old["data"]["release.json"].as_str().unwrap()).unwrap();
                let new_release: serde_json::Value =
                    serde_json::from_str(new["data"]["release.json"].as_str().unwrap()).unwrap();
                let field = if key == "webDigests" {
                    "webs"
                } else {
                    "clients"
                };
                assert_ne!(old_release[field], new_release[field]);
                let mut expected = old_release;
                expected[field] = serde_json::to_value(&second[key]).unwrap();
                assert_eq!(new_release, expected);
                new["data"]["release.json"] = old["data"]["release.json"].clone();
                assert_eq!(old, new);
            } else {
                assert_eq!(bytes, &after[object], "{key} changed {object:?}");
            }
            if bytes != &after[object] {
                changed.push(object.clone());
            }
        }
        assert_eq!(changed, [("ConfigMap".into(), name.clone())]);
    }
}

#[tokio::test]
#[ignore = "requires: bin:helm"]
async fn the_release_chart_renders_one_host_group_and_its_role_workloads() {
    wamn_test_postgres::require_ignored_prerequisites("requires: bin:helm");
    let scratch = Scratch::new("objects");
    let chart = stamped_chart(&scratch);
    let name = release_name("acme", "wms", "prod").expect("valid coordinate");
    let label = release_label(DIGEST).expect("digest");
    let documents = render(&chart, &scratch, "prod");
    for object in &documents {
        assert!(!matches!(
            object["kind"].as_str(),
            Some("Database" | "Publication" | "Secret" | "ServiceAccount" | "Role" | "RoleBinding")
        ));
        if object["kind"] == "WorkloadDeployment" {
            assert!(object["spec"]["template"]["spec"].get("service").is_none());
        }
    }
    // The application host uses the existing environment event stream,
    // and every grant stays inside the environment prefix.
    let host_config = documents
        .iter()
        .find(|object| {
            object["kind"] == "ConfigMap"
                && object["metadata"]["name"].as_str()
                    == Some(&format!("hostgroup-{name}-host-config"))
        })
        .expect("the host configuration file");
    let file: Value = serde_yaml::from_str(
        host_config["data"]["config.yaml"]
            .as_str()
            .expect("config.yaml"),
    )
    .expect("the host configuration file is YAML");
    let plugins = file["host"]["plugins"].as_sequence().expect("plugins");
    assert_eq!(plugins.len(), 1);
    let nats = &plugins[0];
    assert_eq!(nats["id"].as_str(), Some("wasmcloud-nats"));
    assert_eq!(nats["workloadConfig"].as_str(), Some("deny"));
    let events = &nats["bindings"]["events"]["config"];
    assert!(
        events.get("servers").is_none(),
        "the binding inherits the data connection"
    );
    let prefix = wamn_control_registry::project_env_key("acme", "wms", "prod");
    assert_eq!(
        events["subject-allow"].as_str(),
        Some("evt.acme.wms.prod.>")
    );
    assert!(
        events["stream-allow"]
            .as_str()
            .is_some_and(|stream| stream.starts_with("EVT_") && stream.ends_with("_4_prod"))
    );
    assert_eq!(
        events["inbox-prefix"].as_str(),
        Some(format!("_INBOX_wamn-env-{prefix}").as_str())
    );
    let row = documents
        .iter()
        .find(|object| object["kind"] == "ConfigMap" && object["metadata"]["name"] == name)
        .expect("the release row");
    let row: serde_json::Value =
        serde_json::from_str(row["data"]["release.json"].as_str().unwrap()).unwrap();
    assert_eq!(row["host"], "wms.acme.example");
    assert_eq!(row["digest"], DIGEST);
    assert_eq!(row["clients"]["wms.acme.example"], "wamn_wms");
    assert_eq!(row["webs"]["wms.acme.example"], WEB_DIGEST);
    assert_eq!(row["platformVersion"], "");
    for document in &documents {
        let annotations = &document["metadata"]["annotations"];
        assert!(
            annotations
                .as_mapping()
                .is_none_or(|map| !map.keys().any(|key| key
                    .as_str()
                    .is_some_and(|key| key.starts_with("helm.sh/hook")))),
            "{:?} carries a Helm hook",
            kind_name(document)
        );
    }

    let deployment = documents
        .iter()
        .find(|document| document["kind"] == "Deployment")
        .expect("the host Deployment");
    let pod = &deployment["spec"]["template"];
    // R1 (2) and §9.2: the release is the digest on the pod, never an integer.
    assert_eq!(
        pod["metadata"]["labels"]["wamn.release"].as_str(),
        Some(label.as_str())
    );
    assert_eq!(
        pod["metadata"]["labels"]["wasmcloud.com/hostgroup"].as_str(),
        Some(name.as_str())
    );
    assert_eq!(
        pod["metadata"]["annotations"]["wamn.release-digest"].as_str(),
        Some(DIGEST)
    );
    assert_eq!(
        pod["metadata"]["annotations"]["wamn.environment"].as_str(),
        Some("acme/wms/prod")
    );
    // Provisioning owns the environment account and its Secret grant.
    assert_eq!(
        pod["spec"]["serviceAccountName"].as_str(),
        Some("wamn-host-acme--wms--prod")
    );
    assert_eq!(
        pod["spec"]["automountServiceAccountToken"],
        Value::Bool(true)
    );
    // R20: the policy's drain bound is the pod's grace period.
    assert_eq!(
        pod["spec"]["terminationGracePeriodSeconds"].as_u64(),
        Some(300)
    );
    let containers = pod["spec"]["containers"].as_sequence().expect("containers");
    assert_eq!(containers.len(), 1);
    let host = &containers[0];
    // R6 (2): no container lifecycle hook on the host pod.
    assert!(host.get("lifecycle").is_none());
    // observability.md R6: the host exports to the cluster's collector, each
    // variable once.
    let variables = host["env"].as_sequence().expect("host variables");
    let materializer_bounds: Vec<&Value> = variables
        .iter()
        .filter(|variable| variable["name"] == "WAMN_MATERIALIZER_DRAIN_BOUND_SECONDS")
        .collect();
    assert_eq!(materializer_bounds.len(), 1);
    assert_eq!(materializer_bounds[0]["value"].as_str(), Some("10"));
    // Owner ruling 2026-10-21: count guest growth, keep the chart's 64Mi request.
    assert_eq!(
        host["resources"]["requests"]["memory"].as_str(),
        Some("64Mi")
    );
    assert_eq!(
        host["resources"]["limits"]["memory"].as_str(),
        Some("768Mi")
    );
    let guest_modes: Vec<&Value> = variables
        .iter()
        .filter(|variable| variable["name"] == "WASH_GUEST_MEMORY_MODE")
        .collect();
    assert_eq!(guest_modes.len(), 1);
    assert_eq!(guest_modes[0]["value"].as_str(), Some("count"));
    assert!(
        variables
            .iter()
            .all(|variable| variable["name"] != "WASH_HOST_MAX_GUEST_MEMORY")
    );
    for (name, value) in wamn_release_control::release_chart::TELEMETRY_VARIABLES {
        let set: Vec<&Value> = variables
            .iter()
            .filter(|variable| variable["name"] == name)
            .collect();
        assert_eq!(set.len(), 1, "{name} is set once");
        assert_eq!(set[0]["value"].as_str(), Some(value));
    }
    let arguments: Vec<&str> = host["args"]
        .as_sequence()
        .expect("host arguments")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(arguments.contains(&format!("--host-group={name}").as_str()));
    assert!(arguments.contains(&format!("--release-manifest-digest={DIGEST}").as_str()));
    // R2: the scheduler and data connections name the one NATS.
    for flag in ["--scheduler-nats-url", "--data-nats-url"] {
        assert!(arguments.contains(
            &format!("{flag}=nats://wamn-nats.platform.svc.cluster.local:4222").as_str()
        ));
    }

    // R5: the scheduler connection keeps the runtime certificate, and the
    // data connection is the environment's NATS user.
    let secrets: BTreeSet<&str> = pod["spec"]["volumes"]
        .as_sequence()
        .expect("volumes")
        .iter()
        .filter_map(|volume| volume["secret"]["secretName"].as_str())
        .collect();
    assert!(secrets.contains("wasmcloud-runtime-tls"));
    assert!(
        secrets
            .iter()
            .any(|secret| secret.starts_with("wamn-nats-acme--wms--"))
    );
    assert!(!secrets.contains("wasmcloud-data-tls"));

    // Platform services use the chart set; HTTP uses the release's entry.
    let set = image_set(&chart).await.expect("the stamped image set");
    assert_eq!(set, image_set_fixture());
    // R15 (4): the defaults also pin the runner, gates and identity images.
    assert_eq!(
        qualification_images(&chart)
            .await
            .expect("the stamped qualification images"),
        qualification_images_fixture()
    );
    assert_eq!(host["image"].as_str(), Some(set.host.as_str()));
    for document in documents
        .iter()
        .filter(|document| document["kind"] == "WorkloadDeployment")
    {
        let spec = &document["spec"]["template"]["spec"];
        assert_eq!(
            spec["hostSelector"],
            serde_yaml::from_str::<Value>(&format!("{{hostgroup: {name}, wamn.release: {label}}}"))
                .expect("yaml")
        );
        assert_eq!(spec["environment"].as_str(), Some(NAMESPACE));
        let image = spec["components"][0]["image"]
            .as_str()
            .or_else(|| spec["service"]["image"].as_str())
            .expect("a role image");
        if spec["components"].is_sequence() {
            // The http image is the component base joined with the entry path.
            assert_eq!(
                image,
                format!(
                    "registry.example/components/entries@sha256:{}",
                    "a".repeat(64)
                )
            );
            assert_eq!(spec["components"][0]["poolSize"].as_i64(), Some(0));
        }
        assert!(
            spec.get("service").is_none(),
            "application releases own no shared service workload"
        );
    }
}

#[test]
#[ignore = "requires: bin:helm"]
fn an_unstamped_chart_does_not_render() {
    wamn_test_postgres::require_ignored_prerequisites("requires: bin:helm");
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
#[ignore = "requires: bin:helm"]
fn two_environments_in_one_namespace_share_no_object_name() {
    wamn_test_postgres::require_ignored_prerequisites("requires: bin:helm");
    let scratch = Scratch::new("names");
    let chart = stamped_chart(&scratch);
    // Two envs, and two whose first 24 characters are equal (one slug).
    let long = "a".repeat(30);
    let longer = format!("{}b", "a".repeat(29));
    let envs = ["prod", "prod2", long.as_str(), longer.as_str()];
    let mut seen = BTreeSet::new();
    for env in envs {
        for document in render(&chart, &scratch, env) {
            assert!(
                seen.insert(kind_name(&document)),
                "{:?} is rendered by two environments",
                kind_name(&document)
            );
        }
    }
}

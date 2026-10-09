//! Renders `deploy/platform/release` with `helm template` and checks what an
//! environment's Helm release holds (docs/plan/platform-deploy.md §9.2, R6, R15;
//! the render halves of assumptions A2 and A12).
//!
//! The subchart is fetched from OCI by `helm dependency build`, so the test is
//! ignored and needs `helm` and the network.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize as _;
use serde_yaml::{Mapping, Value};
use wamn_control::release_chart::{
    ImageSet, Role, RoleName, ValuesInput, image_set, release_name, stamp, values,
};
use wamn_engine::release_manifest::release_label;

const DIGEST: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
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

/// Stamp a copy of the chart and build its subchart archive there.
fn stamped_chart(scratch: &Scratch) -> PathBuf {
    let chart = stamp(
        &repository_root().join("deploy/platform/release"),
        &image_set_fixture(),
        &scratch.0.join("chart"),
    )
    .expect("stamp the chart");
    let built = helm(&["dependency", "build", chart.to_str().expect("utf-8 path")]);
    assert!(
        built.status.success(),
        "helm dependency build: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    chart
}

fn input(env: &str) -> ValuesInput {
    let mut group = Mapping::new();
    group.insert("replicas".into(), 3.into());
    let http: Value = serde_yaml::from_str("{enabled: true, port: 80}").expect("yaml");
    group.insert("http".into(), http);
    let mut config = Mapping::new();
    config.insert("wamn.tenant".into(), "acme".into());
    let mut environment = Mapping::new();
    environment.insert("WAMN_MAT_ORG".into(), "acme".into());
    ValuesInput {
        org: "acme".into(),
        project: "wms".into(),
        env: env.into(),
        manifest_digest: DIGEST.into(),
        artifact_base: "registry.example/releases".into(),
        route_host: "wms.acme.example".into(),
        roles: vec![
            Role {
                name: RoleName::Http,
                config: config.clone(),
                environment: None,
            },
            Role {
                name: RoleName::Materializer,
                config,
                environment: Some(environment),
            },
        ],
        drain_bound_seconds: 300,
        actor: "render".into(),
        host_group: group,
    }
}

/// `helm template` one environment; the documents it renders.
fn render(chart: &Path, scratch: &Scratch, env: &str) -> Vec<Value> {
    let input = input(env);
    let name = release_name(&input.org, &input.project, &input.env).expect("valid coordinate");
    let path = scratch.0.join(format!("values-{env}.yaml"));
    std::fs::write(
        &path,
        serde_yaml::to_string(&values(&input).expect("values")).expect("yaml"),
    )
    .expect("write values");
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
    serde_yaml::Deserializer::from_slice(&output.stdout)
        .map(Value::deserialize)
        .collect::<Result<Vec<_>, _>>()
        .expect("rendered YAML")
        .into_iter()
        .filter(|document| !document.is_null())
        .collect()
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

#[tokio::test]
#[ignore = "requires: helm"]
async fn the_release_chart_renders_one_host_group_and_its_role_workloads() {
    wamn_test_postgres::require_prerequisites(&["helm"]);
    let scratch = Scratch::new("objects");
    let chart = stamped_chart(&scratch);
    let name = release_name("acme", "wms", "prod").expect("valid coordinate");
    let label = release_label(DIGEST).expect("digest");
    let documents = render(&chart, &scratch, "prod");
    let kinds: BTreeSet<(String, String)> = documents.iter().map(kind_name).collect();

    // R6 (1): one Deployment, the role WorkloadDeployments and their Service, and
    // the subchart's own ServiceAccount and NetworkPolicy (owner ruling
    // 2026-10-07). No hook, Job, Secret, CRD or other custom resource.
    let expected: BTreeSet<(String, String)> = [
        ("Deployment", format!("hostgroup-{name}")),
        ("WorkloadDeployment", format!("{name}-http")),
        ("WorkloadDeployment", format!("{name}-materializer")),
        ("Service", format!("{name}-http")),
        ("NetworkPolicy", format!("hostgroup-{name}")),
        ("ServiceAccount", format!("{name}-runtime-operator-runtime")),
    ]
    .into_iter()
    .map(|(kind, name)| (kind.to_owned(), name))
    .collect();
    assert_eq!(kinds, expected);
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
    let arguments: Vec<&str> = host["args"]
        .as_sequence()
        .expect("host arguments")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(arguments.contains(&format!("--host-group={name}").as_str()));
    assert!(arguments.contains(&format!("--release-manifest-digest={DIGEST}").as_str()));
    assert!(arguments.contains(
        &format!("--scheduler-nats-url=nats://nats.{NAMESPACE}.svc.cluster.local:4222").as_str()
    ));

    // A2, render half: the host mounts the operator release's TLS Secrets.
    let secrets: BTreeSet<&str> = pod["spec"]["volumes"]
        .as_sequence()
        .expect("volumes")
        .iter()
        .filter_map(|volume| volume["secret"]["secretName"].as_str())
        .collect();
    assert!(secrets.contains("wasmcloud-runtime-tls") && secrets.contains("wasmcloud-data-tls"));

    // R15 (1), (2): the rendered images are exactly the chart's default set.
    let set = image_set(&chart).await.expect("the stamped image set");
    assert_eq!(set, image_set_fixture());
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
        assert!(image == set.http || image == set.materializer);
    }
}

#[test]
#[ignore = "requires: helm"]
fn an_unstamped_chart_does_not_render() {
    wamn_test_postgres::require_prerequisites(&["helm"]);
    let scratch = Scratch::new("unstamped");
    let chart = scratch.0.join("chart");
    let status = Command::new("cp")
        .args(["-r"])
        .arg(repository_root().join("deploy/platform/release"))
        .arg(&chart)
        .status()
        .expect("copy the chart");
    assert!(status.success());
    let built = helm(&["dependency", "build", chart.to_str().expect("utf-8 path")]);
    assert!(built.status.success());
    let output = helm(&["template", "x", chart.to_str().expect("utf-8 path")]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("stamp the platform image set"));
}

#[test]
#[ignore = "requires: helm"]
fn two_environments_in_one_namespace_share_no_object_name() {
    wamn_test_postgres::require_prerequisites(&["helm"]);
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

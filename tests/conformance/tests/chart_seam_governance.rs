//! Render the host chart and inspect its native controls and namespace permissions.

#[path = "../../../crates/control/environment/tests/support/render_fixture.rs"]
pub mod render_fixture;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize as _;
use serde_json::Value;

const VALUES: &str = "deploy/infra/values-wamn.yaml";
const EVENTS_RBAC: &str = "deploy/platform/runtime-operator-events-rbac.example.yaml";
static RENDER_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const DEPLOYMENT: (&str, &str) = ("apps/v1", "Deployment");
const NETWORK_POLICY: (&str, &str) = ("networking.k8s.io/v1", "NetworkPolicy");
const ROLE: (&str, &str) = ("rbac.authorization.k8s.io/v1", "Role");
const ROLE_BINDING: (&str, &str) = ("rbac.authorization.k8s.io/v1", "RoleBinding");

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn read_repository_file(root: &Path, relative: &str) -> String {
    let path = root.join(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

struct RenderDirectory(PathBuf);

impl RenderDirectory {
    fn create() -> Self {
        let sequence = RENDER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "wamn-host-render-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated Helm render directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for RenderDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn render_chart(root: &Path, release: &str, values: &[&str]) -> Vec<Value> {
    let metadata: Value = serde_yaml::from_str(&read_repository_file(
        root,
        "deploy/platform/release/Chart.yaml",
    ))
    .expect("release chart metadata");
    let version = metadata["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|dependency| dependency["name"] == "runtime-operator")
        .and_then(|dependency| dependency["version"].as_str())
        .expect("the release chart selects the operator version");
    let chart = format!("deploy/platform/release/charts/runtime-operator-{version}.tgz");
    let mut helm = Command::new("helm");
    helm.current_dir(root)
        .args(["template", release, &chart, "--namespace", "platform"]);
    for value in values {
        helm.args(["--values", value]);
    }
    let rendered = helm.output().expect("run the pinned Helm renderer");
    assert!(
        rendered.status.success(),
        "Helm render failed: {}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    serde_yaml::Deserializer::from_slice(&rendered.stdout)
        .map(|document| Value::deserialize(document).expect("decode rendered Kubernetes object"))
        .filter(|object| !object.is_null())
        .collect()
}

/// Decode a manifest into the JSON that `kubectl create --dry-run=client --output=json`
/// printed: empty documents are skipped, one object stays itself, and several
/// objects become a `List`. Every object must carry one of the `expected`
/// `apiVersion` and `type` pairs, and every expected pair must occur.
fn decode_manifest(manifest: &[u8], expected: &[(&str, &str)]) -> Value {
    let mut objects = serde_yaml::Deserializer::from_slice(manifest)
        .map(|document| Value::deserialize(document).expect("Kubernetes manifest decodes as YAML"))
        .filter(|object| !object.is_null())
        .collect::<Vec<_>>();
    let is = |object: &Value, (api_version, type_): (&str, &str)| {
        object["apiVersion"] == api_version && object["kind"] == type_
    };
    for object in &objects {
        assert!(
            expected.iter().any(|pair| is(object, *pair)),
            "Kubernetes manifest carries unexpected {} {}",
            object["apiVersion"],
            object["kind"]
        );
    }
    for &(api_version, type_) in expected {
        assert!(
            objects
                .iter()
                .any(|object| is(object, (api_version, type_))),
            "Kubernetes manifest carries no {api_version} {type_}"
        );
    }
    match objects.len() {
        0 => panic!("Kubernetes manifest holds no object"),
        1 => objects.remove(0),
        _ => serde_json::json!({"apiVersion": "v1", "kind": "List", "items": objects}),
    }
}

fn rendered_object<'a>(
    objects: &'a [Value],
    pair: (&str, &str),
    name: &str,
    namespace: &str,
) -> &'a Value {
    objects
        .iter()
        .find(|object| {
            object["apiVersion"] == pair.0
                && object["kind"] == pair.1
                && object["metadata"]["name"] == name
                && object["metadata"]["namespace"] == namespace
        })
        .unwrap_or_else(|| panic!("rendered chart has no {} {} {name}", pair.0, pair.1))
}

fn render_host_objects(root: &Path) -> (Vec<Value>, String) {
    use wamn_test_infrastructure::rendering::{EventIdentity, HostValuesInput};
    let values = render_fixture::host_values(
        &HostValuesInput {
            namespace: "platform".into(),
            host_tag: "dev".into(),
            replicas: 3,
            component_artifact_base: "registry.example/components".into(),
            release_artifact_base: "registry.example/releases".into(),
            manifest_digest: format!("sha256:{}", "a".repeat(64)),
            stream_replicas: 1,
            dup_window_secs: 120,
            event: EventIdentity {
                org: "acme".into(),
                project: "wms".into(),
                environment: "dev".into(),
            },
            guest_secret_name: "wamn-guest-acme--wms--dev".into(),
            role_secrets: Vec::new(),
            object_store_secret_name: None,
        },
        "wms",
    )
    .expect("render apply host values");
    let overlay_values: Value = serde_yaml::from_str(&values.overlay).expect("decode host values");
    let name = format!(
        "hostgroup-{}",
        overlay_values["runtime"]["hostGroups"][0]["name"]
            .as_str()
            .unwrap()
    );
    let work = RenderDirectory::create();
    let base = work.path().join("base.yaml");
    let overlay = work.path().join("overlay.yaml");
    fs::write(&base, values.base).expect("write release host defaults");
    fs::write(&overlay, values.overlay).expect("write apply host values");
    let output = render_chart(
        root,
        "wamn-host",
        &[base.to_str().unwrap(), overlay.to_str().unwrap()],
    );
    (output, name)
}

fn host_container(deployment: &Value) -> &Value {
    let containers = deployment["spec"]["template"]["spec"]["containers"]
        .as_array()
        .expect("rendered host Deployment carries containers");
    containers
        .iter()
        .find(|container| container["name"] == "host")
        .expect("rendered host container")
}

fn assert_native_host_controls(
    deployment: &Value,
    network_policy: &Value,
    concurrent_starts: &str,
    cpu_request: &str,
    cpu_limit: &str,
) {
    let container = host_container(deployment);
    assert_eq!(deployment["metadata"]["namespace"], "platform");
    for (name, expected) in [
        ("WASH_DEFAULT_HEAP_MEMORY", "256MiB"),
        ("WASH_CORE_INSTANCES", "512"),
        ("WASH_MAX_CONCURRENT_STARTS", concurrent_starts),
    ] {
        let entries = container["env"]
            .as_array()
            .expect("host environment is an array")
            .iter()
            .filter(|entry| entry["name"] == name)
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1, "host must set {name} exactly once");
        assert_eq!(entries[0]["value"], expected, "wrong host {name}");
    }
    assert_eq!(
        container["resources"],
        serde_json::json!({
            "requests": {"cpu": cpu_request, "memory": "256Mi"},
            "limits": {"cpu": cpu_limit, "memory": "4Gi"},
        }),
        "native host controls must not leak into Kubernetes resources or change CPU limits"
    );
    for argument in [
        "--probe-addr=0.0.0.0:8081",
        "--drain-delay=5s",
        "--nats-connect-timeout=60s",
    ] {
        assert_eq!(
            container["args"]
                .as_array()
                .expect("host arguments are an array")
                .iter()
                .filter(|entry| *entry == argument)
                .count(),
            1,
            "host must receive {argument} exactly once"
        );
    }
    let ports = container["ports"].as_array().expect("host exposes ports");
    let probes = ports
        .iter()
        .filter(|port| port["name"] == "probes")
        .collect::<Vec<_>>();
    assert_eq!(probes.len(), 1, "host must expose one native probe port");
    assert_eq!(probes[0]["containerPort"], 8081);
    assert_eq!(probes[0]["protocol"], "TCP");
    for (name, path) in [
        ("startupProbe", "/livez"),
        ("livenessProbe", "/livez"),
        ("readinessProbe", "/readyz"),
    ] {
        assert_eq!(
            container[name]["httpGet"],
            serde_json::json!({"path": path, "port": "probes"}),
            "host {name} must use the native health listener"
        );
        assert!(container[name].get("tcpSocket").is_none());
    }
    assert_eq!(container["startupProbe"]["periodSeconds"], 5);
    assert_eq!(container["startupProbe"]["failureThreshold"], 60);

    let registry_mount = container["volumeMounts"]
        .as_array()
        .expect("host volume mounts are an array")
        .iter()
        .find(|mount| mount["name"] == "registry-pull")
        .expect("host container must mount the registry credential");
    assert_eq!(registry_mount["mountPath"], "/etc/wamn/registry");
    assert_eq!(registry_mount["readOnly"], true);

    assert_eq!(network_policy["kind"], "NetworkPolicy");
    assert_eq!(
        network_policy["metadata"]["namespace"], deployment["metadata"]["namespace"],
        "probe ingress permission must stay in the host group's namespace"
    );
    assert_eq!(
        network_policy["spec"]["podSelector"], deployment["spec"]["selector"],
        "probe ingress permission must select only the host group"
    );
    assert_eq!(
        network_policy["spec"]["policyTypes"],
        serde_json::json!(["Ingress"])
    );
    assert_eq!(
        network_policy["spec"]["ingress"],
        serde_json::json!([{"ports": [
            {"port": 80, "protocol": "TCP"},
            {"port": 8081, "protocol": "TCP"},
            {"port": 8082, "protocol": "TCP"},
        ]}]),
        "native policy must allow traffic and probe ports without changing source target"
    );
    assert!(network_policy["spec"].get("egress").is_none());
}

fn assert_events_overlay_remains_namespace_targeted(root: &Path) {
    let values = read_repository_file(root, VALUES);
    let overlay = read_repository_file(root, EVENTS_RBAC);
    let output = render_chart(root, "wamn", &[VALUES]);
    let values: serde_yaml::Value = serde_yaml::from_str(&values).expect("decode operator values");
    let namespaces = values["operator"]["watchNamespaces"]
        .as_sequence()
        .expect("operator watchNamespaces is a list");
    assert!(
        !namespaces.is_empty(),
        "operator must name its watched namespaces"
    );
    for namespace in namespaces {
        let namespace = namespace.as_str().expect("namespace is a string");
        let manifest = decode_manifest(
            overlay
                .replace("__ENVIRONMENT_NAMESPACE__", namespace)
                .as_bytes(),
            &[ROLE, ROLE_BINDING],
        );
        let objects = manifest["items"]
            .as_array()
            .expect("Events overlay contains a Role and RoleBinding");
        assert_eq!(
            objects.len(),
            2,
            "Events overlay must stay narrowly targeted"
        );
        let role = objects
            .iter()
            .find(|object| object["kind"] == "Role")
            .expect("Events overlay contains a namespaced Role");
        assert_eq!(role["metadata"]["namespace"], namespace);
        assert_eq!(
            role["rules"],
            serde_json::json!([{
                "apiGroups": ["events.k8s.io"],
                "resources": ["events"],
                "verbs": ["create", "patch"],
            }])
        );
        let binding = objects
            .iter()
            .find(|object| object["kind"] == "RoleBinding")
            .expect("Events overlay contains a namespaced RoleBinding");
        assert_eq!(binding["metadata"]["namespace"], namespace);
        assert_eq!(
            binding["roleRef"],
            serde_json::json!({
                "apiGroup": "rbac.authorization.k8s.io",
                "kind": "Role",
                "name": role["metadata"]["name"],
            })
        );
        let chart_binding = rendered_object(
            &output,
            ROLE_BINDING,
            "wamn-runtime-operator-workload-namespace",
            namespace,
        );
        assert_eq!(
            binding["subjects"], chart_binding["subjects"],
            "Events overlay must bind the existing operator identity"
        );
    }
}

#[test]
#[ignore = "requires: bin:helm"]
fn the_host_key_reader_accepts_the_rendered_secret_permissions() {
    use std::os::unix::fs::PermissionsExt as _;

    wamn_test_postgres::require_ignored_prerequisites("requires: bin:helm");
    let (objects, name) = render_host_objects(&repository_root());
    let deployment = rendered_object(&objects, DEPLOYMENT, &name, "platform");
    let pod = &deployment["spec"]["template"]["spec"];
    let container = host_container(deployment);
    let key = container["env"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "WAMN_OAUTH_KEY_FILE")
        .expect("the local host declares its OAuth key file")["value"]
        .as_str()
        .unwrap();
    let (mount, relative) = container["volumeMounts"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|mount| {
            Path::new(key)
                .strip_prefix(mount["mountPath"].as_str().unwrap())
                .ok()
                .map(|relative| (mount, relative))
        })
        .expect("the host key file is mounted");
    let secret = &pod["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|volume| volume["name"] == mount["name"])
        .expect("the key mount names a volume")["secret"];
    assert!(secret.is_object(), "the host key comes from a Secret");
    let item_mode = secret["items"].as_array().map(|items| {
        &items
            .iter()
            .find(|item| item["path"].as_str() == relative.to_str())
            .expect("the Secret projects the host key file")["mode"]
    });
    // Secret volumes default to 0644. For fsGroup the kubelet ORs 0440
    // into read-only volume files, including files with an explicit mode.
    let mut mode = item_mode
        .and_then(Value::as_u64)
        .or_else(|| secret["defaultMode"].as_u64())
        .unwrap_or(0o644);
    if pod["securityContext"]["fsGroup"].as_u64().is_some() {
        mode |= 0o440;
    }
    let work = RenderDirectory::create();
    let path = work.path().join("key");
    fs::write(&path, [7; 32]).expect("write representative Secret key bytes");
    fs::set_permissions(
        &path,
        fs::Permissions::from_mode(u32::try_from(mode).unwrap()),
    )
    .expect("apply the kubelet's file mode");
    wamn_runtime::oauth_encryption::Envelope::from_file(&path)
        .unwrap_or_else(|error| panic!("rendered key {key} with mode {mode:o}: {error}"));
}

#[test]
#[ignore = "requires: bin:helm"]
fn the_host_tier_renders_the_native_host_controls() {
    wamn_test_postgres::require_ignored_prerequisites("requires: bin:helm");
    let root = repository_root();
    let (objects, name) = render_host_objects(&root);
    let base = rendered_object(&objects, DEPLOYMENT, &name, "platform");
    let base_policy = rendered_object(&objects, NETWORK_POLICY, &name, "platform");
    assert_native_host_controls(base, base_policy, "4", "2", "6");
    assert_events_overlay_remains_namespace_targeted(&root);
}

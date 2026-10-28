//! Render the host chart and inspect its native controls and namespace permissions.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize as _;
use serde_json::Value;

const VALUES: &str = "deploy/infra/values-wamn.yaml";
const HOST_VALUES: &str = "deploy/platform/values-host-default.yaml";
const EVENTS_RBAC: &str = "deploy/platform/runtime-operator-events-rbac.example.yaml";
const EXPECTED_CHART_VERSION: &str = "2.11.0";
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

fn render_chart(root: &Path, release: &str, values: &[&str]) -> RenderDirectory {
    let output = RenderDirectory::create();
    let mut helm = Command::new("helm");
    helm.current_dir(root).args([
        "template",
        release,
        "oci://ghcr.io/wasmcloud/charts/runtime-operator",
        "--version",
        EXPECTED_CHART_VERSION,
        "--namespace",
        "wamn-system",
    ]);
    for value in values {
        helm.args(["--values", value]);
    }
    helm.arg("--output-dir").arg(output.path());
    let rendered = helm.output().expect("run the pinned Helm renderer");
    assert!(
        rendered.status.success(),
        "Helm render failed: {}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    assert!(
        !output
            .path()
            .join("runtime-operator/templates/gateway")
            .exists(),
        "release {release} rendered the disabled gateway"
    );
    output
}

/// Decode a manifest into the JSON that `kubectl create --dry-run=client --output=json`
/// printed: empty documents are skipped, one object stays itself, and several
/// objects become a `List`. Every object must carry one of the `expected`
/// `apiVersion` and `kind` pairs, and every expected pair must occur.
fn decode_manifest(manifest: &[u8], expected: &[(&str, &str)]) -> Value {
    let mut objects = serde_yaml::Deserializer::from_slice(manifest)
        .map(|document| Value::deserialize(document).expect("Kubernetes manifest decodes as YAML"))
        .filter(|object| !object.is_null())
        .collect::<Vec<_>>();
    let is = |object: &Value, (api_version, kind): (&str, &str)| {
        object["apiVersion"] == api_version && object["kind"] == kind
    };
    for object in &objects {
        assert!(
            expected.iter().any(|pair| is(object, *pair)),
            "Kubernetes manifest carries unexpected {} {}",
            object["apiVersion"],
            object["kind"]
        );
    }
    for &(api_version, kind) in expected {
        assert!(
            objects.iter().any(|object| is(object, (api_version, kind))),
            "Kubernetes manifest carries no {api_version} {kind}"
        );
    }
    match objects.len() {
        0 => panic!("Kubernetes manifest holds no object"),
        1 => objects.remove(0),
        _ => serde_json::json!({"apiVersion": "v1", "kind": "List", "items": objects}),
    }
}

fn rendered_manifest(output: &RenderDirectory, template: &str, expected: &[(&str, &str)]) -> Value {
    let path = output
        .path()
        .join("runtime-operator/templates")
        .join(template);
    let manifest = fs::read(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    decode_manifest(&manifest, expected)
}

fn render_host_objects(root: &Path, values: &[&str]) -> (Value, Value) {
    let output = render_chart(root, "wamn-host", values);
    (
        rendered_manifest(&output, "runtime/deployment.yaml", &[DEPLOYMENT]),
        rendered_manifest(&output, "runtime/networkpolicy.yaml", &[NETWORK_POLICY]),
    )
}

fn host_container(deployment: &Value) -> &Value {
    let containers = deployment["spec"]["template"]["spec"]["containers"]
        .as_array()
        .expect("rendered host Deployment carries containers");
    assert_eq!(
        containers.len(),
        1,
        "host chart rendered an unexpected sidecar"
    );
    &containers[0]
}

fn assert_native_host_controls(
    deployment: &Value,
    network_policy: &Value,
    concurrent_starts: &str,
    cpu_request: &str,
    cpu_limit: &str,
) {
    let pod = &deployment["spec"]["template"]["spec"];
    let container = host_container(deployment);
    assert_eq!(deployment["metadata"]["namespace"], "wamn-system");
    assert_eq!(pod["terminationGracePeriodSeconds"], 70);
    for (name, expected) in [
        ("WASH_DEFAULT_HEAP_MEMORY", "256MiB"),
        ("WASH_CORE_INSTANCES", "512"),
        ("WASH_GUEST_MEMORY_MODE", "count"),
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
    assert!(
        container["env"]
            .as_array()
            .expect("host environment is an array")
            .iter()
            .all(|entry| entry["name"] != "WASH_HOST_MAX_GUEST_MEMORY"),
        "the host derives its guest budget from the cgroup limit"
    );
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

    let registry_volume = pod["volumes"]
        .as_array()
        .expect("host volumes are an array")
        .iter()
        .find(|volume| volume["name"] == "registry-pull")
        .expect("host must mount the pull-only registry credential");
    assert_eq!(
        registry_volume["secret"],
        serde_json::json!({
            "secretName": "wamn-registry-pull",
            "optional": false,
            "items": [{"key": ".dockerconfigjson", "path": "config.json"}],
        }),
        "native volumes passthrough must preserve the mandatory registry Secret"
    );
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
        ]}]),
        "native policy must allow traffic and probe ports without changing source scope"
    );
    assert!(network_policy["spec"].get("egress").is_none());
}

fn assert_events_overlay_remains_namespace_scoped(root: &Path) {
    let values = read_repository_file(root, VALUES);
    let overlay = read_repository_file(root, EVENTS_RBAC);
    let output = render_chart(root, "wamn", &[VALUES]);
    let namespace_roles = rendered_manifest(
        &output,
        "operator/workload-namespace-role.yaml",
        &[ROLE, ROLE_BINDING],
    );
    let namespace_roles = namespace_roles["items"]
        .as_array()
        .expect("watched namespaces render operator Roles and RoleBindings");
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
        assert_eq!(objects.len(), 2, "Events overlay must stay narrowly scoped");
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
        let chart_binding = namespace_roles
            .iter()
            .find(|object| {
                object["kind"] == "RoleBinding" && object["metadata"]["namespace"] == namespace
            })
            .expect("watched namespace has the chart's operator RoleBinding");
        assert_eq!(
            binding["subjects"], chart_binding["subjects"],
            "Events overlay must bind the existing operator identity"
        );
    }
}

#[test]
#[ignore = "requires: helm"]
fn the_host_tier_renders_the_native_host_controls() {
    wamn_test_postgres::require_prerequisites(&["helm"]);
    let root = repository_root();
    let (base, base_policy) = render_host_objects(&root, &[HOST_VALUES]);
    assert_native_host_controls(&base, &base_policy, "4", "2", "6");
    assert_events_overlay_remains_namespace_scoped(&root);
}

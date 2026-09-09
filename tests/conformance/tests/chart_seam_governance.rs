//! Guards registry credential mounts and native runtime controls in the chart.
//!
//! The chart is pulled from OCI at install time and is not in this repository,
//! so no hermetic test can render it. What is guarded instead is the coupling
//! that makes a chart move visible: `deploy/infra/values-wamn.yaml` records the
//! seam the mount depends on against the pinned runtime revision whose chart was
//! inspected, and that revision is the pin in the root `Cargo.toml`. Moving the
//! pin without re-inspecting the chart fails here (wamn-0h0g.15.54).
//!
//! The same reasoning covers the two-release split the chart is installed under
//! (wamn-0h0g.15.15): one cluster-singleton operator release plus one host-tier
//! release per environment. Convergence itself needs a cluster, so what is
//! guarded here is the agreement between the files — the host tier names the
//! environment namespace, and the operator's watch/host namespace lists and the
//! component workloads' namespace and `environment` must all follow it, or the
//! operator's `allowSharedHosts: false` lock refuses the workload at schedule
//! time with `CrossEnvironmentSchedulingDenied`.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

const VALUES: &str = "deploy/infra/values-wamn.yaml";
const HOST_VALUES: &str = "deploy/platform/values-host-default.yaml";
const RECEIVING_HOST_VALUES: &str = "deploy/platform/values-host-receiving-pat.yaml";
const WMS_HOST_VALUES: &str = "deploy/platform/values-host-wms-pat.yaml";
const EVENTS_RBAC: &str = "deploy/platform/runtime-operator-events-rbac.example.yaml";
const EXECUTOR: &str = "deploy/platform/executor.yaml";
const SOCKPROBE: &str = "components/fixtures/sockprobe/src/main.rs";
const EXPECTED_CHART_VERSION: &str = "2.9.0";
static RENDER_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const EXPECTED_RUNTIME_REVISION: &str = "68ebece9";

/// The component workloads the operator schedules onto the host tier — the only
/// two in scope for operator management (ruling wamn-0h0g.13.46).
const WORKLOADS: [&str; 2] = [
    "deploy/platform/http-route-workload.example.yaml",
    "deploy/platform/materializer.example.yaml",
];

const RE_VERIFY: &str = "grep -n 'with \\.volumes\\|with \\.volumeMounts'";

/// Chart halves the host-tier release must leave to the operator release, each
/// paired with why it is that release's alone.
const HOST_RELEASE_DISABLED: [(&str, &str); 3] = [
    (
        "operator:\n  enabled: false",
        "the operator and its cluster-scoped CRDs are a per-cluster singleton",
    ),
    (
        "nats:\n  enabled: false",
        "the control-plane NATS the hosts heartbeat into belongs to the operator release",
    ),
    (
        "generate: false",
        "the operator release owns the CA and every certificate Secret in its namespace",
    ),
];

/// The values key the manifest mount sets, paired with the chart template
/// expression that renders it — the two halves a rename would separate.
const SEAM: [(&str, &str); 2] = [
    ("runtime.hostGroups[].volumes", "{{- with .volumes }}"),
    (
        "runtime.hostGroups[].volumeMounts",
        "{{- with .volumeMounts }}",
    ),
];

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

fn decode_manifest(root: &Path, manifest: &[u8]) -> Value {
    let mut kubectl = Command::new("kubectl")
        .current_dir(root)
        .args([
            "create",
            "--dry-run=client",
            "--validate=false",
            "--filename=-",
            "--output=json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the Kubernetes structural decoder");
    kubectl
        .stdin
        .take()
        .expect("kubectl stdin is piped")
        .write_all(manifest)
        .expect("send rendered chart to the structural decoder");
    let decoded = kubectl
        .wait_with_output()
        .expect("join the Kubernetes structural decoder");
    assert!(
        decoded.status.success(),
        "Kubernetes decode failed: {}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    serde_json::from_slice(&decoded.stdout).expect("Kubernetes manifest decodes as JSON")
}

fn rendered_manifest(root: &Path, output: &RenderDirectory, template: &str) -> Value {
    let path = output
        .path()
        .join("runtime-operator/templates")
        .join(template);
    let manifest = fs::read(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    decode_manifest(root, &manifest)
}

fn render_host_objects(root: &Path, values: &[&str]) -> (Value, Value) {
    let output = render_chart(root, "wamn-host", values);
    (
        rendered_manifest(root, &output, "runtime/deployment.yaml"),
        rendered_manifest(root, &output, "runtime/networkpolicy.yaml"),
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

fn environment_entry<'a>(container: &'a Value, name: &str) -> Option<&'a Value> {
    container["env"]
        .as_array()
        .expect("rendered host container carries env")
        .iter()
        .find(|entry| entry["name"] == name)
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
        ("WASH_HOST_MAX_GUEST_MEMORY", "4Gi"),
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

fn assert_executor_native_controls(root: &Path) {
    let source = read_repository_file(root, EXECUTOR);
    let manifest = decode_manifest(root, source.as_bytes());
    let deployment = manifest["items"]
        .as_array()
        .expect("executor manifest contains a Deployment and disruption budget")
        .iter()
        .find(|object| object["kind"] == "Deployment")
        .expect("executor Deployment is present");
    let pod = &deployment["spec"]["template"]["spec"];
    // Five seconds to drain, one to stop probes, two to flush, one to stop Tokio.
    assert!(
        pod["terminationGracePeriodSeconds"]
            .as_u64()
            .is_some_and(|seconds| seconds > 9),
        "executor grace must exceed its complete nine-second shutdown envelope"
    );
    let container = host_container(deployment);
    assert_eq!(
        environment_entry(container, "WASH_GUEST_MEMORY_MODE")
            .and_then(|entry| entry["value"].as_str()),
        Some("count")
    );
    for (name, path) in [
        ("startupProbe", "/livez"),
        ("livenessProbe", "/livez"),
        ("readinessProbe", "/readyz"),
    ] {
        assert_eq!(container[name]["httpGet"]["path"], path);
        assert_eq!(container[name]["httpGet"]["port"], "readiness");
        assert!(container[name].get("tcpSocket").is_none());
    }
    assert!(
        container["ports"]
            .as_array()
            .expect("executor exposes its health port")
            .iter()
            .any(|port| port["name"] == "readiness" && port["containerPort"] == 8089)
    );
}

fn assert_events_overlay_remains_namespace_scoped(root: &Path) {
    let values = read_repository_file(root, VALUES);
    let overlay = read_repository_file(root, EVENTS_RBAC);
    let output = render_chart(root, "wamn", &[VALUES]);
    let namespace_roles = rendered_manifest(root, &output, "operator/workload-namespace-role.yaml");
    let namespace_roles = namespace_roles["items"]
        .as_array()
        .expect("watched namespaces render operator Roles and RoleBindings");
    let namespaces = list_items(&values, "watchNamespaces");
    assert!(
        !namespaces.is_empty(),
        "operator must name its watched namespaces"
    );
    for namespace in namespaces {
        let manifest = decode_manifest(
            root,
            overlay
                .replace("__ENVIRONMENT_NAMESPACE__", namespace)
                .as_bytes(),
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

fn installed_chart_version(values: &str) -> &str {
    values
        .split_once("--version ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .expect("values file must document the chart version its install pulls")
}

/// Every `<key>: <value>` mapping in a YAML document, comments dropped and list
/// items (`- <key>: …`) left out — a list item is a different object, and both
/// workload files carry `- namespace: <wit-namespace>` entries under
/// `hostInterfaces` that are not Kubernetes namespaces at all.
fn mapping_values<'a>(document: &'a str, key: &str) -> Vec<&'a str> {
    let needle = format!("{key}: ");
    document
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.strip_prefix(needle.as_str()))
        .collect()
}

/// The items of a YAML list opened by `<key>:`, up to the first following line
/// that is not one — a comment between the key and its items ends the list, and
/// none of the files read here puts one there.
fn list_items<'a>(document: &'a str, key: &str) -> Vec<&'a str> {
    let opener = format!("{key}:");
    let mut items = Vec::new();
    let mut open = false;
    for line in document.lines().map(str::trim) {
        if line == opener.as_str() {
            open = true;
            continue;
        }
        if open {
            match line.strip_prefix("- ") {
                Some(item) => items.push(item),
                None => break,
            }
        }
    }
    items
}

/// Whether `entries` holds `wanted`. Written out rather than `contains`, which
/// would demand both sides borrow for the same region: the values compared here
/// come from different files, read at different points.
fn holds(entries: &[&str], wanted: &str) -> bool {
    for entry in entries {
        if *entry == wanted {
            return true;
        }
    }

    false
}

/// The environment namespaces the host tier deploys its host groups into. This
/// is the authority every other namespace in the split follows: the host Pod's
/// namespace is what the operator records as `Host.Environment`.
fn host_tier_namespaces(host_values: &str) -> Vec<&str> {
    let namespaces = mapping_values(host_values, "namespace");
    assert!(
        !namespaces.is_empty(),
        "{HOST_VALUES} must give every runtime.hostGroups[] entry an explicit \
         `namespace:` — it is the environment identity the operator's \
         allowSharedHosts=false lock is enforced against"
    );

    namespaces
}

#[test]
fn seam_record_names_both_undeclared_passthrough_keys() {
    let root = repository_root();
    let values = read_repository_file(&root, VALUES);

    for (key, expression) in SEAM {
        assert!(
            values.contains(key),
            "{VALUES} must record the values key {key:?} that reaches the seam"
        );
        assert!(
            values.contains(expression),
            "{VALUES} must record the chart template expression {expression:?} \
             that {key} renders through"
        );
    }
    assert!(
        values.contains(EXPECTED_RUNTIME_REVISION),
        "{VALUES} must record that the seam was re-verified at the pinned runtime revision"
    );
    assert!(
        values.contains(RE_VERIFY),
        "{VALUES} must record the command that re-verifies the seam at a new pin"
    );
}

#[test]
fn operator_install_command_pins_the_upstream_chart() {
    let root = repository_root();
    let values = read_repository_file(&root, VALUES);

    let installed = installed_chart_version(&values);
    assert_eq!(
        installed, EXPECTED_CHART_VERSION,
        "{VALUES} must install the chart matching the pinned upstream runtime"
    );
}

#[test]
fn host_and_executor_use_the_native_v2_9_memory_contract() {
    let root = repository_root();
    let host_values = read_repository_file(&root, HOST_VALUES);
    let executor = read_repository_file(&root, EXECUTOR);

    for marker in [
        "memory: \"4Gi\"",
        "defaultHeapMemory: \"256MiB\"",
        "coreInstances: 512",
    ] {
        assert!(
            host_values.contains(marker),
            "{HOST_VALUES} must carry the native v2.9 memory setting {marker:?}"
        );
    }
    for marker in [
        "name: WASH_HOST_MAX_GUEST_MEMORY",
        "resource: limits.memory",
        "name: WASH_DEFAULT_HEAP_MEMORY",
        "value: \"256MiB\"",
        "name: WASH_CORE_INSTANCES",
        "value: \"512\"",
    ] {
        assert!(
            executor.contains(marker),
            "{EXECUTOR} must carry the native v2.9 memory setting {marker:?}"
        );
    }

    for legacy in [
        "WAMN_MEMORY_CEILING_MB",
        "WAMN_DISABLE_INSTANCE_POOLING",
        "--pool-slots",
        "--pool-memory-cap-bytes",
    ] {
        assert!(
            !host_values.contains(legacy) && !executor.contains(legacy),
            "the native memory cutover must remove legacy setting {legacy:?}"
        );
    }
}

#[test]
fn synchronous_host_groups_keep_a_warm_replica() {
    let root = repository_root();
    let host_values = read_repository_file(&root, HOST_VALUES);
    let host_groups = host_values
        .split_once("\n  hostGroups:\n")
        .map(|(_, groups)| groups)
        .expect("host-tier values must carry runtime.hostGroups");
    let profiles: Vec<_> = host_groups
        .strip_prefix("    - name: ")
        .expect("runtime.hostGroups must use named chart entries")
        .split("\n    - name: ")
        .collect();

    assert!(
        !profiles.is_empty(),
        "{HOST_VALUES} must define at least one runtime.hostGroups[] entry"
    );

    let mut synchronous_groups = 0;
    for profile in profiles {
        let name = profile.lines().next().expect("host group must name itself");
        let declared_replicas = mapping_values(profile, "replicas");
        assert_eq!(
            declared_replicas.len(),
            1,
            "host group {name:?} must carry exactly one explicit replicas value"
        );
        let replicas: u32 = declared_replicas[0]
            .parse()
            .unwrap_or_else(|error| panic!("host group {name:?} has invalid replicas: {error}"));

        if profile.contains("\n      http:\n        enabled: true") {
            synchronous_groups += 1;
            assert!(
                replicas >= 1,
                "host group {:?} enables the chart's native HTTP listener, Service port, and readiness probe, so it serves synchronous work and must keep at least one warm replica",
                name
            );
        }
    }

    assert!(
        synchronous_groups >= 1,
        "{HOST_VALUES} is the shipped synchronous profile and must keep an explicit http.enabled=true host group"
    );
}

#[test]
fn legacy_raw_socket_opt_in_is_absent_from_the_shipped_contract() {
    let root = repository_root();
    for path in [HOST_VALUES, EXECUTOR, SOCKPROBE] {
        let source = read_repository_file(&root, path);
        for legacy in ["wamn.allow-raw-sockets", "WAMN_ALLOW_RAW_SOCKETS"] {
            assert!(
                !source.contains(legacy),
                "{path} must not retain the removed raw-socket opt-in {legacy:?}"
            );
        }
    }
}

#[test]
fn both_releases_install_the_same_pinned_chart() {
    let root = repository_root();
    let values = read_repository_file(&root, VALUES);
    let host_values = read_repository_file(&root, HOST_VALUES);

    assert_eq!(
        installed_chart_version(&host_values),
        installed_chart_version(&values),
        "{HOST_VALUES} and {VALUES} install the same chart, and the pin is per \
         cluster (ruling wamn-0h0g.13.49): two versions in one cluster renders \
         the host tier from a chart the operator was not installed from"
    );
}

#[test]
fn the_operator_release_locks_scheduling_to_the_workload_namespace() {
    let root = repository_root();
    let values = read_repository_file(&root, VALUES);

    assert!(
        values.contains("allowSharedHosts: false"),
        "{VALUES} must set operator.allowSharedHosts: false. The chart ships it \
         true, which lets a WorkloadDeployment in one namespace schedule onto \
         hosts in another just by naming spec.template.spec.environment"
    );
    assert!(
        !values.contains("allowSharedHosts: true"),
        "{VALUES} must not re-enable operator.allowSharedHosts: it is the \
         wrong-target lock, and the operator only refuses a cross-environment \
         target while it is false"
    );
    assert!(
        values.contains("CrossEnvironmentSchedulingDenied"),
        "{VALUES} must record the refusal the lock produces, so the value is not \
         softened later as though it were cosmetic"
    );
}

#[test]
fn the_operator_release_covers_every_host_tier_namespace() {
    let root = repository_root();
    let values = read_repository_file(&root, VALUES);
    let host_values = read_repository_file(&root, HOST_VALUES);
    let namespaces = host_tier_namespaces(&host_values);

    assert!(
        values.contains("hostGroups: []"),
        "{VALUES} is the cluster-singleton operator release and must carry no \
         host groups: the host tier is one Helm release per environment \
         ({HOST_VALUES}), ruling wamn-0h0g.13.50"
    );

    for key in ["watchNamespaces", "hostNamespaces"] {
        let declared = list_items(&values, key);
        assert!(
            !declared.is_empty(),
            "{VALUES} must list this cluster's environment namespaces under \
             operator.{key}; the chart's `[]` default watches every namespace \
             and assumes host Pods run only in the operator's own"
        );
        for namespace in &namespaces {
            assert!(
                holds(&declared, namespace),
                "{VALUES} operator.{key} does not cover {namespace:?}, which \
                 {HOST_VALUES} deploys a host group into. The chart derives \
                 host namespaces from runtime.hostGroups[] in the SAME release \
                 only, and this release has none, so an uncovered namespace \
                 leaves the operator without Pod RBAC or cache access there"
            );
        }
    }
}

#[test]
fn the_host_release_leaves_the_cluster_singletons_to_the_operator_release() {
    let root = repository_root();
    let values = read_repository_file(&root, VALUES);
    let host_values = read_repository_file(&root, HOST_VALUES);

    for (setting, why) in HOST_RELEASE_DISABLED {
        assert!(
            host_values.contains(setting),
            "{HOST_VALUES} must set {setting:?} — {why}, so rendering it from \
             the host release either installs a duplicate or collides with the \
             operator release on Helm ownership metadata"
        );
    }
    assert!(
        !values.contains("operator:\n  enabled: false"),
        "{VALUES} is the release that installs the operator; disabling it there \
         leaves the cluster with host groups and no controller"
    );
}

#[test]
fn the_deprecated_gateway_is_off_in_both_releases() {
    let root = repository_root();
    for file in [VALUES, HOST_VALUES] {
        let document = read_repository_file(&root, file);
        assert!(
            document.contains("gateway:\n  enabled: false"),
            "{file} must keep the deprecated runtime-gateway off: HTTP exposure \
             rides operator-managed EndpointSlices on standard Services, and the \
             gateway's NodePort collides with workload Services on 30950"
        );
    }
}

#[test]
fn the_component_workloads_target_the_host_tier_environment() {
    let root = repository_root();
    let host_values = read_repository_file(&root, HOST_VALUES);
    let namespaces = host_tier_namespaces(&host_values);

    for workload in WORKLOADS {
        let document = read_repository_file(&root, workload);

        let environments = mapping_values(&document, "environment");
        assert!(
            !environments.is_empty(),
            "{workload} must name the environment it schedules into under \
             spec.template.spec.environment, so the target is legible rather \
             than implied by the object's own namespace"
        );

        for environment in &environments {
            assert!(
                holds(&namespaces, environment),
                "{workload} targets environment {environment:?}, which no host \
                 group in {HOST_VALUES} runs in — no host will ever report that \
                 Environment, so the workload never schedules"
            );
        }

        for namespace in mapping_values(&document, "namespace") {
            assert!(
                holds(&namespaces, namespace),
                "{workload} lives in {namespace:?}, which no host group in \
                 {HOST_VALUES} runs in"
            );
            assert!(
                holds(&environments, namespace),
                "{workload} lives in {namespace:?} but does not name it as its \
                 environment. Under operator.allowSharedHosts: false the \
                 operator refuses any environment other than the workload's own \
                 namespace with CrossEnvironmentSchedulingDenied"
            );
        }

        let selected = mapping_values(&document, "hostgroup");
        assert!(
            !selected.is_empty(),
            "{workload} must select a host group under spec.template.spec.hostSelector"
        );
        for group in selected {
            let entry = format!("- name: {group}");
            assert!(
                host_values.contains(&entry),
                "{workload} selects host group {group:?}, which {HOST_VALUES} \
                 does not define; the selector matches the label the host \
                 reports from the chart's --host-group"
            );
        }
    }
}

#[test]
#[ignore = "pulls and renders the pinned OCI chart; run via [RECEIVING-HOST-OVERLAY]"]
fn receiving_pat_overlay_renders_a_complete_scoped_host() {
    let root = repository_root();
    let (base, base_policy) = render_host_objects(&root, &[HOST_VALUES]);
    let (receiving, receiving_policy) =
        render_host_objects(&root, &[HOST_VALUES, RECEIVING_HOST_VALUES]);
    let (wms, wms_policy) = render_host_objects(&root, &[HOST_VALUES, WMS_HOST_VALUES]);
    assert_native_host_controls(&base, &base_policy, "4", "2", "6");
    assert_native_host_controls(&receiving, &receiving_policy, "4", "2", "6");
    assert_native_host_controls(&wms, &wms_policy, "1", "250m", "2");
    assert_executor_native_controls(&root);
    assert_events_overlay_remains_namespace_scoped(&root);
    let base_container = host_container(&base);
    let receiving_container = host_container(&receiving);

    for name in [
        "WAMN_ORG",
        "WAMN_PROJECT",
        "WAMN_SCHEMA",
        "WAMN_SYSTEM_URL",
        "WAMN_EXECUTOR_PLATFORM_PG_URL",
        "WAMN_HTTP_ADMITTER_PG_URL",
        "WAMN_EVENT_MATERIALIZER_PG_URL",
    ] {
        assert!(
            environment_entry(base_container, name).is_none(),
            "generic host unexpectedly carries Receiving setting {name}"
        );
    }
    let base_args = base_container["args"]
        .as_array()
        .expect("rendered generic host carries args");
    assert!(
        base_args.iter().all(|argument| {
            argument
                .as_str()
                .is_none_or(|argument| !argument.starts_with("--release-"))
        }),
        "generic host unexpectedly carries a release coordinate"
    );
    let receiving_args = receiving_container["args"]
        .as_array()
        .expect("rendered Receiving host carries args");
    assert_eq!(
        receiving_args.get(..base_args.len()),
        Some(base_args.as_slice()),
        "Receiving overlay changed or dropped a generic host argument"
    );
    assert_eq!(
        receiving_args.len(),
        base_args.len() + 2,
        "Receiving overlay must add only the two release arguments"
    );

    let receiving_names = [
        "WAMN_ORG",
        "WAMN_PROJECT",
        "WAMN_SCHEMA",
        "WAMN_SYSTEM_URL",
        "WAMN_EXECUTOR_PLATFORM_PG_URL",
        "WAMN_HTTP_ADMITTER_PG_URL",
        "WAMN_EVENT_MATERIALIZER_PG_URL",
    ];
    let base_env = base_container["env"]
        .as_array()
        .expect("rendered generic host carries env");
    let inherited_env = receiving_container["env"]
        .as_array()
        .expect("rendered Receiving host carries env")
        .iter()
        .filter(|entry| {
            entry["name"]
                .as_str()
                .is_none_or(|name| !receiving_names.contains(&name))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        inherited_env,
        base_env.iter().collect::<Vec<_>>(),
        "Receiving overlay changed or dropped a generic host environment entry"
    );
    assert_eq!(
        receiving_container["env"]
            .as_array()
            .expect("rendered Receiving host carries env")
            .len(),
        base_env.len() + receiving_names.len(),
        "Receiving overlay must add exactly its seven scoped environment entries"
    );

    for (name, expected) in [
        ("WAMN_ORG", "acme"),
        ("WAMN_PROJECT", "receiving"),
        ("WAMN_SCHEMA", "receiving"),
    ] {
        assert_eq!(
            environment_entry(receiving_container, name).and_then(|entry| entry["value"].as_str()),
            Some(expected),
            "Receiving host rendered the wrong trusted {name} value"
        );
    }
    for (name, secret) in [
        (
            "WAMN_SYSTEM_URL",
            "wamn-identity-reader-acme--receiving--dev",
        ),
        (
            "WAMN_EXECUTOR_PLATFORM_PG_URL",
            "wamn-executor-platform-acme--receiving--dev",
        ),
        (
            "WAMN_HTTP_ADMITTER_PG_URL",
            "wamn-http-admitter-acme--receiving--dev",
        ),
        (
            "WAMN_EVENT_MATERIALIZER_PG_URL",
            "wamn-event-materializer-acme--receiving--dev",
        ),
    ] {
        let selector = &environment_entry(receiving_container, name)
            .unwrap_or_else(|| panic!("Receiving host omitted {name}"))["valueFrom"]["secretKeyRef"];
        assert_eq!(selector["name"], secret, "{name} names the wrong Secret");
        assert_eq!(selector["key"], "url", "{name} reads the wrong key");
        assert_eq!(
            selector["optional"], false,
            "{name} must fail deployment when its Secret is absent"
        );
    }

    assert_eq!(base["spec"]["replicas"], receiving["spec"]["replicas"]);
    assert_eq!(
        base["spec"]["template"]["spec"]["volumes"],
        receiving["spec"]["template"]["spec"]["volumes"],
        "Receiving overlay lost the base host volumes through Helm list replacement"
    );
    for field in ["image", "ports", "resources", "volumeMounts"] {
        assert_eq!(
            base_container[field], receiving_container[field],
            "Receiving overlay lost the base host {field} through Helm list replacement"
        );
    }

    let release_args: Vec<&str> = receiving_args
        .iter()
        .filter_map(Value::as_str)
        .filter(|argument| argument.starts_with("--release-"))
        .collect();
    assert_eq!(
        release_args,
        [
            "--release-artifact-base=registry.wamn-system.svc.cluster.local:5000/wamn/releases",
            "--release-manifest-digest=sha256:0000000000000000000000000000000000000000000000000000000000000000",
        ]
    );
}

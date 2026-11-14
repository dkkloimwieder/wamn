//! The host key reader accepts the permissions rendered by the chart.

#[path = "../../environment/tests/support/render_fixture.rs"]
pub mod render_fixture;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize as _;
use serde_json::Value;

static RENDER_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const DEPLOYMENT: (&str, &str) = ("apps/v1", "Deployment");

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("platform-install lives three levels below the repository")
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
        "deploy/platform/environment/Chart.yaml",
    ))
    .expect("release chart metadata");
    let version = metadata["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|dependency| dependency["name"] == "runtime-operator")
        .and_then(|dependency| dependency["version"].as_str())
        .expect("the release chart selects the operator version");
    let chart = format!("deploy/platform/environment/charts/runtime-operator-{version}.tgz");
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
    use render_fixture::{EventIdentity, HostValuesInput};
    let values = render_fixture::host_values(
        &HostValuesInput {
            namespace: "platform".into(),
            host_tag: "dev".into(),
            replicas: 3,
            component_artifact_base: "registry.example/components".into(),
            stream_replicas: 1,
            dup_window_secs: 120,
            event: EventIdentity {
                org: "acme".into(),
                project: "wms".into(),
                environment: "dev".into(),
            },
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
    wamn_connections::oauth_encryption::Envelope::from_file(&path)
        .unwrap_or_else(|error| panic!("rendered key {key} with mode {mode:o}: {error}"));
}

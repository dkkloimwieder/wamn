//! The key reader accepts the permissions rendered for the control service.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static RENDER_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const DEPLOYMENT: (&str, &str) = ("apps/v1", "Deployment");

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("platform-install lives two levels below the repository")
        .to_path_buf()
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

fn render_control_objects(root: &Path) -> (Vec<Value>, String) {
    let chart = root.join("deploy/platform/release");
    let metadata: Value =
        serde_yaml::from_slice(&fs::read(chart.join("Chart.yaml")).unwrap()).unwrap();
    let name = "wamn-control";
    let objects = wamn_kubernetes::helm::template(&serde_json::json!({
        "chart": chart, "version": metadata["version"], "name": name, "namespace": "platform",
        "values": {"controlService": true, "org": "acme", "control": {
            "image": "wamn-control:dev", "configuration": {}, "account": "control",
            "administration": "control-administration", "natsSecret": "control-nats",
            "operatorSecret": "control-operator", "registrySecret": "registry-pull",
            "oauthSecret": "wamn-oauth"
        }}
    }))
    .expect("render the control service");
    (objects, name.to_owned())
}

fn control_container(deployment: &Value) -> &Value {
    let containers = deployment["spec"]["template"]["spec"]["containers"]
        .as_array()
        .expect("rendered control Deployment carries containers");
    containers
        .iter()
        .find(|container| container["name"] == "control")
        .expect("rendered control container")
}

#[test]
#[ignore = "requires: bin:helm"]
fn the_host_key_reader_accepts_the_rendered_secret_permissions() {
    use std::os::unix::fs::PermissionsExt as _;

    let (objects, name) = render_control_objects(&repository_root());
    let deployment = rendered_object(&objects, DEPLOYMENT, &name, "platform");
    let pod = &deployment["spec"]["template"]["spec"];
    let container = control_container(deployment);
    let key = container["env"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "WAMN_OAUTH_KEY_FILE")
        .expect("the control service declares its OAuth key file")["value"]
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

//! The collector configuration passes its deployed image validation command.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("provision lives two levels below the repository")
        .to_path_buf()
}

fn find<'a>(documents: &'a [Value], type_: &str, name: &str) -> &'a Value {
    documents
        .iter()
        .find(|document| document["kind"] == type_ && document["metadata"]["name"] == name)
        .unwrap_or_else(|| panic!("no {type_} {name}"))
}

#[test]
#[ignore = "requires: bin:helm, bin:docker"]
fn the_collector_configuration_passes_the_pinned_image_validate() {
    let manifest =
        wamn_test_infrastructure::collector::deployed_collector(&repository_root()).unwrap();
    let pod = &find(&manifest, "Deployment", "otel-collector")["spec"]["template"]["spec"];
    let container = &pod["containers"][0];
    let image = container["image"]
        .as_str()
        .expect("the container names an image");
    let security = &pod["securityContext"];
    let uid = security["runAsUser"].as_u64().expect("the pod sets a UID");
    let gid = security["runAsGroup"]
        .as_u64()
        .expect("the pod sets a group ID");
    let configuration = find(&manifest, "ConfigMap", "otel-collector")["data"]["relay"]
        .as_str()
        .expect("the ConfigMap holds config.yaml");
    let values = container["env"].as_array().unwrap();

    let directory =
        std::env::temp_dir().join(format!("wamn-otel-collector-{}", std::process::id()));
    fs::create_dir_all(&directory).expect("create the configuration directory");
    fs::write(directory.join("config.yaml"), configuration).expect("write the configuration");
    // The deployed UID needs access independent of the host umask.
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
        .expect("make the configuration directory traversable by the image user");
    fs::set_permissions(
        directory.join("config.yaml"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("make the configuration readable by the image user");
    let mut docker = Command::new("docker");
    docker
        .args(["run", "--rm", "--user"])
        .arg(format!("{uid}:{gid}"))
        .arg("-v")
        .arg(format!("{}:/configuration:ro", directory.display()));
    for variable in values {
        let value = variable["value"].as_str().or_else(|| {
            let reference = &variable["valueFrom"]["configMapKeyRef"];
            find(&manifest, "ConfigMap", reference["name"].as_str()?)["data"]
                [reference["key"].as_str()?]
            .as_str()
        });
        if let (Some(name), Some(value)) = (variable["name"].as_str(), value) {
            docker.arg("-e").arg(format!("{name}={value}"));
        }
    }
    let output = docker
        .args([image, "validate", "--config=/configuration/config.yaml"])
        .output()
        .expect("run docker");
    fs::remove_dir_all(&directory).expect("remove the configuration directory");
    assert!(
        output.status.success(),
        "validate refused the configuration:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

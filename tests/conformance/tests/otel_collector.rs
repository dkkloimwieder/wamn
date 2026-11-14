//! The collector manifest (docs/plan/observability.md section 4): its image
//! is pinned by digest, and its configuration passes the `validate`
//! subcommand of that image with the values of wamn-dev (wamn-22lsr.1).
//! The test runs Docker.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn find<'a>(documents: &'a [Value], type_: &str, name: &str) -> &'a Value {
    documents
        .iter()
        .find(|document| document["kind"] == type_ && document["metadata"]["name"] == name)
        .unwrap_or_else(|| panic!("no {type_} {name}"))
}

#[test]
fn the_collector_configuration_passes_the_pinned_image_validate() {
    let manifest =
        wamn_test_infrastructure::telemetry::deployed_collector(&repository_root()).unwrap();
    let image =
        find(&manifest, "Deployment", "otel-collector")["spec"]["template"]["spec"]["containers"]
            [0]["image"]
            .as_str()
            .expect("the container names an image");
    assert!(
        image.contains("@sha256:"),
        "the collector image {image} is not pinned by digest"
    );
    let configuration = find(&manifest, "ConfigMap", "otel-collector")["data"]["relay"]
        .as_str()
        .expect("the ConfigMap holds config.yaml");
    let deployment = find(&manifest, "Deployment", "otel-collector");
    let values = deployment["spec"]["template"]["spec"]["containers"][0]["env"]
        .as_array()
        .unwrap();

    let directory =
        std::env::temp_dir().join(format!("wamn-otel-collector-{}", std::process::id()));
    fs::create_dir_all(&directory).expect("create the configuration directory");
    fs::write(directory.join("config.yaml"), configuration).expect("write the configuration");
    // Docker keeps the image user, which needs access independent of the host umask.
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
        .expect("make the configuration directory traversable by the image user");
    fs::set_permissions(
        directory.join("config.yaml"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("make the configuration readable by the image user");
    let mut docker = Command::new("docker");
    docker
        .args(["run", "--rm", "-v"])
        .arg(format!("{}:/configuration:ro", directory.display()));
    for variable in values {
        if let (Some(name), Some(value)) = (variable["name"].as_str(), variable["value"].as_str()) {
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

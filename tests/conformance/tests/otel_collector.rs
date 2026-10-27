//! The collector manifest (docs/plan/observability.md section 4): its image
//! is pinned by digest, and its configuration passes the `validate`
//! subcommand of that image with the values of wamn-dev (wamn-22lsr.1).
//! The test runs Docker.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize as _;
use serde_yaml::Value;

const MANIFEST: &str = "deploy/platform/otel-collector.yaml";
const VALUES: &str = "deploy/gcp/otel-collector-values.yaml";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn documents(path: &str) -> Vec<Value> {
    let path = repository_root().join(path);
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_yaml::Deserializer::from_str(&source)
        .map(|document| Value::deserialize(document).expect("each document parses"))
        .collect()
}

fn find<'a>(documents: &'a [Value], kind: &str, name: &str) -> &'a Value {
    documents
        .iter()
        .find(|document| document["kind"] == kind && document["metadata"]["name"] == name)
        .unwrap_or_else(|| panic!("no {kind} {name}"))
}

#[test]
fn the_collector_configuration_passes_the_pinned_image_validate() {
    let manifest = documents(MANIFEST);
    let image =
        find(&manifest, "Deployment", "otel-collector")["spec"]["template"]["spec"]["containers"]
            [0]["image"]
            .as_str()
            .expect("the container names an image");
    assert!(
        image.contains("@sha256:"),
        "the collector image {image} is not pinned by digest"
    );
    let configuration = find(&manifest, "ConfigMap", "otel-collector")["data"]["config.yaml"]
        .as_str()
        .expect("the ConfigMap holds config.yaml");
    let values = documents(VALUES);
    let values = find(&values, "ConfigMap", "otel-collector-values")["data"]
        .as_mapping()
        .expect("the values ConfigMap holds data");

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
    for (name, value) in values {
        let (name, value) = (
            name.as_str().expect("a name"),
            value.as_str().expect("a value"),
        );
        docker.arg("-e").arg(format!("{name}={value}"));
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

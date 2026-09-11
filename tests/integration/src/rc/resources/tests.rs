use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use sha2::Digest as _;
use tokio::process::Command;
use wamn_test_infrastructure::scratch::ScratchRoot;

use super::{Resources, finish_result, recorded};

fn directory() -> ScratchRoot {
    let path = std::env::temp_dir().join(format!("rc-results-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    ScratchRoot(path)
}

fn resources(root: &Path) -> Resources {
    let work = root.join("work");
    let evidence = root.join("results");
    fs::create_dir(&work).unwrap();
    fs::create_dir(&evidence).unwrap();
    Resources {
        repository: root.to_owned(),
        work,
        evidence,
        source: "test-source".into(),
        lifecycle: root.join("unused-lifecycle"),
        host_image: "unused-host".into(),
        gates_image: "unused-gates".into(),
        postgres_image: "unused-postgres".into(),
        owned: false,
    }
}

#[test]
fn final_hashes_cover_the_written_result_and_captured_files() {
    let root = directory();
    fs::write(root.path().join("command.log"), b"command output\n").unwrap();
    fs::create_dir(root.path().join("job")).unwrap();
    fs::write(root.path().join("job/result.json"), b"{\"passed\":true}").unwrap();
    let result = json!({"passed":true,"failure":null});
    finish_result(root.path(), &result).unwrap();
    let hashes = fs::read_to_string(root.path().join("evidence.sha256")).unwrap();
    assert_eq!(hashes.lines().count(), 3);
    for path in ["result.json", "command.log", "job/result.json"] {
        let bytes = fs::read(root.path().join(path)).unwrap();
        let digest = hex::encode(sha2::Sha256::digest(bytes));
        assert!(
            hashes
                .lines()
                .any(|line| line == format!("{digest}  {path}"))
        );
    }
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(root.path().join("result.json")).unwrap())
            .unwrap(),
        result
    );
}

#[test]
fn hash_write_failure_cannot_leave_a_passing_result() {
    let root = directory();
    fs::create_dir(root.path().join("evidence.sha256")).unwrap();
    assert!(finish_result(root.path(), &json!({"passed":true,"failure":null})).is_err());
    let result: Value =
        serde_json::from_slice(&fs::read(root.path().join("result.json")).unwrap()).unwrap();
    assert_eq!(result["passed"], false);
    assert!(!result["capture_failure"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn failed_command_retains_argv_status_and_exact_output_bytes() {
    let root = directory();
    let resources = resources(root.path());
    let script = "printf 'stdout\\000data\\n'; printf 'stderr\\377data\\n' >&2; exit 7";
    let result = recorded(
        &resources,
        "failure",
        Command::new("/bin/sh")
            .current_dir(root.path())
            .args(["-c", script]),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        fs::read(resources.evidence.join("failure.log")).unwrap(),
        b"stdout\0data\n"
    );
    assert_eq!(
        fs::read(resources.evidence.join("failure.stderr.log")).unwrap(),
        b"stderr\xffdata\n"
    );
    let command: Value =
        serde_json::from_slice(&fs::read(resources.evidence.join("failure-command.json")).unwrap())
            .unwrap();
    assert_eq!(command["argv"], json!(["/bin/sh", "-c", script]));
    assert_eq!(command["cwd"], json!(root.path()));
    let status: Value =
        serde_json::from_slice(&fs::read(resources.evidence.join("failure-result.json")).unwrap())
            .unwrap();
    assert_eq!(status["exit_code"], 7);
    assert_eq!(status["passed"], false);
    assert!(status["signal"].is_null());
}

#[tokio::test]
async fn spawn_failure_is_recorded_without_an_exit_code() {
    let root = directory();
    let resources = resources(root.path());
    let result = recorded(
        &resources,
        "missing",
        &mut Command::new(root.path().join("absent-command")),
    )
    .await;
    assert!(result.is_err());
    let status: Value =
        serde_json::from_slice(&fs::read(resources.evidence.join("missing-result.json")).unwrap())
            .unwrap();
    assert_eq!(status["passed"], false);
    assert!(status["exit_code"].is_null());
    assert!(!status["failure"].as_str().unwrap().is_empty());
    assert!(
        fs::read(resources.evidence.join("missing.log"))
            .unwrap()
            .is_empty()
    );
    assert!(
        fs::read(resources.evidence.join("missing.stderr.log"))
            .unwrap()
            .is_empty()
    );
}

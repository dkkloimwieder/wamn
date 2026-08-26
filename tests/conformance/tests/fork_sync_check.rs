//! Guards the executable fork-sync gate and its isolated Git environment.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

#[test]
fn dry_run_names_the_zero_patch_identity_and_every_gate_leg() {
    let tool = repository_root().join("tools/fork-sync-check");
    let output = Command::new(&tool)
        .args(["dry-run", "/tmp/fork checkout with spaces"])
        .output()
        .unwrap_or_else(|error| panic!("run {}: {error}", tool.display()));
    assert!(
        output.status.success(),
        "fork-sync-check dry-run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("dry-run output must be UTF-8");
    for required in [
        "expected-branch: wamn/2.8.0",
        "expected-tag: v2.8.0",
        "expected-revision: 5c4ec4a3d008b3f401d9e763515f434deebc9936",
        "git-config-global: /dev/null",
        "git-config-nosystem: 1",
        "format:",
        "wash-runtime:",
        "git-template-fixture:",
        "clone_template_",
    ] {
        assert!(
            stdout.contains(required),
            "dry-run output must contain {required:?}:\n{stdout}"
        );
    }
}

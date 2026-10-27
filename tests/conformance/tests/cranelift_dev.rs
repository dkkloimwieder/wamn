//! Conformance guard for the opt-in Cranelift developer build path.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const HELPER: &str = "tools/cargo-cranelift";
const BACKEND_FLAG: &str = "-Zcodegen-backend=cranelift";

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "wamn-cranelift-dev-{}-{serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn executable(path: &Path, source: &str) {
    fs::write(path, source).expect("write fake executable");
    let mut permissions = fs::metadata(path).expect("fake metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fake executable");
}

fn run_helper(
    root: &Path,
    current_directory: &Path,
    directory: &TestDirectory,
    arguments: &[&str],
) -> Output {
    let inherited_path = std::env::var_os("PATH").expect("test process must have PATH");
    let mut search_path = vec![directory.0.clone()];
    search_path.extend(std::env::split_paths(&inherited_path));
    let search_path = std::env::join_paths(search_path).expect("join fake PATH");

    Command::new(root.join(HELPER))
        .current_dir(current_directory)
        .args(arguments)
        .env("PATH", search_path)
        .env("WAMN_FAKE_CARGO_LOG", directory.path("cargo.log"))
        .output()
        .expect("run cargo-cranelift helper")
}

#[test]
fn cargo_cranelift_sets_the_exact_backend_and_refuses_non_dev_overrides() {
    const FAKE_CARGO: &str = r#"#!/usr/bin/env bash
set -euo pipefail
{
  printf 'RUSTFLAGS=%s\n' "${RUSTFLAGS-}"
  printf 'ARG=%s\n' "$@"
} >"$WAMN_FAKE_CARGO_LOG"
"#;

    let root = repository_root();
    let helper = root.join(HELPER);
    let helper_source = fs::read_to_string(&helper).expect("read cargo-cranelift helper");
    assert!(
        fs::metadata(&helper)
            .expect("helper metadata")
            .permissions()
            .mode()
            & 0o111
            != 0
    );
    assert!(
        helper_source
            .contains("exec env RUSTFLAGS=-Zcodegen-backend=cranelift cargo +nightly build \"$@\"")
    );

    let directory = TestDirectory::new();
    executable(&directory.path("cargo"), FAKE_CARGO);
    let output = run_helper(
        &root,
        &root,
        &directory,
        &["cranelift", "--locked", "-p", "wamn-router", "-vv"],
    );
    assert!(
        output.status.success(),
        "valid helper invocation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(directory.path("cargo.log")).expect("read fake Cargo log"),
        format!(
            "RUSTFLAGS={BACKEND_FLAG}\nARG=+nightly\nARG=build\nARG=--locked\nARG=-p\nARG=wamn-router\nARG=-vv\n"
        )
    );

    let refusal_cases: &[&[&str]] = &[
        &["cranelift", "-r"],
        &["cranelift", "--release"],
        &["cranelift", "--profile", "dev"],
        &["cranelift", "--profile=dev"],
        &["cranelift", "--target", "x86_64-unknown-linux-gnu"],
        &["cranelift", "--target=x86_64-unknown-linux-gnu"],
        &["cranelift", "--manifest-path", "other/Cargo.toml"],
        &["cranelift", "--manifest-path=other/Cargo.toml"],
        &["cranelift", "--config", "build.rustflags=[]"],
        &["cranelift", "--config=build.rustflags=[]"],
    ];
    for arguments in refusal_cases {
        let _ = fs::remove_file(directory.path("cargo.log"));
        let output = run_helper(&root, &root, &directory, arguments);
        assert_eq!(output.status.code(), Some(64), "accepted {arguments:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("debug/native-only"),
            "refusal for {arguments:?} did not explain the boundary"
        );
        assert!(
            !directory.path("cargo.log").exists(),
            "refused invocation reached Cargo: {arguments:?}"
        );
    }

    let output = run_helper(&root, &directory.0, &directory, &["cranelift"]);
    assert_eq!(output.status.code(), Some(64));
    assert!(String::from_utf8_lossy(&output.stderr).contains("workspace root"));
}

//! Exact repo-local lint coverage over both Cargo workspaces.

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const TOOL: &str = "tools/repo-lint";
const ROOT_MEMBER_COUNT: usize = 35;
const COMPONENT_MEMBER_COUNT: usize = 7;
const LEG_LABELS: [&str; 7] = [
    "connection HTTP per-invocation client",
    "root rustfmt",
    "components rustfmt",
    "root Clippy",
    "components native Clippy",
    "connection-http-standard native Clippy",
    "components wasm Clippy",
];

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

const FAKE_CARGO: &str = r#"#!/usr/bin/env bash
set -euo pipefail
count_file="$WAMN_FAKE_CARGO_LOG.count"
count=0
if [[ -r "$count_file" ]]; then
  read -r count <"$count_file"
fi
count=$((count + 1))
printf '%s\n' "$count" >"$count_file"
{
  printf 'CALL\0'
  printf '%s\0' "$PWD" "$@"
} >>"$WAMN_FAKE_CARGO_LOG"
printf '%s\n' "${RUSTFLAGS-}" >>"$WAMN_FAKE_CARGO_LOG.rustflags"
if [[ "${WAMN_FAKE_CARGO_FAIL_AT:-0}" == "$count" ]]; then
  exit 23
fi
"#;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("wamn repo lint {} {serial}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).expect("create repo-lint test directory");
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
    fs::write(path, source).expect("write fake Cargo executable");
    let mut permissions = fs::metadata(path)
        .expect("read fake Cargo permissions")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make fake Cargo executable");
}

fn run_tool(root: &Path, directory: &TestDirectory, arguments: &[&str]) -> Output {
    tool_command(root, directory)
        .args(arguments)
        .output()
        .expect("run repo-lint tool")
}

fn tool_command(root: &Path, directory: &TestDirectory) -> Command {
    let mut command = Command::new(root.join(TOOL));
    command
        .current_dir(&directory.0)
        .env("CARGO", directory.path("fake cargo"))
        .env("WAMN_FAKE_CARGO_LOG", directory.path("cargo calls"));
    command
}

fn captured_invocations(path: &Path) -> Vec<Vec<String>> {
    let bytes = fs::read(path).expect("read fake Cargo invocation log");
    let fields = bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| String::from_utf8(field.to_vec()).expect("captured argv must be UTF-8"))
        .collect::<Vec<_>>();

    let mut calls = Vec::new();
    for fields in fields.split(|field| field == "CALL") {
        if !fields.is_empty() {
            calls.push(fields.to_vec());
        }
    }
    calls
}

fn leg_statuses(output: &Output) -> Vec<String> {
    std::str::from_utf8(&output.stderr)
        .expect("repo-lint stderr must be UTF-8")
        .lines()
        .filter(|line| {
            line.starts_with("repo-lint: PASS: ") || line.starts_with("repo-lint: FAIL: ")
        })
        .map(str::to_owned)
        .collect()
}

fn assert_leg_statuses(output: &Output, failure: Option<(&str, i32)>) {
    let expected = LEG_LABELS.map(|label| match failure {
        Some((failed, exit_code)) if label == failed => {
            format!("repo-lint: FAIL: {label} (exit {exit_code})")
        }
        _ => format!("repo-lint: PASS: {label}"),
    });
    assert_eq!(leg_statuses(output), expected);
}

fn workspace_member_count(root: &Path, manifest: &Path) -> usize {
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args([
            "metadata",
            "--manifest-path",
            manifest
                .to_str()
                .expect("workspace manifest path must be UTF-8"),
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .output()
        .expect("run Cargo metadata");
    assert!(
        output.status.success(),
        "Cargo metadata failed for {}:\n{}",
        manifest.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).expect("parse Cargo metadata");
    metadata["workspace_members"]
        .as_array()
        .expect("workspace_members must be an array")
        .len()
}

#[test]
fn repo_lint_uses_cargo_owned_workspace_selection_from_any_directory() {
    let root = repository_root();
    let root_manifest = root.join("Cargo.toml");
    let component_manifest = root.join("components/Cargo.toml");
    assert_eq!(
        workspace_member_count(&root, &root_manifest),
        ROOT_MEMBER_COUNT
    );
    assert_eq!(
        workspace_member_count(&root, &component_manifest),
        COMPONENT_MEMBER_COUNT
    );

    let tool = root.join(TOOL);
    let metadata = fs::metadata(&tool).expect("read repo-lint metadata");
    assert_ne!(metadata.permissions().mode() & 0o111, 0);
    let source = fs::read_to_string(&tool).expect("read repo-lint source");
    assert!(
        !source.contains("eval"),
        "repo-lint must execute argv directly"
    );

    let directory = TestDirectory::new();
    executable(&directory.path("fake cargo"), FAKE_CARGO);
    let output = run_tool(&root, &directory, &[]);
    assert!(
        output.status.success(),
        "repo-lint failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let root_path = root.display().to_string();
    let root_manifest_path = root_manifest.display().to_string();
    let component_manifest_path = component_manifest.display().to_string();
    assert_eq!(
        captured_invocations(&directory.path("cargo calls")),
        vec![
            vec![
                root_path.clone(),
                "fmt".into(),
                "--manifest-path".into(),
                root_manifest_path.clone(),
                "--all".into(),
                "--".into(),
                "--check".into(),
            ],
            vec![
                root_path.clone(),
                "fmt".into(),
                "--manifest-path".into(),
                component_manifest_path.clone(),
                "--all".into(),
                "--".into(),
                "--check".into(),
            ],
            vec![
                root_path.clone(),
                "clippy".into(),
                "--manifest-path".into(),
                root_manifest_path,
                "--locked".into(),
                "--workspace".into(),
                "--all-targets".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ],
            vec![
                root_path.clone(),
                "clippy".into(),
                "--manifest-path".into(),
                component_manifest_path.clone(),
                "--locked".into(),
                "--workspace".into(),
                "--exclude".into(),
                "flowrunner".into(),
                "--exclude".into(),
                "connection-http-standard".into(),
                "--all-targets".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ],
            vec![
                root_path.clone(),
                "clippy".into(),
                "--manifest-path".into(),
                component_manifest_path.clone(),
                "--locked".into(),
                "--package".into(),
                "connection-http-standard".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ],
            vec![
                root_path.clone(),
                "clippy".into(),
                "--manifest-path".into(),
                component_manifest_path.clone(),
                "--locked".into(),
                "--package".into(),
                "flowrunner".into(),
                "--all-targets".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
                "-A".into(),
                "dead-code".into(),
            ],
            vec![
                root_path.clone(),
                "clippy".into(),
                "--manifest-path".into(),
                component_manifest_path.clone(),
                "--locked".into(),
                "--workspace".into(),
                "--exclude".into(),
                "flowrunner".into(),
                "--target".into(),
                "wasm32-wasip2".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ],
            vec![
                root_path,
                "clippy".into(),
                "--manifest-path".into(),
                component_manifest_path,
                "--locked".into(),
                "--package".into(),
                "flowrunner".into(),
                "--target".into(),
                "wasm32-wasip2".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
                "-A".into(),
                "dead-code".into(),
            ],
        ]
    );
    assert_eq!(
        fs::read_to_string(directory.path("cargo calls.rustflags"))
            .expect("read fake Cargo RUSTFLAGS log")
            .lines()
            .collect::<Vec<_>>(),
        ["", "", "", "", "-C panic=abort", "", "", ""]
    );
    assert_leg_statuses(&output, None);
}

#[test]
fn repo_lint_reports_every_leg_when_an_early_leg_fails() {
    let root = repository_root();
    let directory = TestDirectory::new();
    executable(&directory.path("fake cargo"), FAKE_CARGO);

    let output = tool_command(&root, &directory)
        .env("WAMN_FAKE_CARGO_FAIL_AT", "1")
        .arg("run")
        .output()
        .expect("run failing repo-lint tool");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        captured_invocations(&directory.path("cargo calls")).len(),
        8,
        "an early failure must not hide a later Cargo leg"
    );
    assert_leg_statuses(&output, Some(("root rustfmt", 23)));
}

#[test]
fn repo_lint_reports_every_cargo_leg_when_the_static_leg_fails() {
    let root = repository_root();
    let directory = TestDirectory::new();
    executable(&directory.path("fake cargo"), FAKE_CARGO);
    executable(&directory.path("grep"), "#!/usr/bin/env bash\nexit 1\n");

    let path = format!(
        "{}:{}",
        directory.0.display(),
        std::env::var("PATH").expect("test process must have PATH")
    );
    let output = tool_command(&root, &directory)
        .env("PATH", path)
        .arg("run")
        .output()
        .expect("run failing repo-lint tool");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        captured_invocations(&directory.path("cargo calls")).len(),
        8,
        "a static-leg failure must not hide any Cargo leg"
    );
    assert_leg_statuses(&output, Some(("connection HTTP per-invocation client", 65)));
}

#[test]
fn repo_lint_returns_failure_when_the_last_leg_fails() {
    let root = repository_root();
    let directory = TestDirectory::new();
    executable(&directory.path("fake cargo"), FAKE_CARGO);

    let output = tool_command(&root, &directory)
        .env("WAMN_FAKE_CARGO_FAIL_AT", "8")
        .arg("run")
        .output()
        .expect("run failing repo-lint tool");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        captured_invocations(&directory.path("cargo calls")).len(),
        8,
        "the final Cargo leg must run"
    );
    assert_leg_statuses(&output, Some(("flowrunner wasm Clippy", 23)));
}

#[test]
fn dry_run_is_side_effect_free_and_invalid_commands_are_refused() {
    let root = repository_root();
    let directory = TestDirectory::new();
    executable(&directory.path("fake cargo"), FAKE_CARGO);

    let output = run_tool(&root, &directory, &["dry-run"]);
    assert!(output.status.success());
    assert!(!directory.path("cargo calls").exists());
    let plan = String::from_utf8(output.stdout).expect("dry-run output must be UTF-8");
    assert!(plan.contains("connection-http-native-clippy: RUSTFLAGS=-C\\ panic=abort"));
    for label in [
        "root-rustfmt:",
        "components-rustfmt:",
        "root-clippy:",
        "components-native-clippy:",
        "connection-http-native-clippy:",
        "flowrunner-native-clippy:",
        "components-wasm-clippy:",
        "flowrunner-wasm-clippy:",
    ] {
        assert!(plan.contains(label), "dry-run omitted {label}");
    }

    let output = run_tool(&root, &directory, &["unknown"]);
    assert_eq!(output.status.code(), Some(64));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown command"));
    assert!(!directory.path("cargo calls").exists());
}

//! Exact orchestration proof for repo-local contract drift check 15.
//!
//! WHAT GREEN HERE MEANS (wamn-0h0g.15.138). This file drives `tools/contract-
//! diff` against a FAKE CARGO that only records its argv, so a pass proves the
//! PLAN SHAPE — that the tool invokes exactly these legs, in this order, with
//! `--locked --offline`, from any working directory, and stops at the first
//! failure. It proves NOTHING about whether the guards those legs name are
//! green, and it cannot: no real Cargo runs here.
//!
//! Guard health is proved by the leg targets themselves, each of which is an
//! ordinary test target in the workspace sweep:
//!   * `-p wamn-authoring-model --test contract`
//!   * `-p wamn-runtime --test flow_http_routing_wit_coherence`
//!   * `-p flow-http --test adversarial` (the components workspace)
//! Read a green here as "the orchestration is intact", never as "the contracts
//! have not drifted".

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const TOOL: &str = "tools/contract-diff";
// authoring, runtime-vendored-flow-http-routing, flow-http. The flow-schema
// leg went with wamn-0h0g.26.5: it regenerated
// docs/archive/contracts/flow-schema.schema.json, and both the generator and
// the committed file are gone.
const LEG_COUNT: usize = 3;

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
if [[ "${WAMN_FAKE_CARGO_FAIL_AT:-0}" == "$count" ]]; then
  exit 23
fi
"#;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "wamn contract diff {} {serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).expect("create contract-diff test directory");
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

fn tool_command(root: &Path, directory: &TestDirectory) -> Command {
    let mut command = Command::new(root.join(TOOL));
    command
        .current_dir(&directory.0)
        .env("CARGO", directory.path("fake cargo"))
        .env("WAMN_FAKE_CARGO_LOG", directory.path("cargo calls"));
    command
}

fn run_tool(root: &Path, directory: &TestDirectory, arguments: &[&str]) -> Output {
    tool_command(root, directory)
        .args(arguments)
        .output()
        .expect("run contract-diff tool")
}

fn captured_invocations(path: &Path) -> Vec<Vec<String>> {
    let bytes = fs::read(path).expect("read fake Cargo invocation log");
    let fields = bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| String::from_utf8(field.to_vec()).expect("captured argv must be UTF-8"))
        .collect::<Vec<_>>();

    fields
        .split(|field| field == "CALL")
        .filter(|fields| !fields.is_empty())
        .map(<[String]>::to_vec)
        .collect()
}

fn expected_invocations(root: &Path) -> Vec<Vec<String>> {
    let root = root.display().to_string();
    let root_manifest = format!("{root}/Cargo.toml");
    let component_manifest = format!("{root}/components/Cargo.toml");
    vec![
        vec![
            root.clone(),
            "test".into(),
            "--manifest-path".into(),
            root_manifest.clone(),
            "--locked".into(),
            "--offline".into(),
            "-p".into(),
            "wamn-authoring-model".into(),
            "--test".into(),
            "contract".into(),
        ],
        vec![
            root.clone(),
            "test".into(),
            "--manifest-path".into(),
            root_manifest.clone(),
            "--locked".into(),
            "--offline".into(),
            "-p".into(),
            "wamn-runtime".into(),
            "--test".into(),
            "flow_http_routing_wit_coherence".into(),
        ],
        vec![
            root,
            "test".into(),
            "--manifest-path".into(),
            component_manifest,
            "--locked".into(),
            "--offline".into(),
            "-p".into(),
            "http-route".into(),
            "--test".into(),
            "adversarial".into(),
        ],
    ]
}

#[test]
fn contract_diff_runs_the_exact_owner_proofs_from_any_directory() {
    let root = repository_root();
    let tool = root.join(TOOL);
    let metadata = fs::metadata(&tool).expect("read contract-diff metadata");
    assert_ne!(metadata.permissions().mode() & 0o111, 0);
    assert!(
        !fs::read_to_string(&tool)
            .expect("read contract-diff source")
            .contains("eval"),
        "contract-diff must execute argv directly"
    );

    let directory = TestDirectory::new();
    executable(&directory.path("fake cargo"), FAKE_CARGO);
    let output = run_tool(&root, &directory, &[]);
    assert!(
        output.status.success(),
        "contract-diff failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        captured_invocations(&directory.path("cargo calls")),
        expected_invocations(&root)
    );
}

#[test]
fn contract_diff_dry_run_prints_the_complete_plan_without_cargo() {
    let root = repository_root();
    let directory = TestDirectory::new();
    let missing_cargo = directory.path("cargo must not execute");
    let output = tool_command(&root, &directory)
        .env("CARGO", &missing_cargo)
        .arg("dry-run")
        .output()
        .expect("run contract-diff dry-run");
    assert!(
        output.status.success(),
        "dry-run failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!directory.path("cargo calls").exists());

    let stdout = String::from_utf8(output.stdout).expect("dry-run output must be UTF-8");
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), LEG_COUNT + 1, "{stdout}");
    assert_eq!(lines[0], format!("working-directory: {}", root.display()));
    for (line, label) in lines[1..].iter().zip([
        "authoring",
        "runtime-vendored-flow-http-routing",
        "http-route",
    ]) {
        assert!(line.starts_with(&format!("{label}: ")), "{line}");
        assert!(line.contains(" --locked --offline "), "{line}");
    }
}

#[test]
fn contract_diff_stops_at_each_first_failed_leg() {
    let root = repository_root();
    for failed_leg in 1..=LEG_COUNT {
        let directory = TestDirectory::new();
        executable(&directory.path("fake cargo"), FAKE_CARGO);
        let output = tool_command(&root, &directory)
            .env("WAMN_FAKE_CARGO_FAIL_AT", failed_leg.to_string())
            .arg("run")
            .output()
            .expect("run failing contract-diff tool");
        assert_eq!(output.status.code(), Some(23), "failed leg {failed_leg}");
        assert_eq!(
            captured_invocations(&directory.path("cargo calls")).len(),
            failed_leg,
            "contract-diff continued after failed leg {failed_leg}"
        );
    }
}

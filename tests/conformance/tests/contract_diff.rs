//! Exact orchestration test for repo-local contract drift check 15.
//!
//! WHAT GREEN HERE MEANS (wamn-0h0g.15.138). This file drives `tools/contract-
//! diff` against a FAKE CARGO that records argv and emits fixture results, so a pass shows the
//! PLAN SHAPE — that the tool invokes exactly these legs, in this order, with
//! `--locked --offline`, from any working directory, and stops at the first
//! failure. It shows NOTHING about whether the guards those legs name are
//! green, and it cannot: no real Cargo runs here.
//!
//! Guard health is tested by running the leg targets for real, which is what
//! `tools/contract-diff run` does and what this file never does:
//!
//!   * `-p wamn-authoring-model --test contract`
//!   * `-p http-route --test adversarial` (the components workspace)
//!
//! Read a green here as "the orchestration is intact", never as "the contracts
//! have not drifted".
//! A separate case uses a small libtest executable to exercise result refusal.
//!
//! The first leg is a root-workspace default member, so `cargo test
//! --workspace` also runs it. The second is NOT a root workspace member at
//! all — `http-route` lives in `apps/platform/`, and no root sweep reaches it.
//! `tools/contract-diff run` is therefore the only runner of record for leg 2,
//! and `docs/operations/running-tests.md#the-full-sweep` records its separate
//! command. Do not read this file's green as covering it (wamn-0h0g.15.138).
//!
//! The second leg was written here as `-p flow-http` until wamn-0h0g.15.138. No
//! package by that name exists in either workspace, so anyone following this
//! list hit the package-name trap: a bad `-p` ERRORS and greps as zero
//! failures. Keep these two names in step with `tools/contract-diff` itself.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const TOOL: &str = "tools/contract-diff";
// authoring and flow-http. The flow-schema
// leg went with wamn-0h0g.26.5: it regenerated
// docs/archive/contracts/flow-schema.schema.json, and both the generator and
// the committed file are gone.
const LEG_COUNT: usize = 2;

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
printf 'test result: ok. %s passed; 0 failed; 0 ignored; 0 measured;\n' "${WAMN_FAKE_PASSED:-1}"
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
    let component_manifest = format!("{root}/apps/Cargo.toml");
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
fn contract_diff_runs_the_exact_owner_tests_from_any_directory() {
    let root = repository_root();
    let tool = root.join(TOOL);
    let metadata = fs::metadata(&tool).expect("read contract-diff metadata");
    assert_ne!(metadata.permissions().mode() & 0o111, 0);
    // wamn-0h0g.15.139: `eval ` with the trailing space, not bare `eval`. See the
    // same needle in `repo_lint.rs` for the reasoning — bare, it is a prefix of
    // `evaluate`/`evaluation` and forbids the word rather than the bash builtin.
    assert!(
        !fs::read_to_string(&tool)
            .expect("read contract-diff source")
            .contains("eval "),
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
    for (line, label) in lines[1..].iter().zip(["authoring", "http-route"]) {
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

#[test]
fn contract_diff_refuses_zero_executed_cases() {
    let root = repository_root();
    let directory = TestDirectory::new();
    executable(&directory.path("fake cargo"), FAKE_CARGO);
    let output = tool_command(&root, &directory)
        .env("WAMN_FAKE_PASSED", "0")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no executed passing cases"));
    assert_eq!(
        captured_invocations(&directory.path("cargo calls")).len(),
        1
    );
}

#[test]
fn required_test_result_checks_real_libtest_results() {
    let root = repository_root();
    let directory = TestDirectory::new();
    let source = directory.path("cases.rs");
    let binary = directory.path("cases");
    fs::write(
        &source,
        r#"
        #[test] fn passes() {}
        #[test] #[ignore] fn ignored() {}
        #[test] fn fails() { panic!("deliberate failure"); }
    "#,
    )
    .unwrap();
    let build = Command::new("rustc")
        .args(["--test", "--crate-name", "required_cases"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    for (case, expected) in [
        ("passes", 0),
        ("missing", 1),
        ("ignored", 1),
        ("fails", 101),
    ] {
        let output = Command::new(root.join("tools/require-test-result"))
            .arg(&binary)
            .args([case, "--exact"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{case}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    let output = Command::new(root.join("tools/require-test-result"))
        .arg(directory.path("missing-binary"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(127));
}

#[test]
fn owned_delivery_refuses_missing_named_cases() {
    let root = repository_root();
    let directory = TestDirectory::new();
    executable(
        &directory.path("cargo"),
        "#!/bin/sh\nprintf 'test result: ok. 0 passed; 0 failed; 0 ignored;\\n'\n",
    );
    let path = std::env::join_paths(
        std::iter::once(directory.0.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    for action in ["receiving", "wms"] {
        let output = Command::new(root.join("tools/delivery-owned"))
            .arg(action)
            .env("PATH", &path)
            .env_remove("WAMN_DELIVERY_CANDIDATE")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{action}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("no executed passing cases"));
    }
}

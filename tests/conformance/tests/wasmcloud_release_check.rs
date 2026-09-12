//! Tests upstream release identity, gate execution, and isolated Git settings.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const REVISION: &str = "68ebece9c537f8bb4b5c9999f274ec68d60f35a9";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

#[derive(Debug)]
struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn executable(path: &Path, source: &str) {
    fs::write(path, source).expect("write executable fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make fixture executable");
}

fn fixture() -> Fixture {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock must follow Unix epoch")
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("wamn-release-check-{}-{nonce}", std::process::id()));
    fs::create_dir_all(root.join("bin")).expect("create fixture directory");
    fs::create_dir(root.join("upstream with spaces")).expect("create checkout fixture");
    fs::write(root.join("upstream with spaces/Cargo.toml"), "").expect("write manifest fixture");
    executable(
        &root.join("bin/git"),
        r#"#!/usr/bin/env bash
set -euo pipefail
[[ "$GIT_CONFIG_GLOBAL" == /dev/null && "$GIT_CONFIG_NOSYSTEM" == 1 ]]
shift 2
case "$*" in
  'rev-parse --is-inside-work-tree') echo true ;;
  'remote get-url origin') echo "${WAMN_TEST_ORIGIN:-https://github.com/wasmCloud/wasmCloud}" ;;
  'rev-parse v2.9.0^{commit}') echo "${WAMN_TEST_TAG:-$WAMN_TEST_REVISION}" ;;
  'rev-parse HEAD') echo "${WAMN_TEST_HEAD:-$WAMN_TEST_REVISION}" ;;
  'status --porcelain --untracked-files=all')
    printf '%s' "${WAMN_TEST_DIRTY:-}"
    [[ ! -f "$WAMN_TEST_ROOT/modified" ]] || echo ' M Cargo.toml'
    [[ ! -f "$WAMN_TEST_ROOT/modified-fixture-lock" ]] || echo ' M crates/wash-runtime/tests/fixtures/Cargo.lock'
    ;;
  *) echo "unexpected Git command: $*" >&2; exit 99 ;;
esac
"#,
    );
    executable(
        &root.join("bin/cargo"),
        r#"#!/usr/bin/env bash
set -euo pipefail
[[ "$GIT_CONFIG_GLOBAL" == /dev/null && "$GIT_CONFIG_NOSYSTEM" == 1 ]]
printf '%s\n' "$*" >>"$WAMN_TEST_ROOT/cargo.log"
[[ "${WAMN_TEST_MUTATE:-}" != yes ]] || touch "$WAMN_TEST_ROOT/modified"
if [[ "$*" == *'--package xtask -- build-fixtures' ]]; then
  [[ "${WAMN_TEST_MUTATE_FIXTURE_LOCK:-}" != yes ]] || touch "$WAMN_TEST_ROOT/modified-fixture-lock"
  exit "${WAMN_TEST_FIXTURE_EXIT:-${WAMN_TEST_CARGO_EXIT:-0}}"
fi
exit "${WAMN_TEST_CARGO_EXIT:-0}"
"#,
    );
    Fixture { root }
}

fn command(fixture: &Fixture) -> Command {
    let mut command = Command::new(repository_root().join("tools/wasmcloud-release-check"));
    let paths = std::iter::once(fixture.root.join("bin"))
        .chain(std::env::split_paths(
            &std::env::var_os("PATH").expect("PATH is set"),
        ))
        .collect::<Vec<_>>();
    command
        .arg("run")
        .arg(fixture.root.join("upstream with spaces"))
        .env(
            "PATH",
            std::env::join_paths(paths).expect("join fixture PATH"),
        )
        .env("CARGO", fixture.root.join("bin/cargo"))
        .env("WAMN_TEST_ROOT", &fixture.root)
        .env("WAMN_TEST_REVISION", REVISION)
        .env("GIT_CONFIG_GLOBAL", "/fixture/must-not-be-read")
        .env("GIT_CONFIG_NOSYSTEM", "0");
    command
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "release gate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn dry_run_names_the_upstream_release_and_every_gate_leg() {
    let output = Command::new(repository_root().join("tools/wasmcloud-release-check"))
        .args(["dry-run", "/tmp/upstream checkout with spaces"])
        .output()
        .expect("run release gate dry-run");
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).expect("dry-run output must be UTF-8");
    for required in [
        "expected-repository: https://github.com/wasmCloud/wasmCloud",
        "expected-tag: v2.9.0",
        "expected-revision: 68ebece9c537f8bb4b5c9999f274ec68d60f35a9",
        "git-config-global: /dev/null",
        "git-config-nosystem: 1",
        "format:",
        "wash-runtime-fixtures:",
        "--locked --package xtask -- build-fixtures",
        "wash-runtime-features:",
        "wash-runtime:",
        "git-template-features:",
        "git-template-fixture:",
        "--features default",
        "--include-ignored --nocapture",
        "clone_template_",
    ] {
        assert!(stdout.contains(required), "missing {required:?}:\n{stdout}");
    }
    assert!(!stdout.contains("expected-branch:"));
    assert!(!stdout.contains("expected-patch-count:"));
}

#[test]
fn clean_upstream_runs_all_legs_without_a_fork_branch() {
    let fixture = fixture();
    let output = command(&fixture).output().expect("run release gate");
    assert_success(&output);
    let log = fs::read_to_string(fixture.root.join("cargo.log")).expect("read Cargo calls");
    let commands = log.lines().collect::<Vec<_>>();
    assert_eq!(commands.len(), 6, "{log}");
    assert!(log.contains("fmt --manifest-path"));
    assert!(log.contains("--all -- --check"));
    assert!(commands[1].starts_with("run --manifest-path"), "{log}");
    assert!(
        commands[1].ends_with("--locked --package xtask -- build-fixtures"),
        "{log}"
    );
    assert!(
        commands[3].contains("-p wash-runtime --features default --no-fail-fast"),
        "{log}"
    );
    assert!(log.contains("-p wash --features default --lib clone_template_"));
    assert!(log.contains("--include-ignored --nocapture"));
}

#[test]
fn changed_release_identity_or_source_refuses_before_cargo() {
    for (key, value) in [
        ("WAMN_TEST_ORIGIN", "https://github.com/example/wasmCloud"),
        ("WAMN_TEST_TAG", "wrong-tag-commit"),
        ("WAMN_TEST_HEAD", "extra-commit"),
        ("WAMN_TEST_DIRTY", " M Cargo.toml"),
        ("WAMN_TEST_DIRTY", "?? override.rs"),
    ] {
        let fixture = fixture();
        let output = command(&fixture)
            .env(key, value)
            .output()
            .expect("run negative release gate");
        assert_eq!(output.status.code(), Some(65), "{key}: {output:?}");
        assert!(!fixture.root.join("cargo.log").exists(), "{key}");
    }
}

#[test]
fn failed_gate_legs_keep_their_exit_codes_and_do_not_hide_later_legs() {
    let fixture = fixture();
    let output = command(&fixture)
        .env("WAMN_TEST_CARGO_EXIT", "23")
        .output()
        .expect("run failed gate fixtures");
    assert_eq!(output.status.code(), Some(23));
    let stderr = String::from_utf8(output.stderr).expect("gate stderr must be UTF-8");
    assert!(
        stderr.contains("wash-runtime-fixtures exit-code=23"),
        "{stderr}"
    );
    assert!(stderr.contains("wash-runtime exit-code=23"), "{stderr}");
    assert!(
        stderr.contains("git-template-fixture exit-code=23"),
        "{stderr}"
    );
    let log = fs::read_to_string(fixture.root.join("cargo.log")).expect("read Cargo calls");
    assert_eq!(log.lines().count(), 6, "{log}");
}

#[test]
fn fixture_build_failure_keeps_its_exit_code_while_later_legs_run() {
    let fixture = fixture();
    let output = command(&fixture)
        .env("WAMN_TEST_FIXTURE_EXIT", "37")
        .output()
        .expect("run failed fixture preparation");
    assert_eq!(output.status.code(), Some(37));
    let stderr = String::from_utf8(output.stderr).expect("gate stderr must be UTF-8");
    for status in [
        "wash-runtime-fixtures exit-code=37",
        "wash-runtime exit-code=0",
        "git-template-fixture exit-code=0",
    ] {
        assert!(stderr.contains(status), "missing {status:?}: {stderr}");
    }
    let log = fs::read_to_string(fixture.root.join("cargo.log")).expect("read Cargo calls");
    assert_eq!(log.lines().count(), 6, "{log}");
}

#[test]
fn fixture_build_cannot_change_its_tracked_lockfile_and_continue() {
    let fixture = fixture();
    let output = command(&fixture)
        .env("WAMN_TEST_MUTATE_FIXTURE_LOCK", "yes")
        .output()
        .expect("run fixture preparation that changes its lockfile");
    assert_eq!(output.status.code(), Some(65));
    let log = fs::read_to_string(fixture.root.join("cargo.log")).expect("read Cargo calls");
    assert_eq!(log.lines().count(), 2, "{log}");
    assert!(log.contains("--package xtask -- build-fixtures"), "{log}");
    assert!(!log.contains("test --manifest-path"), "{log}");
}

#[test]
fn a_gate_leg_cannot_change_upstream_source_and_continue() {
    let fixture = fixture();
    let output = command(&fixture)
        .env("WAMN_TEST_MUTATE", "yes")
        .output()
        .expect("run mutating gate fixture");
    assert_eq!(output.status.code(), Some(65));
    let log = fs::read_to_string(fixture.root.join("cargo.log")).expect("read Cargo calls");
    assert_eq!(log.lines().count(), 1, "{log}");
}

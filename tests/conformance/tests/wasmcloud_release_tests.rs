//! Tests upstream release identity, acceptance test execution, and isolated Git settings.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const REVISION: &str = "8b0e24cb6610642783e3f16f5f1eac45ff2cb5aa";

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
        std::env::temp_dir().join(format!("wamn-release-tests-{}-{nonce}", std::process::id()));
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
  'rev-parse v2.12.1^{commit}') echo "${WAMN_TEST_TAG:-$WAMN_TEST_REVISION}" ;;
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
if [[ $1 == test ]]; then
  printf 'test result: ok. %s passed; 0 failed; 0 ignored; 0 measured;\n' "${WAMN_FAKE_PASSED:-1}"
fi
exit "${WAMN_TEST_CARGO_EXIT:-0}"
"#,
    );
    Fixture { root }
}

fn command(fixture: &Fixture) -> Command {
    let mut command = Command::new(repository_root().join("tools/wasmcloud-release-tests"));
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
            .expect("run negative release acceptance test");
        assert_eq!(output.status.code(), Some(65), "{key}: {output:?}");
    }
}

#[test]
fn failed_acceptance_test_legs_keep_their_exit_codes_and_do_not_hide_later_legs() {
    let fixture = fixture();
    let output = command(&fixture)
        .env("WAMN_TEST_CARGO_EXIT", "23")
        .output()
        .expect("run failed acceptance test fixtures");
    assert_eq!(output.status.code(), Some(23));
}

#[test]
fn fixture_build_failure_keeps_its_exit_code_while_later_legs_run() {
    let fixture = fixture();
    let output = command(&fixture)
        .env("WAMN_TEST_FIXTURE_EXIT", "37")
        .output()
        .expect("run failed fixture preparation");
    assert_eq!(output.status.code(), Some(37));
}

#[test]
fn fixture_build_cannot_change_its_tracked_lockfile_and_continue() {
    let fixture = fixture();
    let output = command(&fixture)
        .env("WAMN_TEST_MUTATE_FIXTURE_LOCK", "yes")
        .output()
        .expect("run fixture preparation that changes its lockfile");
    assert_eq!(output.status.code(), Some(65));
}

#[test]
fn an_acceptance_test_leg_cannot_change_upstream_source_and_continue() {
    let fixture = fixture();
    let output = command(&fixture)
        .env("WAMN_TEST_MUTATE", "yes")
        .output()
        .expect("run mutating acceptance test fixture");
    assert_eq!(output.status.code(), Some(65));
}

#[test]
fn upstream_tests_refuse_zero_executed_cases() {
    let fixture = fixture();
    let output = command(&fixture)
        .env("WAMN_FAKE_PASSED", "0")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
}

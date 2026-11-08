//! Repo-local lint runner contracts, exercised with fake executables.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const TOOL: &str = "tools/repo-lint";
const COMPONENT_MANIFEST: &str = "apps/Cargo.toml";
const NO_STD_MANIFEST: &str = "apps/platform/no-std/Cargo.toml";

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

const FAKE_CARGO: &str = r#"#!/usr/bin/env bash
set -euo pipefail
{
  printf 'CALL\0%s\0%s\0' "$PWD" "${RUSTFLAGS-}"
  printf '%s\0' "$@"
} >>"$WAMN_FAKE_CARGO_LOG"
for argument in "$@"; do
  if [[ "$argument" == "${WAMN_FAKE_CARGO_FAIL_AT-}" ]]; then
    exit 23
  fi
done
if [[ "${1-}" == tree ]]; then
  printf '%s\n' "$3"
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
        fs::create_dir(path.join("tools")).expect("create fixture tools directory");
        fs::create_dir(path.join("caller")).expect("create caller working directory");
        for tool in [TOOL, "tools/repo-lint-dependencies"] {
            fs::copy(repository_root().join(tool), path.join(tool)).expect("copy lint tool");
        }
        executable(&path.join("fake cargo"), FAKE_CARGO);
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

fn run_tool(directory: &TestDirectory, arguments: &[&str]) -> Output {
    tool_command(directory)
        .args(arguments)
        .output()
        .expect("run repo-lint tool")
}

fn tool_command(directory: &TestDirectory) -> Command {
    let mut command = Command::new(directory.path(TOOL));
    command
        .current_dir(directory.path("caller"))
        .env("CARGO", directory.path("fake cargo"))
        .env("WAMN_FAKE_CARGO_LOG", directory.path("cargo calls"))
        .env_remove("WAMN_FAKE_CARGO_FAIL_AT")
        .env_remove("RUSTFLAGS");
    command
}

#[derive(Debug)]
struct Invocation {
    directory: PathBuf,
    rustflags: String,
    arguments: Vec<String>,
}

fn captured_invocations(path: &Path) -> Vec<Invocation> {
    let bytes = fs::read(path).expect("read fake Cargo invocation log");
    let fields = bytes
        .strip_suffix(&[0])
        .expect("invocation log ends with a NUL")
        .split(|byte| *byte == 0)
        .collect::<Vec<_>>();
    fields
        .split(|field| *field == b"CALL")
        .filter(|fields| !fields.is_empty())
        .map(|fields| {
            let mut fields = fields.iter().map(|field| {
                String::from_utf8(field.to_vec()).expect("captured argv must be UTF-8")
            });
            Invocation {
                directory: PathBuf::from(fields.next().expect("capture working directory")),
                rustflags: fields.next().expect("capture RUSTFLAGS"),
                arguments: fields.collect(),
            }
        })
        .collect()
}

fn has_pair(arguments: &[String], flag: &str, value: &str) -> bool {
    arguments
        .windows(2)
        .any(|pair| pair[0] == flag && pair[1] == value)
}

#[test]
fn repo_lint_uses_its_repository_directory_and_preserves_arguments_with_spaces() {
    let directory = TestDirectory::new();
    let output = run_tool(&directory, &[]);
    assert!(
        output.status.success(),
        "repo-lint failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let invocations = captured_invocations(&directory.path("cargo calls"));
    assert!(!invocations.is_empty(), "the runner must invoke Cargo");
    for invocation in &invocations {
        assert_eq!(invocation.directory, directory.0);
        if invocation
            .arguments
            .iter()
            .any(|arg| arg == "--manifest-path")
        {
            assert!(
                invocation.arguments.windows(2).any(|pair| {
                    pair[0] == "--manifest-path"
                        && Path::new(&pair[1]).starts_with(&directory.0)
                        && pair[1].ends_with("Cargo.toml")
                }),
                "the manifest path with spaces must stay one argument: {invocation:?}"
            );
        }
    }
    let policy = invocations
        .iter()
        .find(|call| has_pair(&call.arguments, "--bin", "repo-policy"))
        .expect("repository policy must run");
    assert!(has_pair(
        &policy.arguments,
        "--",
        directory.0.to_str().expect("fixture path must be UTF-8")
    ));
}

#[test]
fn repo_lint_selects_native_and_component_targets_without_unifying_no_std_features() {
    let directory = TestDirectory::new();
    let output = run_tool(&directory, &["run"]);
    assert!(output.status.success());
    let invocations = captured_invocations(&directory.path("cargo calls"));
    for manifest in ["Cargo.toml", COMPONENT_MANIFEST, NO_STD_MANIFEST] {
        let path = directory.path(manifest);
        let path = path.to_str().expect("fixture path must be UTF-8");
        let clippy = invocations
            .iter()
            .filter(|call| call.arguments[0] == "clippy")
            .filter(|call| has_pair(&call.arguments, "--manifest-path", path))
            .collect::<Vec<_>>();
        let native = clippy
            .iter()
            .find(|call| {
                call.arguments.iter().any(|arg| arg == "--workspace")
                    && !call.arguments.iter().any(|arg| arg == "--target")
            })
            .expect("each workspace needs native Clippy");
        assert!(has_pair(&native.arguments, "-D", "warnings"));
        if manifest == NO_STD_MANIFEST {
            assert_eq!(native.rustflags, "-C panic=abort");
            assert!(!native.arguments.iter().any(|arg| arg == "--all-targets"));
        } else {
            assert!(native.arguments.iter().any(|arg| arg == "--all-targets"));
        }
        if manifest == "Cargo.toml" {
            continue;
        }
        let wasm = clippy
            .iter()
            .find(|call| {
                call.arguments.iter().any(|arg| arg == "--workspace")
                    && has_pair(&call.arguments, "--target", "wasm32-wasip2")
            })
            .expect("component workspaces need wasm Clippy");
        assert!(wasm.rustflags.is_empty());
        if manifest == NO_STD_MANIFEST {
            assert!(!wasm.arguments.iter().any(|arg| arg == "--all-targets"));
        }
        if manifest == COMPONENT_MANIFEST {
            for call in [native, wasm] {
                assert!(has_pair(
                    &call.arguments,
                    "--exclude",
                    "connection-http-standard"
                ));
            }
            let isolated = clippy
                .iter()
                .find(|call| {
                    has_pair(&call.arguments, "--package", "connection-http-standard")
                        && !call.arguments.iter().any(|arg| arg == "--target")
                })
                .expect("the no_std HTTP component needs separate native Clippy");
            assert_eq!(isolated.rustflags, "-C panic=abort");
            assert!(!isolated.arguments.iter().any(|arg| arg == "--workspace"));
            assert!(!isolated.arguments.iter().any(|arg| arg == "--all-targets"));
            assert!(clippy.iter().any(|call| {
                has_pair(&call.arguments, "--package", "connection-http-standard")
                    && has_pair(&call.arguments, "--target", "wasm32-wasip2")
                    && !call.arguments.iter().any(|arg| arg == "--workspace")
            }));
        }
    }
}

#[test]
fn repo_lint_propagates_failures_and_reports_them_without_hiding_later_steps() {
    // Select failures by argument, so unrelated additions and reordering are harmless.
    for failed_argument in ["fmt", "repo-policy"] {
        let directory = TestDirectory::new();
        let output = tool_command(&directory)
            .env("WAMN_FAKE_CARGO_FAIL_AT", failed_argument)
            .arg("run")
            .output()
            .expect("run failing repo-lint tool");
        assert!(
            !output.status.success(),
            "a failed invocation must fail the tool"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        let failure = stderr.find("repo-lint: FAIL:").expect("report the failure");
        assert!(stderr[failure..].contains("exit 23"));
        if failed_argument == "fmt" {
            assert!(stderr[failure..].contains("repo-lint: PASS:"));
            assert!(
                captured_invocations(&directory.path("cargo calls"))
                    .iter()
                    .any(|call| has_pair(&call.arguments, "--bin", "repo-policy")),
                "report repository policy even after formatting fails"
            );
        }
    }
}

#[test]
fn dry_run_is_side_effect_free_and_invalid_commands_are_refused() {
    let directory = TestDirectory::new();
    let before = fs::read_dir(&directory.0)
        .expect("read fixture directory")
        .count();
    let output = run_tool(&directory, &["dry-run"]);
    assert!(output.status.success());
    assert!(
        !output.stdout.is_empty(),
        "dry-run must describe planned work"
    );
    assert!(!directory.path("cargo calls").exists());
    assert_eq!(
        fs::read_dir(&directory.0)
            .expect("read fixture directory after dry-run")
            .count(),
        before,
        "dry-run must not create files"
    );

    let output = run_tool(&directory, &["unknown"]);
    assert_eq!(output.status.code(), Some(64));
    assert!(!directory.path("cargo calls").exists());
}

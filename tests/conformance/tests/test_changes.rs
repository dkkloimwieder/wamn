//! Selection test for `tools/test-changes`.
//!
//! The tool runs in a small Git fixture with three workspaces. A fake Cargo
//! returns fixture metadata and records each test command. A pass shows which
//! Cargo test commands the tool selects for a change set. It shows nothing about
//! whether those tests pass.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

const TOOL: &str = "tools/test-changes";

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

const FAKE_CARGO: &str = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == metadata ]]; then
  relative="${3#"$WAMN_FAKE_ROOT"/}"
  cat "$WAMN_FAKE_METADATA/${relative//\//_}.json"
  exit 0
fi
count_file="$WAMN_FAKE_CARGO_LOG.count"
count=0
if [[ -r "$count_file" ]]; then
  read -r count <"$count_file"
fi
count=$((count + 1))
printf '%s\n' "$count" >"$count_file"
{
  printf 'CALL\0'
  printf '%s\0' "$@"
} >>"$WAMN_FAKE_CARGO_LOG"
if [[ "${WAMN_FAKE_CARGO_FAIL_AT:-0}" == "$count" ]]; then
  exit 23
fi
printf 'test result: ok. %s passed; 0 failed; 0 ignored; 0 measured;\n' "${WAMN_FAKE_PASSED:-1}"
"#;

/// A Git fixture with root, apps, and no-std workspaces, committed on `main`.
struct Fixture {
    directory: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("wamn test changes {} {serial}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let root = directory.join("repository");
        fs::create_dir_all(root.join("tools")).expect("create the fixture repository");
        let directory = directory
            .canonicalize()
            .expect("resolve the fixture directory");
        let root = root.canonicalize().expect("resolve the fixture repository");
        let fixture = Self { directory, root };

        let tool = fs::read_to_string(repository_root().join(TOOL)).expect("read the tool");
        executable(&fixture.root.join(TOOL), &tool);
        let required = "tools/require-test-result";
        executable(
            &fixture.root.join(required),
            &fs::read_to_string(repository_root().join(required)).unwrap(),
        );
        executable(&fixture.directory.join("fake cargo"), FAKE_CARGO);
        fs::create_dir(fixture.directory.join("metadata")).unwrap();
        for file in [
            "Cargo.toml",
            "Cargo.lock",
            "apps/Cargo.toml",
            "apps/Cargo.lock",
            "apps/platform/no-std/Cargo.toml",
            "apps/platform/no-std/Cargo.lock",
            "crates/core/src/lib.rs",
            "crates/user/src/lib.rs",
            "crates/checker/src/lib.rs",
            "crates/far/src/lib.rs",
            "apps/demo/tests/src/lib.rs",
            "apps/demo/data/src/lib.rs",
            "apps/demo/component/src/lib.rs",
            "apps/platform/wire/src/lib.rs",
            "apps/platform/no-std/guest/src/lib.rs",
            "deploy/sql/schema.sql",
            "docs/readme.md",
        ] {
            fixture.write(file, "fixture\n");
        }
        fixture.write_metadata();
        fixture.git(&["init", "--quiet", "--initial-branch=main"]);
        fixture.git(&["add", "."]);
        fixture.commit("fixture");
        fixture
    }

    fn write(&self, file: &str, contents: &str) {
        let path = self.root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).expect("write a fixture file");
    }

    fn git(&self, arguments: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=fixture"])
            .args(["-c", "user.email=fixture@example.invalid"])
            .args(arguments)
            .current_dir(&self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .expect("run git in the fixture");
        assert!(status.success(), "git {arguments:?} failed");
    }

    fn commit(&self, message: &str) {
        self.git(&["commit", "--quiet", "--all", "--message", message]);
    }

    fn package(&self, directory: &str, name: &str, required_features: &[&str]) -> Value {
        let manifest = self.root.join(directory).join("Cargo.toml");
        let mut targets = vec![json!({"name": name, "kind": ["lib"]})];
        if !required_features.is_empty() {
            targets.push(json!({
                "name": format!("{name}_live"),
                "kind": ["test"],
                "required-features": required_features,
            }));
        }
        json!({
            "name": name,
            "id": format!("path+file://{}#{name}@0.1.0", self.root.join(directory).display()),
            "manifest_path": manifest,
            "targets": targets,
        })
    }

    fn write_metadata(&self) {
        let id = |package: &Value| package["id"].clone();
        let node = |package: &Value, dependencies: &[(&Value, Option<&str>)]| {
            json!({
                "id": id(package),
                "deps": dependencies
                    .iter()
                    .map(|(dependency, kind)| json!({
                        "pkg": id(dependency),
                        "dep_kinds": [{"kind": kind}],
                    }))
                    .collect::<Vec<_>>(),
            })
        };
        let core = self.package("crates/core", "core", &[]);
        let user = self.package("crates/user", "user", &["ops"]);
        let checker = self.package("crates/checker", "checker", &[]);
        let far = self.package("crates/far", "far", &[]);
        let mut demo_tests = self.package("apps/demo/tests", "demo-tests", &[]);
        demo_tests["features"] = json!({"cluster": []});
        let wire = self.package("apps/platform/wire", "wire", &[]);
        let demo_data = self.package("apps/demo/data", "demo-data", &[]);
        let component = self.package("apps/demo/component", "demo-component", &[]);
        let guest = self.package("apps/platform/no-std/guest", "guest", &[]);

        let root_members = [&core, &user, &checker, &far, &demo_tests];
        self.metadata(
            "Cargo.toml",
            "",
            &[&core, &user, &checker, &far, &demo_tests, &wire],
            &root_members,
            &[
                node(&core, &[]),
                node(&user, &[(&core, None), (&wire, Some("build"))]),
                node(&checker, &[(&user, Some("dev"))]),
                node(&far, &[(&checker, None)]),
                node(&demo_tests, &[]),
                node(&wire, &[]),
            ],
        );
        self.metadata(
            "apps/Cargo.toml",
            "/apps",
            &[&wire, &demo_data, &component],
            &[&wire, &demo_data, &component],
            &[
                node(&wire, &[]),
                node(&demo_data, &[(&wire, None)]),
                node(&component, &[(&demo_data, None)]),
            ],
        );
        self.metadata(
            "apps/platform/no-std/Cargo.toml",
            "/apps/platform/no-std",
            &[&guest],
            &[&guest],
            &[node(&guest, &[])],
        );
    }

    fn metadata(
        &self,
        manifest: &str,
        workspace: &str,
        packages: &[&Value],
        members: &[&Value],
        nodes: &[Value],
    ) {
        let document = json!({
            "packages": packages,
            "workspace_members": members.iter().map(|package| package["id"].clone()).collect::<Vec<_>>(),
            "workspace_root": format!("{}{workspace}", self.root.display()),
            "resolve": {"nodes": nodes},
        });
        fs::write(
            self.directory
                .join("metadata")
                .join(format!("{}.json", manifest.replace('/', "_"))),
            document.to_string(),
        )
        .expect("write fixture metadata");
    }

    fn tool_command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(self.root.join(TOOL));
        command
            .args(arguments)
            .current_dir(&self.directory)
            .env("CARGO", self.directory.join("fake cargo"))
            .env("WAMN_FAKE_ROOT", &self.root)
            .env("WAMN_FAKE_METADATA", self.directory.join("metadata"))
            .env("WAMN_FAKE_CARGO_LOG", self.directory.join("cargo calls"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        command
    }

    fn tool(&self, arguments: &[&str]) -> Output {
        self.tool_command(arguments)
            .output()
            .expect("run test-changes")
    }

    /// Run the tool and return each recorded Cargo test command without `test`.
    fn selected(&self, arguments: &[&str]) -> Vec<Vec<String>> {
        let _ = fs::remove_file(self.directory.join("cargo calls"));
        let _ = fs::remove_file(self.directory.join("cargo calls.count"));
        let output = self.tool(arguments);
        assert!(
            output.status.success(),
            "test-changes failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.calls()
    }

    fn calls(&self) -> Vec<Vec<String>> {
        let Ok(bytes) = fs::read(self.directory.join("cargo calls")) else {
            return Vec::new();
        };
        let fields = bytes
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty())
            .map(|field| String::from_utf8(field.to_vec()).unwrap())
            .collect::<Vec<_>>();
        fields
            .split(|field| field == "CALL")
            .filter(|call| !call.is_empty())
            .map(|call| {
                assert_eq!(call[0], "test");
                call[1..].to_vec()
            })
            .collect()
    }

    fn command(&self, manifest: &str, selection: &[&str]) -> Vec<String> {
        let mut command = vec![
            "--manifest-path".to_owned(),
            self.root.join(manifest).display().to_string(),
            "--locked".to_owned(),
            "--offline".to_owned(),
            "--no-fail-fast".to_owned(),
        ];
        command.extend(selection.iter().map(|argument| (*argument).to_owned()));
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
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
    fs::write(path, source).expect("write an executable");
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn documentation_and_beads_changes_select_nothing() {
    let fixture = Fixture::new();
    fixture.write("docs/readme.md", "changed\n");
    fixture.write(".beads/issues.jsonl", "{}\n");
    let output = fixture.tool(&["dry-run"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().last(), Some("selected: nothing"), "{stdout}");
    assert!(!fixture.directory.join("cargo calls").exists());
}

#[test]
fn a_root_markdown_change_selects_nothing() {
    let fixture = Fixture::new();
    fixture.write("README.md", "changed\n");
    let output = fixture.tool(&["dry-run"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().last(), Some("selected: nothing"), "{stdout}");
    assert!(!fixture.directory.join("cargo calls").exists());
}

#[test]
fn a_crate_change_selects_its_dependents_with_required_features() {
    let fixture = Fixture::new();
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let expected = vec![fixture.command(
        "Cargo.toml",
        &[
            "-p",
            "checker",
            "-p",
            "core",
            "-p",
            "user",
            "--features",
            "user/ops",
        ],
    )];
    assert_eq!(fixture.selected(&[]), expected);

    // A committed change on a branch counts against the merge base with main.
    fixture.git(&["checkout", "--quiet", "-b", "work"]);
    fixture.commit("change core");
    assert_eq!(fixture.selected(&["run"]), expected);
    assert_eq!(
        fixture.selected(&["--base", "HEAD", "run"]),
        Vec::<Vec<String>>::new()
    );
}

#[test]
fn an_application_edit_selects_its_packages_in_both_workspaces() {
    let fixture = Fixture::new();
    fixture.write("apps/demo/command/record.sql", "SELECT 1;\n");
    assert_eq!(
        fixture.selected(&[]),
        vec![
            fixture.command("Cargo.toml", &["-p", "demo-tests"]),
            fixture.command(
                "apps/Cargo.toml",
                &["-p", "demo-component", "-p", "demo-data"]
            ),
        ]
    );
}

#[test]
fn a_platform_crate_selects_its_dependents_in_every_workspace() {
    let fixture = Fixture::new();
    fixture.write("apps/platform/wire/src/lib.rs", "changed\n");
    assert_eq!(
        fixture.selected(&[]),
        vec![
            fixture.command(
                "Cargo.toml",
                &["-p", "checker", "-p", "user", "--features", "user/ops"]
            ),
            fixture.command(
                "apps/Cargo.toml",
                &["-p", "demo-component", "-p", "demo-data", "-p", "wire"]
            ),
        ]
    );
}

#[test]
fn shared_wit_changes_select_the_workspaces_with_direct_readers() {
    for (path, no_std) in [
        ("crates/execution/workflow/router/wit/package.wit", true),
        (
            "crates/platform/runtime/wit/deps/wamn-postgres/package.wit",
            true,
        ),
        (
            "crates/execution/host/wit/deps/wamn-router-delivery/package.wit",
            false,
        ),
        (
            "crates/execution/host/wit/deps/wamn-router-delivery-0.2/package.wit",
            false,
        ),
        (
            "apps/platform/execution/materializer/wit/deps/wasi-cli/package.wit",
            false,
        ),
        (
            "apps/platform/execution/materializer/wit/deps/wasi-clocks/package.wit",
            false,
        ),
    ] {
        let fixture = Fixture::new();
        fixture.write(path, "changed shared interface\n");
        let mut expected = vec![
            fixture.command("Cargo.toml", &["--workspace", "--features", "wamn-ctl/ops"]),
            fixture.command("apps/Cargo.toml", &["--workspace"]),
        ];
        if no_std {
            expected.push(fixture.command("apps/platform/no-std/Cargo.toml", &["--workspace"]));
        }
        assert_eq!(fixture.selected(&[]), expected, "{path}");
    }
}

#[test]
fn workspace_files_and_unowned_inputs_run_the_full_command() {
    let root_full = ["--workspace", "--features", "wamn-ctl/ops"];
    let fixture = Fixture::new();
    fixture.write("Cargo.lock", "changed\n");
    fixture.write("apps/platform/no-std/Cargo.lock", "changed\n");
    assert_eq!(
        fixture.selected(&[]),
        vec![
            fixture.command("Cargo.toml", &root_full),
            fixture.command("apps/platform/no-std/Cargo.toml", &["--workspace"]),
        ]
    );

    let fixture = Fixture::new();
    fs::remove_file(fixture.root.join("deploy/sql/schema.sql")).unwrap();
    fixture.write("apps/Cargo.lock", "changed\n");
    assert_eq!(
        fixture.selected(&[]),
        vec![
            fixture.command("Cargo.toml", &root_full),
            fixture.command("apps/Cargo.toml", &["--workspace"]),
        ]
    );
}

#[test]
fn run_attempts_every_command_and_fails_when_one_fails() {
    let fixture = Fixture::new();
    fixture.write("apps/demo/command/record.sql", "SELECT 1;\n");
    let output = fixture
        .tool_command(&[])
        .env("WAMN_FAKE_CARGO_FAIL_AT", "1")
        .output()
        .expect("run test-changes");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(fixture.calls().len(), 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("FAIL: root (exit 23)"), "{stderr}");
    assert!(stderr.contains("PASS: apps"), "{stderr}");
}

#[test]
fn dry_run_prints_the_commands_without_running_tests() {
    let fixture = Fixture::new();
    fixture.write("Cargo.lock", "changed\n");
    let output = fixture.tool(&["dry-run"]);
    assert!(output.status.success());
    assert!(!fixture.directory.join("cargo calls").exists());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3, "{stdout}");
    assert!(lines[0].starts_with("working-directory: "), "{stdout}");
    assert!(lines[1].starts_with("base: "), "{stdout}");
    assert!(
        lines[2].starts_with("root (full: \"Cargo.lock\"): ")
            && lines[2].ends_with(" --no-fail-fast --workspace --features wamn-ctl/ops"),
        "{stdout}"
    );
}

#[test]
fn cluster_runs_the_root_ignored_tests_one_at_a_time() {
    let fixture = Fixture::new();
    fixture.write("apps/demo/command/record.sql", "SELECT 1;\n");
    for (arguments, ending) in [
        (
            &["--cluster", "dry-run"][..],
            " --no-fail-fast -p demo-tests --features demo-tests/cluster -- --ignored --test-threads=1",
        ),
        (
            &["--cluster", "--name", "journey", "dry-run"][..],
            " --no-fail-fast -p demo-tests --features demo-tests/cluster journey -- --ignored --test-threads=1",
        ),
    ] {
        let output = fixture.tool(arguments);
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3, "{stdout}");
        assert!(
            lines[2].starts_with("root: ") && lines[2].ends_with(ending),
            "{stdout}"
        );
    }
    // A full root run keeps the cluster feature beside the root features.
    fixture.write("Cargo.lock", "changed\n");
    let output = fixture.tool(&["--cluster", "dry-run"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.lines().nth(2).is_some_and(|line| line.ends_with(
            " --workspace --features wamn-ctl/ops\\,demo-tests/cluster -- --ignored --test-threads=1"
        )),
        "{stdout}"
    );
    assert!(!fixture.directory.join("cargo calls").exists());
}

#[test]
fn a_name_filter_needs_cluster_and_a_matching_ignored_test() {
    let fixture = Fixture::new();
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let output = fixture.tool(&["--name", "journey", "dry-run"]);
    assert_eq!(output.status.code(), Some(64));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--name requires --cluster"), "{stderr}");

    // The fake Cargo lists no test, so no ignored test matches and none runs.
    let output = fixture.tool(&["--cluster", "--name", "journey", "run"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("matches the name filter 'journey'"),
        "{stderr}"
    );
    assert_eq!(
        fixture.calls(),
        vec![fixture.command(
            "Cargo.toml",
            &[
                "-p",
                "checker",
                "-p",
                "core",
                "-p",
                "user",
                "--features",
                "user/ops",
                "journey",
                "--",
                "--ignored",
                "--test-threads=1",
                "--list",
                "--format",
                "terse",
            ],
        )]
    );
}

#[test]
fn a_name_filter_fails_when_the_change_set_selects_no_package() {
    let fixture = Fixture::new();
    // An unchanged tree selects nothing. Under --cluster, a no-std change selects
    // no root package.
    for (change, command) in [
        (None, "dry-run"),
        (Some("apps/platform/no-std/Cargo.lock"), "run"),
    ] {
        if let Some(file) = change {
            fixture.write(file, "changed\n");
        }
        let output = fixture.tool(&["--cluster", "--name", "journey", command]);
        assert_eq!(output.status.code(), Some(1));
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.lines().last(), Some("selected: nothing"), "{stdout}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(
                "no package is selected, so no ignored test matches the name filter 'journey'"
            ),
            "{stderr}"
        );
    }
    assert!(!fixture.directory.join("cargo calls").exists());
}

#[test]
fn selected_workspace_refuses_zero_executed_cases() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("crates/core/src/lib.rs"), "// changed\n").unwrap();
    let output = fixture
        .tool_command(&["run"])
        .env("WAMN_FAKE_PASSED", "0")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no executed passing cases"));
}

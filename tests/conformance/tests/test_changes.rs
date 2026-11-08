//! Selection test for `tools/test-changes`.
//!
//! The tool runs in a small Git fixture with three workspaces. A fake Cargo
//! returns fixture metadata and records each test command. A pass shows which
//! Cargo test commands the tool selects for a change set. It shows nothing about
//! whether those tests pass.

use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const TOOL: &str = "tools/test-changes";

const EXACT_SUITE: &str = r#"
id = "fixture-exact"
tier = "contract"
workspace = "root"
package = "core"
target = ["lib:core"]
tests = ["fixture::case"]
features = []
prerequisites = []
inputs = ["^crates/core/"]
execution = "libtest"
mode = "local"
resource_lock = ""
timeout = 1800
"#;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

const FAKE_CARGO: &str = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == metadata ]]; then
  relative="${3#"$WAMN_FAKE_ROOT"/}"
  cat "$WAMN_FAKE_METADATA/${relative//\//_}.json"
  exit 0
fi
if [[ -n "${WAMN_FAKE_EXPECT_INPUT:-}" ]]; then
  [[ "${WAMN_FIXTURE_COMPONENT:-}" == "$WAMN_FAKE_EXPECT_INPUT" ]] || exit 32
fi
if [[ -n "${WAMN_FAKE_EXPECT_SCREEN:-}" ]]; then
  [[ "${WAMN_SYNC_BENCH_DIST:-}" == "$WAMN_FAKE_EXPECT_SCREEN" ]] || exit 33
fi
if [[ " $* " == *' --list '* && " $* " == *' --exact '* ]]; then
  [[ "${WAMN_FAKE_MISSING_TEST:-false}" != true ]] || exit 0
  for argument in "$@"; do
    [[ "$argument" != fixture::* ]] || printf '%s: test\n' "$argument"
  done
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
passed="${WAMN_FAKE_PASSED:-1}"
if [[ " $* " == *' --exact '* ]]; then passed="${WAMN_FAKE_EXACT_PASSED:-$passed}"; fi
if ((passed > 0)) && [[ " $* " == *' --exact '* ]]; then
  name=''
  for argument in "$@"; do
    [[ "$argument" != -- ]] || break
    name="$argument"
  done
  printf 'test %s ... ok\n' "${WAMN_FAKE_EXECUTED_TEST:-$name}"
fi
if [[ " $* " == *' --message-format=json-render-diagnostics '* && -n "${WAMN_FAKE_ROOT_OUTPUT:-}" ]]; then
  cat "$WAMN_FAKE_ROOT_OUTPUT"
fi
if ((passed > 0)) && [[ " $* " != *' --exact '* ]]; then printf 'test fixture::case ... ok\n'; fi
printf 'test result: ok. %s passed; 0 failed; 0 ignored; 0 measured;\n' "$passed"
if [[ "${WAMN_FAKE_EMPTY_TARGET:-false}" == true ]]; then
  printf 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured;\n'
fi
"#;

/// Stands in for `docker`: records its name and arguments, and fails when
/// `WAMN_FAKE_GUEST_FAIL` names it.
const FAKE_GUEST_BUILD: &str = r#"#!/usr/bin/env bash
set -euo pipefail
name="$(basename -- "$0")"
{
  printf 'CALL\0%s\0' "$name"
  printf '%s\0' "$@"
} >>"$WAMN_FAKE_GUEST_LOG"
if [[ "${WAMN_FAKE_GUEST_FAIL:-}" == "$name" ]]; then
  exit 29
fi
"#;

/// The fixture Dockerfile: its guest stage builds `demo-component`.
const DOCKERFILE: &str = "FROM toolchain AS component-toolchain\n\
RUN cargo build -p not-a-guest\n\
FROM component-toolchain AS component-builder\n\
RUN cargo build --release --target wasm32-wasip2 \\\n      -p demo-component\n\
FROM debian AS host\n\
RUN cargo build -p host-only\n";

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
        let policy = fs::read_to_string(repository_root().join("tests/selection.toml")).unwrap();
        let mut sections = policy.split("[[suites]]");
        let prefix = sections.next().unwrap();
        let start = prefix.find("targets = [").unwrap();
        let end = start + prefix[start..].find("\n]\n").unwrap() + 3;
        let mut policy = format!("{}targets = []\n{}", &prefix[..start], &prefix[end..]);
        for section in sections.filter(|section| section.contains("package = \"\"")) {
            policy.push_str("[[suites]]");
            policy.push_str(section);
        }
        fixture.write("tests/selection.toml", &policy);
        executable(
            &fixture.root.join("tools/repo-lint"),
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> \"$WAMN_FAKE_QUALITY_LOG\"\n",
        );
        executable(
            &fixture.root.join("tools/check-packages-alone"),
            "#!/usr/bin/env bash\nexit 0\n",
        );
        executable(&fixture.directory.join("fake cargo"), FAKE_CARGO);
        executable(&fixture.directory.join("docker"), FAKE_GUEST_BUILD);
        executable(
            &fixture.directory.join("pnpm"),
            "#!/usr/bin/env bash\nexit 0\n",
        );
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
        fixture.write("Dockerfile", DOCKERFILE);
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

    fn kind_targets(&self) {
        let path = self.root.join("tests/selection.toml");
        let policy = fs::read_to_string(&path).unwrap();
        let policy = policy
            .lines()
            .map(|line| {
                if line.contains("workspace=\"root\"") {
                    line.replace("tier=\"unit\"", "tier=\"kind\"")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(path, policy).unwrap();
        self.commit("fixture tiers");
    }

    fn add_suite(&self, suite: &str) {
        let path = self.root.join("tests/selection.toml");
        let policy = fs::read_to_string(&path).unwrap();
        fs::write(path, format!("{policy}\n[[suites]]\n{suite}\n")).unwrap();
        self.commit("fixture selection");
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
        let mut targets = vec![json!({"name": name, "kind": ["lib"], "test": true})];
        if !required_features.is_empty() {
            targets.push(json!({
                "name": format!("{name}_live"),
                "kind": ["test"],
                "test": true,
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
        let mut conformance = self.package("tests/conformance", "wamn-conformance-tests", &[]);
        conformance["targets"] = json!([
            {"name": "test_changes", "kind": ["test"], "test": true},
            {"name": "test_tiers", "kind": ["test"], "test": true},
        ]);
        let mut demo_tests = self.package("apps/demo/tests", "demo-tests", &[]);
        demo_tests["features"] = json!({"cluster": []});
        let wire = self.package("apps/platform/wire", "wire", &[]);
        let demo_data = self.package("apps/demo/data", "demo-data", &[]);
        let component = self.package("apps/demo/component", "demo-component", &[]);
        let guest = self.package("apps/platform/no-std/guest", "guest", &[]);

        let root_members = [&core, &user, &checker, &far, &demo_tests, &conformance];
        self.metadata(
            "Cargo.toml",
            "",
            &[
                &core,
                &user,
                &checker,
                &far,
                &demo_tests,
                &conformance,
                &wire,
            ],
            &root_members,
            &[
                node(&core, &[]),
                node(&user, &[(&core, None), (&wire, Some("build"))]),
                node(&checker, &[(&user, Some("dev"))]),
                node(&far, &[(&checker, None)]),
                node(&demo_tests, &[]),
                node(&conformance, &[]),
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
        let workspace_name = match workspace {
            "" => "root",
            "/apps" => "apps",
            "/apps/platform/no-std" => "no-std",
            _ => unreachable!(),
        };
        let mut rows = String::new();
        for package in members {
            for target in package["targets"].as_array().unwrap() {
                writeln!(
                    rows,
                    "{{workspace={workspace_name:?}, package={}, target=\"{}:{}\", tier=\"unit\"}},",
                    package["name"],
                    target["kind"][0].as_str().unwrap(),
                    target["name"].as_str().unwrap(),
                ).unwrap();
            }
        }
        let path = self.root.join("tests/selection.toml");
        let policy = fs::read_to_string(&path).unwrap().replacen(
            "targets = [",
            &format!("targets = [\n{rows}"),
            1,
        );
        fs::write(path, policy).unwrap();
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
            .env(
                "WAMN_FAKE_QUALITY_LOG",
                self.directory.join("quality calls"),
            )
            .env("DOCKER", self.directory.join("docker"))
            .env("WAMN_FAKE_GUEST_LOG", self.directory.join("guest calls"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.directory.display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        command
    }

    fn tool(&self, arguments: &[&str]) -> Output {
        self.tool_command(arguments)
            .output()
            .expect("run test-changes")
    }

    /// Run the tool and return the Cargo arguments or built executable commands.
    fn selected(&self, arguments: &[&str]) -> Vec<Vec<String>> {
        let _ = fs::remove_file(self.directory.join("cargo calls"));
        let _ = fs::remove_file(self.directory.join("cargo calls.count"));
        let _ = fs::remove_file(self.directory.join("guest calls"));
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
                assert!(["test", "artifact"].contains(&call[0].as_str()));
                call[1..].to_vec()
            })
            .collect()
    }

    /// Each recorded guest build: the program name, then its arguments.
    fn guest_calls(&self) -> Vec<Vec<String>> {
        let Ok(bytes) = fs::read(self.directory.join("guest calls")) else {
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
            .map(<[String]>::to_vec)
            .collect()
    }

    /// The guest build that a change set selecting a guest runs.
    fn guest_builds(&self) -> Vec<Vec<String>> {
        // The tool keys the build by the first 16 hex digits of the SHA-256
        // of the checkout's real path.
        let real_root = fs::canonicalize(&self.root).unwrap();
        let key = hex::encode(Sha256::digest(real_root.display().to_string().as_bytes()));
        vec![vec![
            "docker".to_owned(),
            "build".to_owned(),
            "--target".to_owned(),
            "component-builder".to_owned(),
            "--build-arg".to_owned(),
            format!("WAMN_CHECKOUT_KEY={}", &key[..16]),
            self.root.display().to_string(),
        ]]
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
        if manifest == "Cargo.toml" && !selection.contains(&"--ignored") {
            command.push("--message-format=json-render-diagnostics".to_owned());
        }
        command
    }

    fn selector_tests(&self) -> Vec<String> {
        self.command(
            "Cargo.toml",
            &[
                "-p",
                "wamn-conformance-tests",
                "--test",
                "test_changes",
                "--test",
                "test_tiers",
            ],
        )
    }

    fn ignored_target(
        &self,
        package: &str,
        kind: &str,
        target: &str,
        cases: &[(&str, &str)],
    ) -> String {
        let built_executable = self.directory.join(format!("{target}-fixture"));
        executable(
            &built_executable,
            r#"#!/usr/bin/env bash
set -euo pipefail
{
  printf 'CALL\0artifact\0%s\0' "$0"
  printf '%s\0' "$@"
} >> "$WAMN_FAKE_CARGO_LOG"
printf 'test %s ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n' "$1"
"#,
        );
        let artifact = json!({
            "reason": "compiler-artifact",
            "manifest_path": self.root.join(format!("crates/{package}/Cargo.toml")),
            "executable": built_executable,
            "target": {"kind": [kind], "name": target},
        });
        let executable = built_executable.display();
        let mut output = format!(
            "{artifact}\n     Running tests/{target}.rs (kache test-runner \"{executable}\")\n"
        );
        for (name, reason) in cases {
            writeln!(output, "test {name} ... ignored, {reason}").unwrap();
        }
        output
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

fn assert_nothing_selected(fixture: &Fixture) {
    let output = fixture.tool(&["dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3, "{stdout}");
    assert!(lines[0].starts_with("working-directory: "), "{stdout}");
    assert!(lines[1].starts_with("base: "), "{stdout}");
    assert_eq!(lines[2], "selected: nothing");
    assert!(!fixture.directory.join("cargo calls").exists());
    assert!(!fixture.directory.join("quality calls").exists());
    let output = fixture.tool(&["run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.directory.join("quality calls").exists());
    assert!(!fixture.directory.join("cargo calls").exists());
}

#[test]
fn documentation_and_beads_changes_select_nothing() {
    let fixture = Fixture::new();
    fixture.write("docs/readme.md", "changed\n");
    fixture.write(".beads/issues.jsonl", "{}\n");
    assert_nothing_selected(&fixture);
}

#[test]
fn a_root_markdown_change_selects_nothing() {
    let fixture = Fixture::new();
    fixture.write("README.md", "changed\n");
    assert_nothing_selected(&fixture);
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
        ("wit/wamn-node/package.wit", true),
        ("wit/wamn-connection/package.wit", true),
        ("wit/wamn-flow-http-routing/package.wit", true),
        ("wit/wamn-integration/package.wit", true),
        ("wit/wamn-jetstream/package.wit", true),
        ("wit/wamn-operation-0.1/package.wit", true),
        ("wit/wasmcloud-blobstore/package.wit", true),
        ("wit/wamn-postgres-0.4/package.wit", true),
        ("wit/wamn-router-delivery-0.4/package.wit", false),
        ("wit/wamn-application-dispatch/package.wit", false),
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
            fixture.command(
                "Cargo.toml",
                &[
                    "--workspace",
                    "--features",
                    "wamn-environment-control/ops,wamn-ctl/ops",
                ],
            ),
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
    let root_full = [
        "--workspace",
        "--features",
        "wamn-environment-control/ops,wamn-ctl/ops",
    ];
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
fn a_grant_sql_change_selects_the_family_denial_matrix() {
    let fixture = Fixture::new();
    let policy = fs::read_to_string(repository_root().join("tests/selection.toml")).unwrap();
    let row = policy
        .split("[[suites]]")
        .next()
        .unwrap()
        .lines()
        .find(|line| line.contains("target=\"test:family_denial_matrix\""))
        .expect("family_denial_matrix must have a real targets row");
    assert!(row.contains("tier=\"live-postgres\""), "{row}");

    let mut provision = fixture.package("crates/control/provision", "wamn-control-provision", &[]);
    provision["targets"] = json!([
        {"name": "family_denial_matrix", "kind": ["test"], "test": true},
        {"name": "other_unit", "kind": ["test"], "test": true},
    ]);
    fixture.metadata(
        "Cargo.toml",
        "",
        &[&provision],
        &[&provision],
        &[json!({"id": provision["id"], "deps": []})],
    );
    let path = "tests/selection.toml";
    let policy = fs::read_to_string(fixture.root.join(path)).unwrap().replace(
        "{workspace=\"root\", package=\"wamn-control-provision\", target=\"test:family_denial_matrix\", tier=\"unit\"},",
        row,
    );
    fixture.write(path, &policy);
    fixture.write("deploy/sql/apply-grants.sql", "SELECT 1;\n");
    fixture.git(&["add", "."]);
    fixture.commit("fixture grant SQL and target tiers");

    fixture.write("deploy/sql/apply-grants.sql", "SELECT 2;\n");
    let output = fixture.tool(&["dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.lines().any(|line| {
            line.starts_with("root (full: \"deploy/sql/apply-grants.sql\"): ")
                && line.ends_with(
                    " --no-fail-fast --workspace --features wamn-environment-control/ops\\,wamn-ctl/ops",
                )
        }),
        "{stdout}"
    );
    let output = fixture.tool(&["--tier", "live-postgres", "dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout
            .lines()
            .any(|line| line.contains("-p wamn-control-provision")
                && line.contains("--test family_denial_matrix")),
        "{stdout}"
    );
    assert!(!stdout.contains("other_unit"), "{stdout}");
}

#[test]
fn unknown_paths_select_every_applicable_suite() {
    for path in ["tools/new-input", "apps/unknown/schema.sql"] {
        let fixture = Fixture::new();
        fixture.write(path, "changed\n");
        let output = fixture.tool(&["dry-run"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.contains("suites: packages-alone web docker-guests"),
            "{stdout}"
        );
        assert!(stdout.contains("root (full:"), "{stdout}");
        assert!(stdout.contains("apps (full:"), "{stdout}");
        assert!(stdout.contains("no-std (full:"), "{stdout}");
        let output = fixture.tool(&["--tier", "kind", "dry-run"]);
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(output.status.success());
        assert!(stdout.contains("suites: \n"), "{stdout}");
        assert!(!stdout.contains("apps (full:"), "{stdout}");
    }
}

#[test]
fn suite_row_edits_select_only_the_suite_and_selector_tests() {
    for existing in [false, true] {
        let fixture = Fixture::new();
        if existing {
            fixture.add_suite(EXACT_SUITE);
        }
        let path = "tests/selection.toml";
        let policy = fs::read_to_string(fixture.root.join(path)).unwrap();
        let changed = if existing {
            policy.replace("fixture::case", "fixture::changed")
        } else {
            format!(
                "{policy}\n[[suites]]\n{}",
                EXACT_SUITE.replace("fixture::case", "fixture::changed")
            )
        };
        fixture.write(path, &changed);
        let calls = fixture.selected(&[]);
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0], fixture.selector_tests());
        assert!(
            calls[1].ends_with(&[
                "-p".into(),
                "core".into(),
                "--lib".into(),
                "fixture::changed".into(),
                "--".into(),
                "--exact".into(),
                "--test-threads=1".into(),
            ]),
            "{calls:?}"
        );
        assert!(fixture.guest_calls().is_empty());
        assert!(!fixture.directory.join("quality calls").exists());
        assert!(fixture.selected(&["--tier", "live-postgres"]).is_empty());

        fixture.write(
            path,
            &changed.replace(
                "id = \"fixture-exact\"\ntier = \"contract\"",
                "id = \"fixture-exact\"\ntier = \"kind\"",
            ),
        );
        assert_eq!(fixture.selected(&[]), vec![fixture.selector_tests()]);
        assert_eq!(
            fixture.selected(&["--tier", "kind"]),
            vec![calls[1].clone()]
        );
        assert!(fixture.guest_calls().is_empty());
        assert!(!fixture.directory.join("quality calls").exists());
    }
}

#[test]
fn row_formatting_is_ignored_and_target_or_exclusion_edits_select_selector_tests() {
    let fixture = Fixture::new();
    fixture.add_suite(EXACT_SUITE);
    let path = "tests/selection.toml";
    let policy = fs::read_to_string(fixture.root.join(path)).unwrap();
    fixture.write(
        path,
        &policy.replace(
            "id = \"fixture-exact\"",
            "id= 'fixture-exact' # row comment",
        ),
    );
    assert_nothing_selected(&fixture);

    for changed in [
        policy.replace("target=\"lib:core\", tier=\"unit\"", "target=\"lib:core\", tier=\"contract\""),
        policy.replace("exclusions = [", "exclusions = [\n{workspace='root', package='core', test='fixture::case', bead='fixture'},"),
    ] {
        fixture.write(path, &changed);
        assert_eq!(fixture.selected(&[]), vec![fixture.selector_tests()]);
        assert!(fixture.guest_calls().is_empty());
        assert!(!fixture.directory.join("quality calls").exists());
    }
}

#[test]
fn shared_header_and_selector_edits_select_everything() {
    let fixture = Fixture::new();
    let path = "tests/selection.toml";
    let policy = fs::read_to_string(fixture.root.join(path)).unwrap();
    let tool = fs::read_to_string(fixture.root.join(TOOL)).unwrap();
    for (changed_policy, changed_tool) in [
        (
            policy.replacen("[[rules]]", "[[rules]]\nextra = true", 1),
            tool.clone(),
        ),
        (format!("# shared comment\n{policy}"), tool.clone()),
        (
            policy.replace("id = \"web\"", "id = \"web\"\nlabel = 'changed'"),
            format!("{tool}\n# selector edit\n"),
        ),
    ] {
        fixture.write(path, &changed_policy);
        fixture.write(TOOL, &changed_tool);
        let output = fixture.tool(&["dry-run"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.contains("suites: packages-alone web docker-guests\n"),
            "{stdout}"
        );
        for workspace in ["root", "apps", "no-std"] {
            assert!(stdout.contains(&format!("{workspace} (full:")), "{stdout}");
        }
    }
}

#[test]
fn package_and_suite_row_edits_select_the_union() {
    let fixture = Fixture::new();
    fixture.add_suite(EXACT_SUITE);
    let path = "tests/selection.toml";
    let policy = fs::read_to_string(fixture.root.join(path)).unwrap();
    fixture.write(path, &policy.replace("fixture::case", "fixture::changed"));
    fixture.write("crates/far/src/lib.rs", "changed\n");
    let calls = fixture.selected(&[]);
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(calls[0], fixture.command("Cargo.toml", &["-p", "far"]));
    assert_eq!(calls[1], fixture.selector_tests());
    assert!(calls[2].contains(&"fixture::changed".into()), "{calls:?}");
    assert!(fixture.guest_calls().is_empty());
    assert!(!fixture.directory.join("quality calls").exists());
}

#[test]
fn removed_suite_rows_require_no_remaining_references() {
    let fixture = Fixture::new();
    let path = "tests/selection.toml";
    let policy = fs::read_to_string(fixture.root.join(path)).unwrap();
    fixture.add_suite(EXACT_SUITE);
    for file in ["docs/history/old.md", "docs/audit/report.md"] {
        fixture.write(file, "fixture-exact\n");
    }
    fixture.write("docs/readme.md", "fixture-exactSuffix\n");
    fixture.git(&["add", "."]);
    fixture.commit("fixture archived references");
    fixture.write(path, &policy);
    assert_eq!(fixture.selected(&[]), vec![fixture.selector_tests()]);
    assert!(fixture.guest_calls().is_empty());
    assert!(!fixture.directory.join("quality calls").exists());

    fixture.write("docs/readme.md", "fixture-exact\n");
    let output = fixture.tool(&["dry-run"]);
    assert_eq!(output.status.code(), Some(65));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("removed suite 'fixture-exact'"), "{stderr}");
    assert!(stderr.contains("docs/readme.md"), "{stderr}");
}

#[test]
fn deleted_paths_select_the_same_tests_as_edits() {
    for path in [
        "crates/core/src/lib.rs",
        "deploy/sql/schema.sql",
        "Dockerfile",
    ] {
        let fixture = Fixture::new();
        let contents = if path == "Dockerfile" {
            DOCKERFILE
        } else {
            "fixture\n"
        };
        fixture.write(path, &format!("{contents}\n# changed\n"));
        let edited = fixture.tool(&["dry-run"]);
        fs::remove_file(fixture.root.join(path)).unwrap();
        let deleted = fixture.tool(&["dry-run"]);
        assert!(edited.status.success(), "{path}");
        assert!(
            deleted.status.success(),
            "{}",
            String::from_utf8_lossy(&deleted.stderr)
        );
        assert_eq!(edited.stdout, deleted.stdout, "{path}");
    }
}

#[test]
fn a_dockerfile_change_builds_the_guest_stage() {
    let fixture = Fixture::new();
    fixture.write("Dockerfile", &format!("{DOCKERFILE}\n# changed\n"));
    fixture.selected(&[]);
    assert_eq!(fixture.guest_calls(), fixture.guest_builds());
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
    assert!(lines[0].starts_with("working-directory: "), "{stdout}");
    assert!(lines[1].starts_with("base: "), "{stdout}");
    assert!(
        lines[2] == "suites: packages-alone"
            && lines[3].starts_with("root (full: \"Cargo.lock\"): ")
            && lines[3].ends_with(
                " --no-fail-fast --workspace --features wamn-environment-control/ops\\,wamn-ctl/ops"
            )
            && lines[4] == "packages alone: tools/check-packages-alone",
        "{stdout}"
    );
}

#[test]
fn kind_runs_the_root_ignored_tests_one_at_a_time() {
    let fixture = Fixture::new();
    fixture.kind_targets();
    fixture.write("apps/demo/command/record.sql", "SELECT 1;\n");
    for (arguments, ending) in [
        (
            &["--tier", "kind", "dry-run"][..],
            " --no-fail-fast -p demo-tests --features demo-tests/cluster -- --ignored --test-threads=1",
        ),
        (
            &["--tier", "kind", "--name", "journey", "dry-run"][..],
            " --no-fail-fast -p demo-tests --features demo-tests/cluster journey -- --ignored --test-threads=1",
        ),
    ] {
        let output = fixture.tool(arguments);
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout.lines().collect::<Vec<_>>();
        assert!(
            lines[2] == "suites: " && lines[3].starts_with("root: ") && lines[3].ends_with(ending),
            "{stdout}"
        );
    }
    // A full root run keeps the cluster feature beside the root features.
    fixture.write("Cargo.lock", "changed\n");
    let output = fixture.tool(&["--tier", "kind", "dry-run"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.lines().nth(3).is_some_and(|line| line.ends_with(
            " --workspace --features wamn-environment-control/ops\\,wamn-ctl/ops\\,demo-tests/cluster -- --ignored --test-threads=1"
        )),
        "{stdout}"
    );
    assert!(!fixture.directory.join("cargo calls").exists());
}

#[test]
fn a_name_filter_needs_kind_and_a_matching_ignored_test() {
    let fixture = Fixture::new();
    fixture.kind_targets();
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let output = fixture.tool(&["--name", "journey", "dry-run"]);
    assert_eq!(output.status.code(), Some(64));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--name requires --tier kind"), "{stderr}");

    // The fake Cargo lists no test, so no ignored test matches and none runs.
    let output = fixture.tool(&["--tier", "kind", "--name", "journey", "run"]);
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
    fixture.kind_targets();
    // An unchanged tree selects nothing. Under --tier kind, a no-std change selects
    // no root package.
    for (change, command) in [
        (None, "dry-run"),
        (Some("apps/platform/no-std/Cargo.lock"), "run"),
    ] {
        if let Some(file) = change {
            fixture.write(file, "changed\n");
        }
        let output = fixture.tool(&["--tier", "kind", "--name", "journey", command]);
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

#[test]
fn an_exact_suite_refuses_zero_executed_cases() {
    let fixture = Fixture::new();
    fixture.add_suite(EXACT_SUITE);
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let output = fixture
        .tool_command(&["--suite", "fixture-exact", "run"])
        .env("WAMN_FAKE_EXACT_PASSED", "0")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no executed passing cases"));
    assert_eq!(fixture.calls().len(), 1);
}

#[test]
fn an_exact_suite_requires_its_named_case_to_pass() {
    let fixture = Fixture::new();
    fixture.add_suite(EXACT_SUITE);
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let output = fixture
        .tool_command(&["--suite", "fixture-exact", "run"])
        .env("WAMN_FAKE_EXECUTED_TEST", "fixture::different_case")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("required test did not pass: fixture::case")
    );
}

#[test]
fn a_selected_target_refuses_an_empty_result_beside_a_passing_result() {
    let fixture = Fixture::new();
    fixture.add_suite(&EXACT_SUITE.replace("tests = [\"fixture::case\"]", "tests = []"));
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let output = fixture
        .tool_command(&["--suite", "fixture-exact", "run"])
        .env("WAMN_FAKE_EMPTY_TARGET", "true")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("required test target reported no executed passing cases")
    );
}

#[test]
fn bad_selectors_fail_before_any_test_runs() {
    for (suite, missing, exact) in [
        (
            EXACT_SUITE.replace("package = \"core\"", "package = \"absent\""),
            "missing package absent",
            false,
        ),
        (
            EXACT_SUITE.replace("lib:core", "test:absent"),
            "missing target test:absent",
            false,
        ),
        (
            EXACT_SUITE.to_owned(),
            "missing exact test fixture::case",
            true,
        ),
    ] {
        let fixture = Fixture::new();
        fixture.add_suite(&suite);
        fixture.write("crates/core/src/lib.rs", "changed\n");
        let output = fixture
            .tool_command(&["run"])
            .env("WAMN_FAKE_MISSING_TEST", exact.to_string())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(65));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(missing), "{stderr}");
        assert!(fixture.calls().is_empty());
        assert!(!fixture.directory.join("quality calls").exists());
    }
}

#[test]
fn runtime_changes_select_the_native_host_reader() {
    let fixture = Fixture::new();
    let policy = fs::read_to_string(repository_root().join("tests/selection.toml")).unwrap();
    let host = policy
        .split("[[suites]]")
        .find(|section| section.contains("id = \"native-host\""))
        .unwrap();
    assert!(host.contains("crates/platform/runtime/"));
    let prerequisites = host
        .lines()
        .find(|line| line.starts_with("prerequisites = "))
        .unwrap();
    // The fixture has no runtime package, so a fixture package stands for it.
    let host = host
        .replace("crates/platform/runtime/", "crates/far/")
        .replace("package = \"wamn-dev-control\"", "package = \"user\"")
        .replace("target = [\"test:native_host\"]", "target = [\"lib:user\"]")
        .replace("features = [\"ops\"]", "features = []")
        .replace(prerequisites, "prerequisites = []")
        .replace(
            "prekind_host::prekind_http_host_runs_and_rejects_respond_without_caller",
            "fixture::case",
        );
    fixture.add_suite(&host);
    fixture.write("crates/far/src/lib.rs", "changed\n");
    let calls = fixture.selected(&[]);
    assert!(
        calls
            .iter()
            .any(|call| call.contains(&"fixture::case".to_owned()))
    );
}

#[test]
fn every_change_selects_the_install_render_checks() {
    let fixture = Fixture::new();
    let policy = fs::read_to_string(repository_root().join("tests/selection.toml")).unwrap();
    let prekind = policy
        .split("[[suites]]")
        .find(|section| section.contains("id = \"prekind\""))
        .unwrap();
    assert!(prekind.contains("always = true"));
    // The fixture has no wamn-environment-control package, so a fixture package stands for it.
    let prekind = prekind
        .replace(
            "package = \"wamn-environment-control\"",
            "package = \"user\"",
        )
        .replace(
            "target = [\"test:environment_render\", \"test:platform_render\"]",
            "target = [\"lib:user\"]",
        )
        .replace("tests = []", "tests = [\"fixture::case\"]")
        .replace("features = [\"ops\"]", "features = []")
        .replace("prerequisites = [\"helm\"]", "prerequisites = []");
    fixture.add_suite(&prekind);
    fixture.write("crates/far/src/lib.rs", "changed\n");
    let calls = fixture.selected(&[]);
    assert!(
        calls
            .iter()
            .any(|call| call.contains(&"fixture::case".to_owned()))
    );
}

#[test]
fn a_change_that_selects_an_image_guest_builds_the_guests_both_ways() {
    // A platform crate the guest links, and a shared WIT root.
    for path in ["apps/platform/wire/src/lib.rs", "wit/wamn-node/package.wit"] {
        let fixture = Fixture::new();
        fixture.write(path, "changed\n");
        fixture.selected(&[]);
        assert_eq!(fixture.guest_calls(), fixture.guest_builds(), "{path}");
    }

    // A crate no guest links builds no guest.
    let fixture = Fixture::new();
    fixture.write("crates/core/src/lib.rs", "changed\n");
    fixture.selected(&[]);
    assert_eq!(fixture.guest_calls(), Vec::<Vec<String>>::new());

    // The cluster run starts no guest build.
    let fixture = Fixture::new();
    fixture.write("apps/platform/wire/src/lib.rs", "changed\n");
    let output = fixture.tool(&["--tier", "kind", "run"]);
    assert!(output.status.success());
    assert_eq!(fixture.guest_calls(), Vec::<Vec<String>>::new());
}

#[test]
fn a_dry_run_prints_the_guest_builds_and_a_failed_build_fails_the_run() {
    let fixture = Fixture::new();
    fixture.write("apps/platform/wire/src/lib.rs", "changed\n");
    let output = fixture.tool(&["dry-run"]);
    assert!(output.status.success());
    assert!(!fixture.directory.join("guest calls").exists());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout.lines().collect::<Vec<_>>();
    assert!(
        lines[2] == "suites: docker-guests"
            && lines[5].starts_with("guests (Dockerfile component-builder): ")
            && lines[5].contains(" build --target component-builder "),
        "{stdout}"
    );

    let output = fixture
        .tool_command(&["run"])
        .env("WAMN_FAKE_GUEST_FAIL", "docker")
        .output()
        .expect("run test-changes");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(fixture.guest_calls(), fixture.guest_builds());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("FAIL: guests (Dockerfile component-builder) (exit 29)"),
        "{stderr}"
    );
}

#[test]
fn tiers_limit_targets_inside_selected_packages() {
    let fixture = Fixture::new();
    let path = fixture.root.join("tests/selection.toml");
    let policy = fs::read_to_string(&path).unwrap().replace(
        "target=\"test:user_live\", tier=\"unit\"",
        "target=\"test:user_live\", tier=\"live-postgres\"",
    );
    fs::write(path, policy).unwrap();
    fixture.commit("fixture tiers");
    fixture.write("crates/user/src/lib.rs", "changed\n");
    let output = fixture.tool(&["--tier", "unit", "dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--lib"), "{stdout}");
    assert!(!stdout.contains("--test user_live"), "{stdout}");
    let output = fixture.tool(&["--tier", "live-postgres", "dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--test user_live"), "{stdout}");
    assert!(!stdout.contains("--lib"), "{stdout}");
    assert!(!stdout.contains("-p checker"), "{stdout}");
    let output = fixture.tool(&["--tier", "unit,live-postgres", "dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let path = fixture.directory.join("metadata/Cargo.toml.json");
    let mut metadata: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let user = metadata["packages"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|package| package["name"] == "user")
        .unwrap();
    user["targets"][1]["required-features"] = json!([]);
    fs::write(path, serde_json::to_string(&metadata).unwrap()).unwrap();
    let output = fixture.tool(&["--tier", "live-postgres", "dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--test user_live"), "{stdout}");
    assert!(!stdout.contains("--features test:user_live"), "{stdout}");
}

#[test]
fn explicit_default_tiers_exclude_kind_targets() {
    let fixture = Fixture::new();
    let path = fixture.root.join("tests/selection.toml");
    let policy = fs::read_to_string(&path).unwrap().replace(
        "target=\"lib:core\", tier=\"unit\"",
        "target=\"lib:core\", tier=\"kind\"",
    );
    fs::write(path, policy).unwrap();
    fixture.commit("fixture tiers");
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let output = fixture.tool(&["--tier", "unit,contract,live-postgres,live-nats", "dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains("-p core"), "{stdout}");
    assert!(stdout.contains("-p user"), "{stdout}");
}

#[test]
fn unknown_tiers_fail_before_metadata() {
    let fixture = Fixture::new();
    let output = fixture.tool(&["--tier", "absent", "dry-run"]);
    assert_eq!(output.status.code(), Some(64));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown tier 'absent'"));
    for tiers in ["", "unit,", ",unit", "unit,,contract"] {
        let output = fixture.tool(&["--tier", tiers, "dry-run"]);
        assert_eq!(output.status.code(), Some(64), "{tiers}");
    }
}

#[test]
fn an_ignored_target_suite_runs_without_exact_names() {
    let fixture = Fixture::new();
    fixture.add_suite(
        &EXACT_SUITE.replace("tests = [\"fixture::case\"]", "tests = []\nignored = true"),
    );
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let calls = fixture.selected(&[]);
    assert!(
        calls.iter().any(|call| call.ends_with(&[
            "--".to_owned(),
            "--ignored".to_owned(),
            "--test-threads=1".to_owned(),
        ])),
        "{calls:?}"
    );
}

#[test]
fn an_explicit_ignored_suite_runs_on_an_unchanged_tree_and_refuses_zero_cases() {
    let fixture = Fixture::new();
    fixture.add_suite(
        &EXACT_SUITE.replace("tests = [\"fixture::case\"]", "tests = []\nignored = true"),
    );
    let arguments = ["--suite", "fixture-exact", "run"];
    let calls = fixture.selected(&arguments);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].contains(&"--ignored".into()));
    let output = fixture
        .tool_command(&arguments)
        .env("WAMN_FAKE_PASSED", "0")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no executed passing cases"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn quality_runs_only_when_requested() {
    let fixture = Fixture::new();
    fixture.write("crates/core/src/lib.rs", "changed\n");
    fixture.selected(&[]);
    assert!(!fixture.directory.join("quality calls").exists());
    let output = fixture.tool(&["--suite", "quality", "run"]);
    assert!(output.status.success());
    assert!(fixture.directory.join("quality calls").exists());
}

#[test]
fn preparation_follows_the_selected_target_and_stops_on_failure() {
    let fixture = Fixture::new();
    let prepare = fixture.directory.join("prepare");
    let prepared = fixture.directory.join("prepared");
    let fail = fixture.directory.join("fail");
    executable(
        &prepare,
        "#!/bin/sh\n[ ! -f \"$2\" ] || exit 29\nprintf built > \"$1\"\n",
    );
    let policy = fixture.root.join("tests/selection.toml");
    let text = format!(
        "{}\n[[preparation]]\nid = \"fixture-component\"\noutputs = {{WAMN_FIXTURE_COMPONENT = \"{{target}}/component.wasm\"}}\ncommands = [[{:?}, {:?}, {:?}]]\n\n[[preparation]]\npackages = [\"user\"]\ntarget = [\"test:user_live\"]\nprerequisites = [\"fixture-component\"]\n",
        fs::read_to_string(&policy).unwrap(),
        prepare.to_str().unwrap(),
        prepared.to_str().unwrap(),
        fail.to_str().unwrap(),
    );
    fixture.write("tests/selection.toml", &text);
    let suite = EXACT_SUITE
        .replace("package = \"core\"", "package = \"user\"")
        .replace("inputs = [\"^crates/core/\"]", "inputs = []")
        .replace("tests = [\"fixture::case\"]", "tests = []");
    fixture.add_suite(&suite.replace("lib:core", "lib:user"));
    fixture.add_suite(
        &suite
            .replace("fixture-exact", "fixture-input")
            .replace("lib:core", "test:user_live"),
    );
    for (id, needs_input) in [("fixture-exact", false), ("fixture-input", true)] {
        let output = fixture.tool(&["--suite", id, "run"]);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(prepared.exists(), needs_input);
        eprintln!(
            "binding {id}: exit {}, prepared={needs_input}",
            output.status.code().unwrap()
        );
    }
    fs::remove_file(&prepared).unwrap();
    let text = fs::read_to_string(&policy).unwrap().replace(
        "target=\"test:user_live\", tier=\"unit\"",
        "target=\"test:user_live\", tier=\"live-postgres\"",
    );
    fixture.write("tests/selection.toml", &text);
    fixture.commit("fixture preparation tiers");
    fixture.write("crates/user/src/lib.rs", "changed\n");
    for (tier, needs_input) in [("unit", false), ("live-postgres", true)] {
        let output = fixture.tool(&["--tier", tier, "run"]);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(prepared.exists(), needs_input);
        eprintln!("binding tier {tier}: exit 0, prepared={needs_input}");
    }
    fs::remove_file(&prepared).unwrap();
    fs::remove_file(fixture.directory.join("cargo calls")).unwrap();
    fs::write(&fail, "fail").unwrap();
    let output = fixture.tool(&["--suite", "fixture-input", "run"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!prepared.exists());
    assert!(fixture.calls().is_empty());
    eprintln!("binding failed preparation: command exit 29, runner exit 1, no tests");
}

#[test]
fn suite_preparation_sets_its_input_and_preserves_a_nonempty_override() {
    let fixture = Fixture::new();
    let prepare = fixture.directory.join("prepare component");
    let prepared = fixture.directory.join("prepared");
    executable(&prepare, "#!/usr/bin/env bash\nprintf built > \"$1\"\n");
    let path = fixture.root.join("tests/selection.toml");
    let policy = fs::read_to_string(&path).unwrap();
    let preparation = format!(
        "[[preparation]]\nid = \"fixture-component\"\noutputs = {{WAMN_FIXTURE_COMPONENT = \"{{target}}/component.wasm\"}}\ncommands = [[{:?}, {:?}]]\n",
        prepare.to_str().unwrap(),
        prepared.to_str().unwrap()
    );
    fs::write(
        &path,
        policy.replacen("[[suites]]", &(preparation + "[[suites]]"), 1),
    )
    .unwrap();
    fixture.add_suite(&EXACT_SUITE.replace(
        "prerequisites = []",
        "prerequisites = [\"fixture-component\"]",
    ));
    let target = fixture.directory.join("target");
    for override_input in ["", "/explicit/component.wasm"] {
        let expected = if override_input.is_empty() {
            target.join("component.wasm")
        } else {
            PathBuf::from(override_input)
        };
        let output = fixture
            .tool_command(&["--suite", "fixture-exact"])
            .env("CARGO_TARGET_DIR", &target)
            .env("WAMN_FIXTURE_COMPONENT", override_input)
            .env("WAMN_FAKE_EXPECT_INPUT", expected)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(prepared.exists(), override_input.is_empty());
        if prepared.exists() {
            fs::remove_file(&prepared).unwrap();
        }
    }
}

#[test]
fn application_inputs_build_web_artifacts_after_packages() {
    let fixture = Fixture::new();
    fixture.write("apps/web_client/wamn.k", "");
    fixture.write("apps/web_client/web/package.json", "{}");
    fixture.write("apps/no_web/wamn.k", "");
    let policy = fixture.root.join("tests/selection.toml");
    let mut text = fs::read_to_string(&policy).unwrap();
    text.push_str(
        r#"
[[preparation]]
id = "web-fixture"
outputs = {}
applications = ["apps/web_client", "apps/no_web"]
"#,
    );
    fs::write(policy, text).unwrap();
    fixture
        .add_suite(&EXACT_SUITE.replace("prerequisites = []", "prerequisites = [\"web-fixture\"]"));
    let target = fixture.directory.join("target");
    fs::create_dir_all(target.join("debug")).unwrap();
    executable(
        &target.join("debug/wamn-test-postgres"),
        "#!/usr/bin/env bash\nwhile [[ $1 != -- ]]; do shift; done\nshift\nexec \"$@\"\n",
    );
    executable(&target.join("debug/wamn"), FAKE_GUEST_BUILD);
    let output = fixture
        .tool_command(&["--suite", "fixture-exact"])
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fixture.guest_calls(),
        [
            vec!["wamn", "build", "apps/web_client", "apps/no_web"],
            vec!["wamn", "web", "build", "--package", "apps/web_client"],
        ]
    );
}

#[test]
fn two_web_consumers_share_one_install_and_overrides_need_none() {
    let fixture = Fixture::new();
    fixture.write("apps/wamn_wms/wamn.k", "");
    fixture.add_suite(EXACT_SUITE);
    fixture.add_suite(
        &EXACT_SUITE.replace("fixture-exact", "fixture-web").replace(
            "prerequisites = []",
            "prerequisites = [\"platform-ui\", \"sync-bench\"]",
        ),
    );
    let target = fixture.directory.join("target");
    fs::create_dir_all(target.join("debug")).unwrap();
    executable(
        &target.join("debug/wamn-test-postgres"),
        "#!/bin/sh\nexit 0\n",
    );
    executable(
        &fixture.directory.join("pnpm"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$WAMN_FAKE_SCREEN_CALLS\"\n",
    );
    let calls = fixture.directory.join("pnpm calls");
    for (suite, supplied, needs_install) in [
        ("fixture-exact", "", false),
        ("fixture-web", "", true),
        ("fixture-web", "/supplied/web", false),
    ] {
        let output = fixture
            .tool_command(&["--suite", suite])
            .env("CARGO_TARGET_DIR", &target)
            .env("WAMN_PLATFORM_UI_DIST", supplied)
            .env("WAMN_SYNC_BENCH_DIST", supplied)
            .env("WAMN_FAKE_SCREEN_CALLS", &calls)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(calls.exists(), needs_install);
        if needs_install {
            let commands = fs::read_to_string(&calls).unwrap();
            assert_eq!(
                commands
                    .lines()
                    .filter(|line| *line == "install --offline")
                    .count(),
                1,
                "{commands}"
            );
            assert!(commands.starts_with("install --offline\n"), "{commands}");
            assert!(
                commands.contains("--filter @wamn/platform-ui build"),
                "{commands}"
            );
            assert!(
                commands.contains("--dir web/sync-bench run build"),
                "{commands}"
            );
            fs::remove_file(&calls).unwrap();
        }
    }
}

#[test]
fn the_screen_build_follows_application_generation_and_preserves_an_override() {
    let fixture = Fixture::new();
    fixture.write("apps/wamn_wms/wamn.k", "");
    fixture
        .add_suite(&EXACT_SUITE.replace("prerequisites = []", "prerequisites = [\"sync-bench\"]"));
    let target = fixture.directory.join("target");
    let marker = fixture.directory.join("application built");
    let calls = fixture.directory.join("pnpm calls");
    fs::create_dir_all(target.join("debug")).unwrap();
    executable(
        &target.join("debug/wamn-test-postgres"),
        "#!/usr/bin/env bash\ntouch \"$WAMN_FAKE_APPLICATION_MARKER\"\n",
    );
    executable(
        &fixture.directory.join("pnpm"),
        "#!/usr/bin/env bash\nset -eu\nif [[ $1 == install ]]; then [[ ! -f $WAMN_FAKE_APPLICATION_MARKER ]]; else [[ -f $WAMN_FAKE_APPLICATION_MARKER ]]; fi\nprintf '%s\\n' \"$*\" >> \"$WAMN_FAKE_SCREEN_CALLS\"\n",
    );
    for override_input in ["", "/explicit/screen"] {
        let expected = if override_input.is_empty() {
            target.join("sync-bench/web")
        } else {
            PathBuf::from(override_input)
        };
        let output = fixture
            .tool_command(&["--suite", "fixture-exact"])
            .env("CARGO_TARGET_DIR", &target)
            .env("WAMN_SYNC_BENCH_DIST", override_input)
            .env("WAMN_FAKE_EXPECT_SCREEN", expected)
            .env("WAMN_FAKE_APPLICATION_MARKER", &marker)
            .env("WAMN_FAKE_SCREEN_CALLS", &calls)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(calls.exists(), override_input.is_empty());
        assert_eq!(marker.exists(), override_input.is_empty());
        if calls.exists() {
            let commands = fs::read_to_string(&calls).unwrap();
            assert!(commands.starts_with("install --offline\n"), "{commands}");
            assert!(
                commands.contains("--dir web/sync-bench run build --outDir"),
                "{commands}"
            );
            fs::remove_file(&calls).unwrap();
            fs::remove_file(&marker).unwrap();
        }
    }
}

#[test]
fn an_unknown_path_keeps_one_default_command_per_workspace() {
    let fixture = Fixture::new();
    let path = fixture.root.join("tests/selection.toml");
    let policy = fs::read_to_string(&path).unwrap().replace(
        "target=\"test:user_live\", tier=\"unit\"",
        "target=\"test:user_live\", tier=\"kind\"",
    );
    fs::write(path, policy).unwrap();
    fixture.commit("fixture mixed target tiers");
    fixture.write("unowned/code.rs", "changed\n");
    let calls = fixture.selected(&[]);
    assert_eq!(
        calls,
        vec![
            fixture.command(
                "Cargo.toml",
                &[
                    "--workspace",
                    "--features",
                    "wamn-environment-control/ops,wamn-ctl/ops"
                ]
            ),
            fixture.command("apps/Cargo.toml", &["--workspace"]),
            fixture.command("apps/platform/no-std/Cargo.toml", &["--workspace"]),
        ]
    );
}

#[test]
fn the_root_runs_control_route_cases_after_the_nats_input() {
    // wamn-inmae.10: the nats preparation supplies these control_route_live cases.
    let fixture = Fixture::new();
    let policy = fixture.root.join("tests/selection.toml");
    let text = format!(
        "{}\n[[preparation]]\npackages = [\"core\"]\ntarget = [\"lib:core\"]\nprerequisites = [\"nats\"]\n",
        fs::read_to_string(&policy).unwrap().replacen(
            "packages = [\"wamn-execution-host\"]",
            "packages = [\"core\"]",
            1,
        )
    );
    fs::write(policy, text).unwrap();
    fixture.commit("fixture nats input");
    executable(
        &fixture.directory.join("nats-server"),
        "#!/bin/sh\nexit 0\n",
    );
    executable(&fixture.directory.join("rustc"), "#!/bin/sh\nexit 0\n");
    let cases = [
        (
            "connection_routes_refuse_mismatches_and_project_only_enabled_rows",
            "requires: env:WAMN_NATIVE_C_NATS_BIN",
        ),
        (
            "oauth_authorization::project_admin_authorize_and_public_pkce_callback",
            "requires: env:WAMN_NATIVE_C_NATS_BIN",
        ),
        (
            "connection_test::candidate_http_and_modbus_health_commit_nothing",
            "requires: env:WAMN_NATIVE_C_NATS_BIN, bin:docker, bin:rustc, bin:/bin/sh",
        ),
        (
            "delayed_grant_cannot_restore_revoked_admin",
            "requires: env:WAMN_NATIVE_C_NATS_BIN",
        ),
    ];
    let transcript = fixture.directory.join("root output");
    fs::write(
        &transcript,
        fixture.ignored_target("core", "lib", "core", &cases),
    )
    .unwrap();
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let output = fixture
        .tool_command(&[])
        .env("WAMN_NATIVE_C_NATS_BIN", "")
        .env("WAMN_FAKE_ROOT_OUTPUT", transcript)
        .env("WAMN_FAKE_PASSED", "0")
        .env("WAMN_FAKE_EXACT_PASSED", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fixture.calls();
    let ignored = calls
        .iter()
        .filter(|call| call.contains(&"--ignored".into()))
        .collect::<Vec<_>>();
    assert_eq!(ignored.len(), cases.len(), "{calls:?}");
    for (call, (name, _)) in ignored.iter().zip(cases) {
        assert_eq!(
            call[0],
            fixture.directory.join("core-fixture").display().to_string()
        );
        assert!(call.contains(&name.to_owned()), "{call:?}");
        assert!(call.contains(&"--exact".into()), "{call:?}");
        assert!(!call.contains(&"--features".into()), "{call:?}");
    }
    assert_eq!(calls.len(), cases.len() + 1, "{calls:?}");
    assert!(
        calls[0]
            .windows(2)
            .any(|pair| pair == ["--features", "core/test-util,user/ops"]),
        "{calls:?}"
    );
    fixture.write("Cargo.lock", "changed\n");
    let output = fixture.tool(&["dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let root = stdout
        .lines()
        .find(|line| line.starts_with("root ("))
        .unwrap();
    assert!(root.contains("core/test-util"), "{root}");
}

#[test]
fn the_root_runs_generated_schema_after_its_output_is_exported() {
    // wamn-inmae.11: fixture-components exports this application-generated input.
    let fixture = Fixture::new();
    let policy = fixture.root.join("tests/selection.toml");
    let text = fs::read_to_string(&policy).unwrap();
    let preparation = "[[preparation]]\nid = \"schema\"\noutputs = {WAMN_GENERATED_WIDGET_UPDATE_SCHEMA = \"{target}/widget/update.json\"}\n\n[[preparation]]\npackages = [\"user\"]\ntarget = [\"test:user_live\"]\nprerequisites = [\"schema\"]\n";
    fs::write(
        &policy,
        text.replacen("[[suites]]", &format!("{preparation}\n[[suites]]"), 1),
    )
    .unwrap();
    fixture.commit("fixture schema output");
    let name = "generated_update_schema_agrees_with_client_presence_and_null_behavior";
    let transcript = fixture.directory.join("root output");
    fs::write(
        &transcript,
        fixture.ignored_target(
            "user",
            "test",
            "user_live",
            &[(name, "requires: env:WAMN_GENERATED_WIDGET_UPDATE_SCHEMA")],
        ),
    )
    .unwrap();
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let output = fixture
        .tool_command(&[])
        .env("WAMN_GENERATED_WIDGET_UPDATE_SCHEMA", "")
        .env("WAMN_FAKE_ROOT_OUTPUT", transcript)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fixture.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.contains(&name.to_owned()))
            .count(),
        1,
        "{calls:?}"
    );
    let call = calls
        .iter()
        .find(|call| call.contains(&name.to_owned()))
        .unwrap();
    assert_eq!(
        call[0],
        fixture
            .directory
            .join("user_live-fixture")
            .display()
            .to_string()
    );
    assert!(!call.contains(&"--features".into()), "{call:?}");
}

#[test]
fn the_root_leaves_unsupplied_and_unrecognized_reasons_ignored() {
    let fixture = Fixture::new();
    let transcript = fixture.directory.join("root output");
    let output = fixture.ignored_target(
        "core",
        "lib",
        "core",
        &[
            (
                "fixture::missing_env",
                "requires: bin:/bin/sh, env:WAMN_FIXTURE_MISSING",
            ),
            (
                "fixture::missing_bin",
                "requires: bin:wamn-fixture-missing-executable",
            ),
            (
                "fixture::explanation",
                "an operator must prepare the fixture",
            ),
            ("fixture::invalid", "requires: env:PATH, file:input"),
            ("fixture::empty", "requires: "),
        ],
    );
    fs::write(&transcript, output).unwrap();
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let output = fixture
        .tool_command(&[])
        .env("WAMN_FIXTURE_MISSING", "")
        .env("WAMN_FAKE_ROOT_OUTPUT", transcript)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !fixture
            .calls()
            .iter()
            .any(|call| call.contains(&"--ignored".into()))
    );
}

#[test]
fn equal_names_run_only_in_the_target_with_supplied_prerequisites() {
    let fixture = Fixture::new();
    let name = "fixture::same_name";
    let transcript = fixture.directory.join("root output");
    let mut output = fixture.ignored_target(
        "core",
        "lib",
        "core",
        &[(name, "requires: env:WAMN_FIXTURE_PRESENT")],
    );
    output.push_str(&fixture.ignored_target(
        "user",
        "test",
        "user_live",
        &[(
            name,
            "requires: env:WAMN_FIXTURE_PRESENT, env:WAMN_FIXTURE_MISSING",
        )],
    ));
    fs::write(&transcript, output).unwrap();
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let output = fixture
        .tool_command(&[])
        .env("WAMN_FIXTURE_PRESENT", "supplied")
        .env("WAMN_FIXTURE_MISSING", "")
        .env("WAMN_FAKE_ROOT_OUTPUT", transcript)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fixture.calls();
    let ignored = calls
        .iter()
        .filter(|call| call.contains(&"--ignored".into()))
        .collect::<Vec<_>>();
    assert_eq!(ignored.len(), 1, "{calls:?}");
    assert_eq!(
        ignored[0][0],
        fixture.directory.join("core-fixture").display().to_string()
    );
    assert!(ignored[0].contains(&name.to_owned()), "{calls:?}");
}

#[test]
fn a_full_root_keeps_kind_cases_ignored_with_supplied_prerequisites() {
    let fixture = Fixture::new();
    let policy = fixture.root.join("tests/selection.toml");
    let text = fs::read_to_string(&policy).unwrap().replace(
        "target=\"test:user_live\", tier=\"unit\"",
        "target=\"test:user_live\", tier=\"kind\"",
    );
    fs::write(policy, text).unwrap();
    fixture.commit("fixture kind target");
    fixture.write("Cargo.lock", "changed\n");
    let transcript = fixture.directory.join("root output");
    fs::write(
        &transcript,
        fixture.ignored_target(
            "user",
            "test",
            "user_live",
            &[(
                "fixture::kind",
                "requires: env:WAMN_FIXTURE_PRESENT, bin:/bin/sh",
            )],
        ),
    )
    .unwrap();
    let output = fixture
        .tool_command(&[])
        .env("WAMN_FIXTURE_PRESENT", "supplied")
        .env("WAMN_FAKE_ROOT_OUTPUT", transcript)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fixture.calls();
    assert!(calls[0].contains(&"--workspace".into()), "{calls:?}");
    assert!(
        !calls.iter().any(|call| call.contains(&"--ignored".into())),
        "{calls:?}"
    );
}

#[test]
fn a_full_root_keeps_cases_without_a_tier_row_ignored() {
    let fixture = Fixture::new();
    let policy = fixture.root.join("tests/selection.toml");
    let text = fs::read_to_string(&policy)
        .unwrap()
        .lines()
        .filter(|line| !line.contains("target=\"test:user_live\""))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(policy, text).unwrap();
    fixture.commit("fixture target without a tier row");
    fixture.write("Cargo.lock", "changed\n");
    let transcript = fixture.directory.join("root output");
    fs::write(
        &transcript,
        fixture.ignored_target(
            "user",
            "test",
            "user_live",
            &[(
                "fixture::unassigned",
                "requires: env:WAMN_FIXTURE_PRESENT, bin:/bin/sh",
            )],
        ),
    )
    .unwrap();
    let output = fixture
        .tool_command(&[])
        .env("WAMN_FIXTURE_PRESENT", "supplied")
        .env("WAMN_FAKE_ROOT_OUTPUT", transcript)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fixture.calls();
    assert!(calls[0].contains(&"--workspace".into()), "{calls:?}");
    assert!(
        !calls.iter().any(|call| call.contains(&"--ignored".into())),
        "{calls:?}"
    );
}

#[test]
fn a_path_suite_adds_features_without_repeating_a_selected_target() {
    let fixture = Fixture::new();
    fixture.add_suite(
        &EXACT_SUITE
            .replace("tests = [\"fixture::case\"]", "tests = []")
            .replace("features = []", "features = [\"ops\"]"),
    );
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let calls = fixture.selected(&[]);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0]
            .windows(2)
            .any(|pair| pair == ["--features", "user/ops,core/ops"]),
        "{calls:?}"
    );
}

#[test]
fn an_ignored_suite_owns_only_its_selected_target_and_case() {
    let fixture = Fixture::new();
    fixture.add_suite(&EXACT_SUITE.replace(
        "tests = [\"fixture::case\"]",
        "tests = [\"fixture::case\"]\nignored = true",
    ));
    let name = "fixture::case";
    let transcript = fixture.directory.join("root output");
    let mut output =
        fixture.ignored_target("core", "lib", "core", &[(name, "requires: bin:/bin/sh")]);
    output.push_str(&fixture.ignored_target(
        "user",
        "test",
        "user_live",
        &[(name, "requires: bin:/bin/sh")],
    ));
    fs::write(&transcript, output).unwrap();
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let output = fixture
        .tool_command(&[])
        .env("WAMN_FAKE_ROOT_OUTPUT", transcript)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fixture.calls();
    let owners = calls
        .iter()
        .filter(|call| call.contains(&name.to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(owners.len(), 2, "{calls:?}");
    assert!(
        owners.iter().any(|call| call[0]
            == fixture
                .directory
                .join("user_live-fixture")
                .display()
                .to_string()),
        "{calls:?}"
    );
    assert!(
        owners
            .iter()
            .any(|call| call.windows(2).any(|pair| pair == ["-p", "core"])),
        "{calls:?}"
    );
}

#[test]
fn a_kind_target_runs_its_exact_suite_or_its_focused_filter() {
    let fixture = Fixture::new();
    fixture.kind_targets();
    fixture.add_suite(
        &EXACT_SUITE
            .replace("tier = \"contract\"", "tier = \"kind\"")
            .replace(
                "tests = [\"fixture::case\"]",
                "tests = [\"fixture::case\"]\nignored = true",
            ),
    );
    fixture.write("crates/core/src/lib.rs", "changed\n");
    let calls = fixture.selected(&["--tier", "kind"]);
    let owners = calls
        .iter()
        .filter(|call| call.windows(2).any(|pair| pair == ["-p", "core"]))
        .collect::<Vec<_>>();
    assert_eq!(owners.len(), 1, "{calls:?}");
    assert!(owners[0].contains(&"--exact".into()), "{calls:?}");
    let output = fixture.tool(&["--tier", "kind", "--name", "focused", "dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout
            .lines()
            .any(|line| line.starts_with("fixture-exact:")),
        "{stdout}"
    );
    assert!(
        stdout.lines().any(|line| line.starts_with("root:")
            && line.contains("-p core")
            && line.contains(" focused ")),
        "{stdout}"
    );
}

//! Exact repo-local lint coverage over all three Cargo workspaces.
//! Fake Cargo covers runner argv and reporting, not runtime isolation behavior.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const TOOL: &str = "tools/repo-lint";
const HTTP_SOURCE: &str = "crates/platform/runtime/src/plugins/connection_http.rs";
const HTTP_TRANSPORT: &str = "crates/platform/runtime/src/plugins/connection_http/transport.rs";
const COMPONENT_MANIFEST: &str = "components/Cargo.toml";
const NO_STD_MANIFEST: &str = "components/no-std/Cargo.toml";
const LEG_LABELS: [&str; 10] = [
    "connection HTTP scoped retained clients",
    "root rustfmt",
    "components rustfmt",
    "no-std rustfmt",
    "root Clippy",
    "components native Clippy",
    "connection-http-standard native Clippy",
    "components wasm Clippy",
    "no-std native Clippy",
    "no-std wasm Clippy",
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

#[test]
fn repo_lint_uses_cargo_owned_workspace_selection_from_any_directory() {
    let root = repository_root();
    let tool = root.join(TOOL);
    let metadata = fs::metadata(&tool).expect("read repo-lint metadata");
    assert_ne!(metadata.permissions().mode() & 0o111, 0);
    let source = fs::read_to_string(&tool).expect("read repo-lint source");
    // wamn-0h0g.15.139: the needle is `eval ` with the trailing space, not bare
    // `eval`. The target is the bash builtin, which always takes an argument and
    // so is always followed by whitespace. Bare, the needle is a prefix of
    // ordinary English — `evaluate`, `evaluation`, `re-evaluate` — so one comment
    // in this shell tool saying it re-evaluates its leg list would have failed a
    // guard about executing argv directly. That is the `flowrunner` shape of
    // wamn-0h0g.15.131. Do not re-broaden this to the bare word.
    assert!(
        !source.contains("eval "),
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
    let root_manifest_path = root.join("Cargo.toml").display().to_string();
    let component_manifest_path = root.join(COMPONENT_MANIFEST).display().to_string();
    let no_std_manifest_path = root.join(NO_STD_MANIFEST).display().to_string();
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
                "fmt".into(),
                "--manifest-path".into(),
                no_std_manifest_path.clone(),
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
                component_manifest_path,
                "--locked".into(),
                "--workspace".into(),
                "--target".into(),
                "wasm32-wasip2".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ],
            vec![
                root_path.clone(),
                "clippy".into(),
                "--manifest-path".into(),
                no_std_manifest_path.clone(),
                "--locked".into(),
                "--workspace".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ],
            vec![
                root_path,
                "clippy".into(),
                "--manifest-path".into(),
                no_std_manifest_path,
                "--locked".into(),
                "--workspace".into(),
                "--target".into(),
                "wasm32-wasip2".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ],
        ]
    );
    assert_eq!(
        fs::read_to_string(directory.path("cargo calls.rustflags"))
            .expect("read fake Cargo RUSTFLAGS log")
            .lines()
            .collect::<Vec<_>>(),
        [
            "",
            "",
            "",
            "",
            "",
            "-C panic=abort",
            "",
            "-C panic=abort",
            ""
        ]
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
        9,
        "an early failure must not hide a later Cargo leg"
    );
    assert_leg_statuses(&output, Some(("root rustfmt", 23)));
}

fn scope_fixture_output(relative: &str, before: &str, after: &str) -> Output {
    let directory = TestDirectory::new();
    let root = directory.path("repository");
    for relative in [TOOL, HTTP_SOURCE, HTTP_TRANSPORT] {
        let destination = root.join(relative);
        fs::create_dir_all(destination.parent().expect("fixture file parent"))
            .expect("create isolated source fixture");
        fs::copy(repository_root().join(relative), destination).expect("copy source fixture");
    }
    let changed = root.join(relative);
    let source = fs::read_to_string(&changed).expect("read source fixture");
    assert_eq!(
        source.matches(before).count(),
        1,
        "mutation must target one site"
    );
    fs::write(changed, source.replacen(before, after, 1)).expect("mutate isolated source fixture");
    executable(&directory.path("fake cargo"), FAKE_CARGO);
    let output = run_tool(&root, &directory, &["run"]);
    assert_eq!(
        captured_invocations(&directory.path("cargo calls")).len(),
        9,
        "a static-leg failure must not hide any Cargo leg"
    );
    output
}

fn assert_scope_refusal(relative: &str, before: &str, after: &str, reason: &str) {
    let output = scope_fixture_output(relative, before, after);
    assert_eq!(output.status.code(), Some(1));
    assert_leg_statuses(
        &output,
        Some(("connection HTTP scoped retained clients", 65)),
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(reason),
        "guard omitted {reason}:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn repo_lint_refuses_invocation_identity_in_the_pool_key() {
    assert_scope_refusal(
        HTTP_TRANSPORT,
        "struct ClientKey {\n",
        "struct ClientKey {\n    invocation_id: String,\n",
        "ClientKey must not retain invocation identity",
    );
}

#[test]
fn repo_lint_refuses_each_omitted_scope_dimension() {
    for (declaration, field, kind) in [
        ("ConnectionScope", "tenant", "Box<str>"),
        ("ConnectionScope", "project", "Box<str>"),
        ("ConnectionScope", "environment", "Box<str>"),
        ("ConnectionScope", "instance", "Box<str>"),
        ("ClientScope", "connection", "ConnectionScope"),
        ("ClientScope", "package", "Box<str>"),
        ("ClientScope", "component_digest", "Box<str>"),
        ("ClientScope", "requirement", "Box<str>"),
        ("ClientScope", "binding_hash", "Box<str>"),
        ("ClientScope", "definition_hash", "Box<str>"),
        ("ClientScope", "generation", "i64"),
        ("Target", "authority", "Box<str>"),
        ("Target", "peer", "SocketAddr"),
        ("Target", "tls_name", "Option<Box<str>>"),
    ] {
        let visibility = if declaration == "Target" {
            ""
        } else {
            "pub(crate) "
        };
        let mut field_source = format!("    {visibility}{field}: {kind},\n");
        if declaration == "Target" && field == "peer" {
            field_source.push_str("    tls_name: Option<Box<str>>,\n");
        }
        // A commented declaration must not satisfy the required field.
        assert_scope_refusal(
            HTTP_TRANSPORT,
            &field_source,
            &format!("// {field_source}"),
            &format!("{declaration}.{field} must remain {kind}"),
        );
    }
}

#[test]
fn repo_lint_refuses_unscoped_retained_clients() {
    assert_scope_refusal(
        HTTP_TRANSPORT,
        "struct Inner {\n",
        "struct Inner {\n    fallback: HttpClient,\n",
        "unscoped retained HTTP client in Inner",
    );
    assert_scope_refusal(
        HTTP_TRANSPORT,
        "clients: HashMap<ClientKey, CachedClient>,",
        "clients: HashMap<String, CachedClient>,",
        "State.clients must remain HashMap<ClientKey,CachedClient>",
    );
    assert_scope_refusal(
        HTTP_TRANSPORT,
        "struct Inner {\n",
        "struct Unscoped(HttpClient);\nstruct Inner {\n",
        "unsupported HTTP client retention declaration",
    );
}

#[test]
fn repo_lint_refuses_client_cells_but_accepts_unrelated_cells() {
    assert_scope_refusal(
        HTTP_TRANSPORT,
        "struct Inner {\n",
        "static HTTP: std::sync::OnceLock<HttpClient> = std::sync::OnceLock::new();\nstruct Inner {\n",
        "unsupported HTTP client retention declaration",
    );
    let output = scope_fixture_output(
        HTTP_TRANSPORT,
        "struct Inner {\n",
        "static COUNT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();\nstruct Inner {\n",
    );
    assert!(
        output.status.success(),
        "unrelated cell must not fail the guard"
    );
    assert_leg_statuses(&output, None);
}

#[test]
fn repo_lint_refuses_partial_hashing_and_unsupported_keys() {
    assert_scope_refusal(
        HTTP_TRANSPORT,
        "#[derive(Clone, Eq, Hash, PartialEq)]\nstruct ClientKey",
        "#[derive(Clone, Eq, PartialEq)]\nstruct ClientKey",
        "ClientKey must derive Eq, Hash, and PartialEq",
    );
    assert_scope_refusal(
        HTTP_TRANSPORT,
        "struct ClientKey {\n    scope: ClientScope,\n    target: Target,\n}",
        "struct ClientKey(ClientScope, Target);",
        "missing or unsupported ClientKey declaration",
    );
}

#[test]
fn repo_lint_refuses_constant_or_invocation_substitutes_at_acquisition() {
    for (before, after) in [
        (
            "tenant: self.tenant.clone(),",
            "tenant: String::new().into(),",
        ),
        (
            "package: invocation.package_id.clone().into(),",
            "package: invocation.node_id.clone().into(),",
        ),
        (
            "generation: snapshot\n                .generation\n                .ok_or(ConnectionError::CredentialUnavailable)?,",
            "generation: 0,",
        ),
    ] {
        assert_scope_refusal(
            HTTP_SOURCE,
            before,
            after,
            "ClientScope acquisition must use the attested",
        );
    }
}

#[test]
fn repo_lint_returns_failure_when_the_last_leg_fails() {
    let root = repository_root();
    let directory = TestDirectory::new();
    executable(&directory.path("fake cargo"), FAKE_CARGO);

    let output = tool_command(&root, &directory)
        .env("WAMN_FAKE_CARGO_FAIL_AT", "9")
        .arg("run")
        .output()
        .expect("run failing repo-lint tool");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        captured_invocations(&directory.path("cargo calls")).len(),
        9,
        "the final Cargo leg must run"
    );
    assert_leg_statuses(&output, Some(("no-std wasm Clippy", 23)));
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
    assert!(plan.contains("no-std-native-clippy: RUSTFLAGS=-C\\ panic=abort"));
    for label in [
        "connection-http-scope:",
        "root-rustfmt:",
        "components-rustfmt:",
        "no-std-rustfmt:",
        "root-clippy:",
        "components-native-clippy:",
        "connection-http-native-clippy:",
        "components-wasm-clippy:",
        "no-std-native-clippy:",
        "no-std-wasm-clippy:",
    ] {
        assert!(plan.contains(label), "dry-run omitted {label}");
    }

    let output = run_tool(&root, &directory, &["unknown"]);
    assert_eq!(output.status.code(), Some(64));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown command"));
    assert!(!directory.path("cargo calls").exists());
}

//! Conformance guard for the opt-in Cranelift developer build path.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const HELPER: &str = "tools/cargo-cranelift";
const BACKEND_FLAG: &str = "-Zcodegen-backend=cranelift";
const POLICY_TOKENS: &[&str] = &[
    "+nightly",
    "cargo cranelift",
    "cargo-cranelift",
    "codegen-backend=cranelift",
    "rustc-codegen-cranelift",
];

/// Directory names [`text_files`] never descends into, at any depth: build output
/// (a nested crate's `target/debug/.fingerprint/*` files are extension-less and
/// BINARY, so scanning them panicked the walker outright) and VCS metadata.
const SKIPPED_DIRECTORIES: &[&str] = &["target", ".git"];

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

#[derive(Debug)]
struct DockerStage {
    parent: String,
    name: String,
    first_line: usize,
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

fn docker_stages(dockerfile: &str) -> Vec<DockerStage> {
    dockerfile
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            if fields.first() != Some(&"FROM") {
                return None;
            }
            assert_eq!(fields.len(), 4, "unexpected FROM shape: {line}");
            assert_eq!(fields[2], "AS", "Docker stages must be named: {line}");
            Some(DockerStage {
                parent: fields[1].to_string(),
                name: fields[3].to_string(),
                first_line: index + 1,
            })
        })
        .collect()
}

fn text_files(path: &Path, files: &mut Vec<PathBuf>) {
    if path.is_file() {
        files.push(path.to_path_buf());
        return;
    }

    let mut entries = fs::read_dir(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        .map(|entry| entry.expect("read directory entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            let name = entry
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if SKIPPED_DIRECTORIES.contains(&name.as_str()) {
                continue;
            }
            text_files(&entry, files);
        } else if matches!(
            entry.extension().and_then(|extension| extension.to_str()),
            Some("json" | "rs" | "sh" | "toml" | "yaml" | "yml") | None
        ) {
            files.push(entry);
        }
    }
}

fn assert_no_policy_tokens(path: &Path) {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {} as UTF-8: {error}", path.display()));
    let lower = source.to_ascii_lowercase();
    for token in POLICY_TOKENS {
        assert!(
            !lower.contains(token),
            "{} contains developer-only policy token {token:?}",
            path.display()
        );
    }
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
        &["cranelift", "--locked", "-p", "wamn-flow-invocation", "-vv"],
    );
    assert!(
        output.status.success(),
        "valid helper invocation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(directory.path("cargo.log")).expect("read fake Cargo log"),
        format!(
            "RUSTFLAGS={BACKEND_FLAG}\nARG=+nightly\nARG=build\nARG=--locked\nARG=-p\nARG=wamn-flow-invocation\nARG=-vv\n"
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

#[test]
fn cranelift_toolchain_is_isolated_to_one_leaf_developer_stage() {
    let root = repository_root();
    let dockerfile = fs::read_to_string(root.join("Dockerfile")).expect("read Dockerfile");
    let stages = docker_stages(&dockerfile);
    let developer = stages
        .iter()
        .find(|stage| stage.name == "cranelift-dev")
        .expect("Dockerfile must define cranelift-dev");
    assert_eq!(developer.parent, "chef");
    assert_eq!(
        stages.last().map(|stage| stage.name.as_str()),
        Some("cranelift-dev")
    );
    assert!(
        stages.iter().all(|stage| stage.parent != "cranelift-dev"),
        "cranelift-dev must remain a leaf stage"
    );

    let lines = dockerfile.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let lower = line.to_ascii_lowercase();
        if !["nightly", "cranelift", "codegen-backend"]
            .iter()
            .any(|token| lower.contains(token))
        {
            continue;
        }
        let owner = stages
            .iter()
            .rev()
            .find(|stage| stage.first_line <= index + 1)
            .expect("sensitive Docker line must belong to a named stage");
        assert_eq!(
            owner.name,
            "cranelift-dev",
            "developer toolchain leaked at Dockerfile line {}: {line}",
            index + 1
        );
    }

    assert!(dockerfile.contains("rustup toolchain install nightly --profile minimal"));
    assert!(
        dockerfile
            .contains("rustup component add rustc-codegen-cranelift-preview --toolchain nightly")
    );
    assert!(
        dockerfile
            .contains("COPY --chmod=0755 tools/cargo-cranelift /usr/local/bin/cargo-cranelift")
    );
    assert!(dockerfile.contains("WORKDIR /workspace"));
}

/// wamn-hyrn: the scan reads extension-less files (shell helpers carry no
/// extension), so an in-place `cargo build` under a scanned tree used to feed it
/// `target/debug/.fingerprint/*` and panic `stream did not contain valid UTF-8`
/// before any policy assertion ran. Build output is skipped by directory NAME, at
/// any depth — a sub-crate's `target/` nests below the scan roots.
#[test]
fn text_file_scan_skips_build_output_but_keeps_extensionless_sources() {
    let directory = TestDirectory::new();
    let root = directory.path("scan");
    let planted = [
        ("crate/target/debug/.fingerprint/lib-abc123/output", false),
        (".git/objects/ab/cdef0123456789", false),
        ("tools/helper", true),
        ("crate/build.rs", true),
    ];
    for (relative, _) in planted {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("planted file has a parent"))
            .expect("create planted directory");
        fs::write(&path, [0xff, 0xfe, 0x00, 0x9f]).expect("write planted file");
    }
    // The scanned files must be readable as UTF-8 text; the skipped ones are not.
    for (relative, scanned) in planted {
        if scanned {
            fs::write(root.join(relative), "#!/usr/bin/env bash\ntrue\n")
                .expect("write scanned file");
        }
    }

    let mut files = Vec::new();
    text_files(&root, &mut files);

    let expected = planted
        .iter()
        .filter(|(_, scanned)| *scanned)
        .map(|(relative, _)| root.join(relative))
        .collect::<Vec<_>>();
    assert_eq!(
        files.iter().collect::<std::collections::BTreeSet<_>>(),
        expected.iter().collect::<std::collections::BTreeSet<_>>(),
        "scan descended into build output or dropped an extension-less source"
    );
    for path in &files {
        assert_no_policy_tokens(path);
    }
}

#[test]
fn stable_defaults_and_shipping_command_surfaces_exclude_cranelift() {
    let root = repository_root();
    for configuration in ["rust-toolchain.toml", ".cargo/config.toml"] {
        assert_no_policy_tokens(&root.join(configuration));
    }

    let mut command_surfaces = Vec::new();
    for path in [
        "architecture",
        "components",
        "deploy",
        "services/builder",
        "tools",
    ] {
        text_files(&root.join(path), &mut command_surfaces);
    }
    command_surfaces.retain(|path| path != &root.join(HELPER));
    for path in command_surfaces {
        assert_no_policy_tokens(&path);
    }
}

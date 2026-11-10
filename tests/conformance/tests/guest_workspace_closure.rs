//! Guest path dependencies stay inside their workspace.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest as _, Sha256};

const GUEST_WORKSPACES: [&str; 2] = ["apps/Cargo.toml", "apps/platform/no-std/Cargo.toml"];

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the conformance package lives at tests/conformance")
        .to_path_buf()
}

#[test]
fn no_guest_workspace_declares_a_dependency_outside_itself() {
    for workspace in GUEST_WORKSPACES {
        let output = Command::new(env!("CARGO"))
            .args([
                "metadata",
                "--no-deps",
                "--locked",
                "--offline",
                "--format-version",
                "1",
                "--manifest-path",
                workspace,
            ])
            .current_dir(repository_root())
            .output()
            .expect("run Cargo metadata for the guest workspace");
        assert!(
            output.status.success(),
            "{workspace}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("decode Cargo metadata");
        let root = Path::new(metadata["workspace_root"].as_str().unwrap());
        let members = metadata["workspace_members"].as_array().unwrap();
        for package in metadata["packages"].as_array().unwrap() {
            if !members.contains(&package["id"]) {
                continue;
            }
            for dependency in package["dependencies"].as_array().unwrap() {
                if let Some(path) = dependency["path"].as_str() {
                    assert!(
                        Path::new(path).starts_with(root),
                        "{workspace}: {} depends on {path} outside {}",
                        package["name"],
                        root.display()
                    );
                }
            }
        }
    }
}

/// Directories holding one checkout's virtualized guest artifacts.
const REPRO_A_ENV: &str = "WAMN_DIGEST_REPRO_A";
const REPRO_B_ENV: &str = "WAMN_DIGEST_REPRO_B";

fn virtualized_digests(directory: &Path) -> Vec<(String, String)> {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()));
    let mut digests = Vec::new();
    for entry in entries {
        let path = entry.expect("read artifact directory entry").path();
        if path
            .extension()
            .is_some_and(|extension| extension == "wasm")
        {
            let bytes =
                fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("artifact file name is UTF-8")
                .to_string();
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let hex = hasher
                .finalize()
                .iter()
                .fold(String::new(), |mut out, byte| {
                    use std::fmt::Write as _;
                    write!(out, "{byte:02x}").expect("writing to a string is infallible");
                    out
                });
            digests.push((name, hex));
        }
    }
    digests.sort();
    digests
}

/// THE PROPERTY THE RELOCATION EXISTS FOR: one commit, two checkouts, one digest.
///
/// Armed through `docs/operations/building.md#guest-artifact-comparisons`,
/// which builds the guests in two worktrees of the same commit and points this
/// test at both artifact directories. Ignored by default because it costs two
/// full guest builds; the structural guards above run every time and are what
/// catch a regression early.
#[test]
#[ignore = "requires: env:WAMN_DIGEST_REPRO_A, env:WAMN_DIGEST_REPRO_B"]
fn one_commit_built_in_two_checkouts_yields_identical_guest_digests() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_DIGEST_REPRO_A, env:WAMN_DIGEST_REPRO_B",
    );
    let a = std::env::var(REPRO_A_ENV)
        .unwrap_or_else(|_| panic!("{REPRO_A_ENV} must name the first checkout's artifacts"));
    let b = std::env::var(REPRO_B_ENV)
        .unwrap_or_else(|_| panic!("{REPRO_B_ENV} must name the second checkout's artifacts"));
    assert_ne!(
        a, b,
        "the two artifact directories must come from different checkouts"
    );

    let first = virtualized_digests(Path::new(&a));
    let second = virtualized_digests(Path::new(&b));
    assert!(
        !first.is_empty(),
        "{a} holds no virtualized guest artifacts, so this compares no guest artifacts"
    );
    assert_eq!(
        first, second,
        "the same commit produced different guest digests in two checkouts, so a component \
         digest is still a claim about the build directory rather than about the source \
         (wamn-10yt.10.29)"
    );
}

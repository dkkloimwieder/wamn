//! No guest-consumed crate may live outside the components workspace.
//!
//! # Why this is a gate and not a convention
//!
//! A Cargo path dependency that escapes its workspace root gets a `-C metadata`
//! hash derived from its ABSOLUTE path, which lands in the v0 crate
//! disambiguator of every mangled symbol and therefore in the compiled bytes.
//! A component digest built from such a graph is a claim about the checkout it
//! was built in, not about the source an author wrote: the same commit produced
//! a different digest in every worktree, so a pin minted in one was
//! unreproducible in all the others, and `[WAMN-DEV-LIVE]` could only pass from
//! the directory the pin happened to be minted in (`wamn-10yt.10.29`).
//!
//! `wamn-10yt.10.29` relocated the four escaping crates under `components/`.
//! This gate keeps them there. It is deliberately structural rather than a
//! digest comparison: the property is cheap to assert on every run, while
//! comparing two checkouts' digests costs two full guest builds.
//!
//! The companion channel — absolute `file!()` strings that `include!`d package
//! sources bake into the artifact — is closed by `--remap-path-prefix` in
//! `tools/build-components`, asserted below so the two cannot drift apart.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

/// Every workspace whose members are compiled into guest artifacts.
const GUEST_WORKSPACES: [&str; 2] = ["components/Cargo.toml", "components/no-std/Cargo.toml"];

/// The one call every guest workspace leg compiles through.
const BUILD_TOOL: &str = "tools/build-components";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the conformance package lives at tests/conformance")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repository_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// `path = "..."` values declared anywhere in one manifest, with their line.
fn declared_paths(source: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        let mut rest = trimmed;
        while let Some(at) = rest.find("path = \"") {
            let tail = &rest[at + "path = \"".len()..];
            let Some(end) = tail.find('"') else { break };
            found.push((index + 1, tail[..end].to_string()));
            rest = &tail[end..];
        }
    }
    found
}

#[test]
fn no_guest_workspace_declares_a_dependency_outside_itself() {
    let mut escapes = Vec::new();
    for manifest in GUEST_WORKSPACES {
        for (line, declared) in declared_paths(&read(manifest)) {
            if declared.starts_with("..") || declared.starts_with('/') {
                escapes.push(format!("{manifest}:{line}: path = {declared:?}"));
            }
        }
    }
    assert!(
        escapes.is_empty(),
        "a guest workspace declares a dependency outside itself, which makes every component \
         digest a function of the build directory rather than of the source \
         (wamn-10yt.10.29). Move the crate under the workspace instead of reaching out to it: \
         {escapes:#?}"
    );
}

#[test]
fn the_shared_guest_build_remaps_the_source_prefix() {
    let tool = read(BUILD_TOOL);
    assert!(
        tool.contains("--remap-path-prefix=$WAMN_REPOSITORY_ROOT="),
        "{BUILD_TOOL} must remap the repository root out of guest artifacts; without it the \
         absolute file!() strings that include!d package sources carry survive into the bytes \
         and the digest moves with the checkout (wamn-10yt.10.29)"
    );
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
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            digests.push((name, hex));
        }
    }
    digests.sort();
    digests
}

/// THE PROPERTY THE RELOCATION EXISTS FOR: one commit, two checkouts, one digest.
///
/// Armed by `[GUEST-DIGEST-REPRODUCIBILITY]` in `docs/operations/build-and-test.md`,
/// which builds the guests in two worktrees of the same commit and points this
/// test at both artifact directories. Ignored by default because it costs two
/// full guest builds; the structural guards above run every time and are what
/// catch a regression early.
#[test]
#[ignore = "requires two checkouts of one commit built by [GUEST-DIGEST-REPRODUCIBILITY]"]
fn one_commit_built_in_two_checkouts_yields_identical_guest_digests() {
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
        "{a} holds no virtualized guest artifacts, so this proves nothing"
    );
    assert_eq!(
        first, second,
        "the same commit produced different guest digests in two checkouts, so a component \
         digest is still a claim about the build directory rather than about the source \
         (wamn-10yt.10.29)"
    );
}

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
//!
//! A shared Cargo invocation previously changed guest bytes when the selected
//! packages changed (`wamn-10yt.61`). Each guest now uses its own invocation.
//! The cross-profile arm compares declared application guests with all workspace
//! guests to prove that the selection does not change their bytes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

/// Every workspace whose members are compiled into guest artifacts.
const GUEST_WORKSPACES: [&str; 2] = ["apps/Cargo.toml", "apps/platform/no-std/Cargo.toml"];

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

/// Artifact plans printed by `tools/build-components build-only app APP_DIRECTORY...`
/// and `tools/build-components build-only all`.
const PROFILE_APP_PLAN_ENV: &str = "WAMN_DIGEST_PROFILE_APP_PLAN";
const PROFILE_ALL_PLAN_ENV: &str = "WAMN_DIGEST_PROFILE_ALL_PLAN";

/// `package -> sha256` from one artifact plan, refusing a plan for another profile.
///
/// The plan is the build tool's own output and already carries the raw digest of
/// every artifact it declares, so this reads the bytes the profile produced
/// without re-hashing them and without a virtualization pass.
fn artifact_plan_digests(path: &Path, expected_profile: &str) -> BTreeMap<String, String> {
    let source =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let plan: serde_json::Value = serde_json::from_str(&source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    assert_eq!(
        plan.get("profile").and_then(serde_json::Value::as_str),
        Some(expected_profile),
        "{} is not the {expected_profile} artifact plan; comparing a profile against itself \
         proves nothing, which is exactly how this defect stayed invisible (wamn-10yt.61)",
        path.display()
    );
    let artifacts = plan
        .pointer("/virtualization/artifacts")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("{} carries no virtualization artifacts", path.display()));
    let mut digests = BTreeMap::new();
    for artifact in artifacts {
        let package = artifact
            .get("package")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("{} has an artifact with no package", path.display()));
        let digest = artifact
            .get("sha256")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("{} has no sha256 for {package}", path.display()));
        assert!(
            digests
                .insert(package.to_owned(), digest.to_owned())
                .is_none(),
            "{} declares {package} twice",
            path.display()
        );
    }
    digests
}

/// One commit must produce the same guest bytes for the application and all selections.
///
/// `[GUEST-DIGEST-REPRODUCIBILITY]` in `docs/operations/build-and-test.md` builds
/// the same tree with both selections and supplies their artifact plans.
/// The application selection names declared guests. The all selection includes
/// every workspace guest. Each guest uses its own Cargo invocation to keep its
/// features independent of other selected packages.
/// This test compares shared packages and refuses missing artifacts from the all selection.
#[test]
#[ignore = "requires one commit built under both profiles by [GUEST-DIGEST-REPRODUCIBILITY]"]
fn one_commit_built_under_two_profiles_yields_identical_guest_digests() {
    let app_plan = std::env::var(PROFILE_APP_PLAN_ENV)
        .unwrap_or_else(|_| panic!("{PROFILE_APP_PLAN_ENV} must name the app artifact plan"));
    let all_plan = std::env::var(PROFILE_ALL_PLAN_ENV)
        .unwrap_or_else(|_| panic!("{PROFILE_ALL_PLAN_ENV} must name the all artifact plan"));
    assert_ne!(
        app_plan, all_plan,
        "the two artifact plans must come from different profile builds"
    );

    let app = artifact_plan_digests(Path::new(&app_plan), "app");
    let all = artifact_plan_digests(Path::new(&all_plan), "all");
    assert!(
        !app.is_empty(),
        "{app_plan} declares no artifacts to compare"
    );

    let mut drifted = Vec::new();
    for (package, digest) in &app {
        let Some(other) = all.get(package) else {
            panic!(
                "the all selection does not include {package}, which app does; the all \
                 selection is meant to be the wider one"
            );
        };
        if other != digest {
            drifted.push(format!("{package}: app {digest} != all {other}"));
        }
    }
    assert!(
        drifted.is_empty(),
        "the same commit produced different guest bytes under the app and all component \
         profiles, so a component digest is a claim about WHICH PROFILE built it and a pin \
         minted under one profile cannot be reproduced under the other (wamn-10yt.61). The \
         cause is a feature the two selections resolve differently: diff the `features` field \
         of the two target directories' `.fingerprint/*/lib-*.json` to name the crate, then \
         find which requester adds it, which may be a dependency's own manifest and not \
         anything this repository declares: {drifted:#?}"
    );
}

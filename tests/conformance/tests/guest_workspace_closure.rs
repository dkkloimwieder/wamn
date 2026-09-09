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
//! A THIRD axis is not the checkout at all: the same commit in the same
//! directory produced different bytes under the `m1` and `proof` component
//! profiles, because the two profiles select different `-p` sets and Cargo
//! unifies features per invocation (`wamn-10yt.61`). Nothing here could see it,
//! because the two-checkout arm builds `m1` on both sides. The cross-profile
//! arm below is the one that can.

use std::collections::BTreeMap;
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

/// Artifact plans printed by `tools/build-components build-only <profile>`.
const PROFILE_M1_PLAN_ENV: &str = "WAMN_DIGEST_PROFILE_M1_PLAN";
const PROFILE_PROOF_PLAN_ENV: &str = "WAMN_DIGEST_PROFILE_PROOF_PLAN";

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

/// THE PROPERTY THE FEATURE GOVERNANCE EXISTS FOR: one commit, two profiles, one digest.
///
/// Armed by `[GUEST-DIGEST-REPRODUCIBILITY]` in `docs/operations/build-and-test.md`,
/// which builds the same tree under both component profiles into separate target
/// directories and points this test at both artifact plans.
///
/// A component profile selects which packages one `cargo build` compiles. Cargo
/// unifies features across everything in that one invocation, so a package the
/// `proof` profile adds can turn on a feature in a crate the `m1` guests already
/// link, and the resolved feature NAME LIST goes into `-C metadata` even when
/// the feature itself compiles to nothing. That is a digest that depends on the
/// PROFILE, and a pin minted under one profile is then unmintable under the
/// other.
///
/// Measured at `wamn-10yt.61`, all four virtualized artifacts move. The carrier
/// is `sqlx-core`, which the `proof` selection compiles and `m1` does not: its
/// manifest asks for `futures-util` with `io`, `io` pulls `memchr`, and
/// `serde_json` links `memchr` — so `blob-put` moves without going near sqlx.
///
/// The `m1` selection is a subset of `proof`'s, so the shared packages are what
/// this compares; a package `m1` declares and `proof` does not is itself a
/// failure, because `proof` is meant to be the wider net.
#[test]
#[ignore = "requires one commit built under both profiles by [GUEST-DIGEST-REPRODUCIBILITY]"]
fn one_commit_built_under_two_profiles_yields_identical_guest_digests() {
    let m1_plan = std::env::var(PROFILE_M1_PLAN_ENV)
        .unwrap_or_else(|_| panic!("{PROFILE_M1_PLAN_ENV} must name the m1 artifact plan"));
    let proof_plan = std::env::var(PROFILE_PROOF_PLAN_ENV)
        .unwrap_or_else(|_| panic!("{PROFILE_PROOF_PLAN_ENV} must name the proof artifact plan"));
    assert_ne!(
        m1_plan, proof_plan,
        "the two artifact plans must come from different profile builds"
    );

    let m1 = artifact_plan_digests(Path::new(&m1_plan), "m1");
    let proof = artifact_plan_digests(Path::new(&proof_plan), "proof");
    assert!(
        !m1.is_empty(),
        "{m1_plan} declares no artifacts, so this proves nothing"
    );

    let mut drifted = Vec::new();
    for (package, digest) in &m1 {
        let Some(other) = proof.get(package) else {
            panic!(
                "the proof profile does not declare {package}, which m1 does; the proof \
                 selection is meant to be the wider one"
            );
        };
        if other != digest {
            drifted.push(format!("{package}: m1 {digest} != proof {other}"));
        }
    }
    assert!(
        drifted.is_empty(),
        "the same commit produced different guest bytes under the m1 and proof component \
         profiles, so a component digest is a claim about WHICH PROFILE built it and a pin \
         minted under one profile cannot be reproduced under the other (wamn-10yt.61). The \
         cause is a feature the two selections resolve differently: diff the `features` field \
         of the two target directories' `.fingerprint/*/lib-*.json` to name the crate, then \
         find which requester adds it, which may be a dependency's own manifest and not \
         anything this repository declares: {drifted:#?}"
    );
}

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
//! That is one of two costs, and it is the narrower one. Cargo reads EVERY
//! member manifest of a workspace before it builds one guest, so any escape,
//! of any dependency kind, makes the guest workspace unloadable on its own. In
//! `wamn-i9rg` that broke the Docker component stage, which was then given a
//! copy of the escaped tree to read. `wamn-98hz` measured the two apart: the
//! escape there was a dev-dependency, and the guest build compiles lib targets
//! only, so it reached no guest byte and no digest moved.
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
//! guests to check that the selection does not change their bytes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

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

/// The dependency table one declaration sits in, which decides what an escape
/// from it costs.
#[derive(Clone, Copy)]
enum DependencyKind {
    Normal,
    Build,
    Dev,
    /// `[workspace.dependencies]`, where the inheriting member decides the kind.
    Workspace,
}

/// The dependency table a header names, or `None` for any other table.
///
/// The keyword sits last in `[dependencies]`, `[build-dependencies]` and
/// `[target.'cfg(unix)'.dependencies]`, and ahead of the one crate it declares
/// in `[dev-dependencies.wamn-test-postgres]`, so this walks the segments
/// instead of reading either end.
fn dependency_kind(table: &str) -> Option<DependencyKind> {
    if table == "workspace.dependencies" || table.starts_with("workspace.dependencies.") {
        return Some(DependencyKind::Workspace);
    }
    let mut rest = table;
    loop {
        let (segment, tail) = match rest.split_once('.') {
            Some((segment, tail)) => (segment, Some(tail)),
            None => (rest, None),
        };
        match segment {
            "dependencies" => return Some(DependencyKind::Normal),
            "build-dependencies" => return Some(DependencyKind::Build),
            "dev-dependencies" => return Some(DependencyKind::Dev),
            _ => rest = tail?,
        }
    }
}

/// The canonical spelling of the table a kind names.
fn table_name(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Normal => "dependencies",
        DependencyKind::Build => "build-dependencies",
        DependencyKind::Dev => "dev-dependencies",
        DependencyKind::Workspace => "workspace.dependencies",
    }
}

/// What an escape from this table costs.
///
/// One cost applies to every kind. Cargo reads every member manifest before it
/// builds one guest, so the escaped directory has to be present wherever a guest
/// is built. The digest cost is narrower: it needs the escaped crate to be
/// COMPILED into a guest, and `tools/build-components` builds lib targets only,
/// under resolver 2, so a dev-dependency reaches no guest byte. `wamn-98hz`
/// measured that: built at two absolute roots, `wamn-event-reg` kept
/// `-C metadata=cddb1d4d2d3d1541` and a byte-identical rlib, and the escaped
/// dev-dependency was never compiled in either build.
fn cost(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Dev => {
            "ONE COST APPLIES HERE. The guest workspace cannot load on its own, because Cargo \
             reads every member manifest before it builds one guest, so the escaped directory \
             has to be present wherever a guest is built (wamn-i9rg). The digest channel of \
             wamn-10yt.10.29 does NOT apply to a dev-dependency: the guest build compiles lib \
             targets only, under resolver 2, so this crate reaches no guest byte (wamn-98hz). \
             Move the test that needs it into the root workspace."
        }
        DependencyKind::Workspace => {
            "BOTH COSTS ARE IN PLAY, and the members that inherit this entry decide which. \
             Whatever inherits it: the guest workspace cannot load on its own, because Cargo \
             reads every member manifest before it builds one guest, so the escaped directory \
             has to be present wherever a guest is built (wamn-i9rg). Where a member inherits \
             it into [dependencies] or [build-dependencies]: the escaped crate's absolute path \
             enters its -C metadata disambiguator and so its compiled bytes, which makes every \
             component digest a claim about the build directory rather than about the source \
             (wamn-10yt.10.29)."
        }
        DependencyKind::Normal | DependencyKind::Build => {
            "TWO COSTS APPLY HERE. The guest workspace cannot load on its own, because Cargo \
             reads every member manifest before it builds one guest, so the escaped directory \
             has to be present wherever a guest is built (wamn-i9rg). And the escaped crate's \
             absolute path enters its -C metadata disambiguator and so its compiled bytes, \
             which makes every component digest a claim about the build directory rather than \
             about the source (wamn-10yt.10.29). Move the crate under the workspace instead of \
             reaching out to it."
        }
    }
}

/// `path = "..."` values declared in a dependency table of one manifest, with
/// their line and the table they sit in.
///
/// Only dependency tables are read. A `[[bin]]` or `[lib]` path is a source file
/// of the declaring package, not a reach into another one.
fn declared_dependency_paths(source: &str) -> Vec<(usize, String, DependencyKind)> {
    let mut found = Vec::new();
    let mut table = None;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            table = dependency_kind(trimmed.trim_matches(['[', ']']));
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        let Some(kind) = table else { continue };
        let mut rest = trimmed;
        while let Some(at) = rest.find("path = \"") {
            let tail = &rest[at + "path = \"".len()..];
            let Some(end) = tail.find('"') else { break };
            found.push((index + 1, tail[..end].to_string(), kind));
            rest = &tail[end..];
        }
    }
    found
}

/// Whether a `path` declared in `manifest_directory` lands outside `workspace_root`.
///
/// Both directories are relative to the repository root, so a `..` that walks
/// off the top of the tree fails to pop and is an escape. The resolution is
/// lexical: `../label-template` from one member of a workspace names a sibling
/// member and is not an escape, which a leading-`..` test alone cannot tell.
fn leaves_workspace(workspace_root: &Path, manifest_directory: &Path, declared: &str) -> bool {
    let mut resolved = manifest_directory.to_path_buf();
    for component in Path::new(declared).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !resolved.pop() {
                    return true;
                }
            }
            Component::Normal(part) => resolved.push(part),
            // A root or a prefix makes the path absolute, which is always outside.
            Component::RootDir | Component::Prefix(_) => return true,
        }
    }
    !resolved.starts_with(workspace_root)
}

/// The member directories one workspace root manifest declares.
///
/// The value is a literal array in both guest workspaces, so this reads the
/// quoted entries between its brackets. An `exclude`d directory is not a member,
/// so it is already left out, and it belongs to another workspace.
fn declared_members(manifest: &str, source: &str) -> Vec<String> {
    let at = source
        .find("members = [")
        .unwrap_or_else(|| panic!("{manifest} declares no workspace members"));
    let tail = &source[at + "members = [".len()..];
    let end = tail
        .find(']')
        .unwrap_or_else(|| panic!("{manifest} has an unterminated members array"));
    let mut members = Vec::new();
    let mut rest = &tail[..end];
    while let Some(open) = rest.find('"') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('"') else { break };
        let entry = &after[..close];
        assert!(
            !entry.contains('*'),
            "{manifest} declares the glob member {entry:?}. This gate expands no globs, so a \
             member matched only by a glob goes unscanned, which is the hole wamn-gn57 closed"
        );
        members.push(entry.to_string());
        rest = &after[close + 1..];
    }
    members
}

/// Every manifest Cargo reads to load one guest workspace: its root and each member.
fn workspace_manifests(workspace: &str) -> Vec<String> {
    let root = Path::new(workspace)
        .parent()
        .expect("a workspace manifest sits in a directory");
    let mut manifests = vec![workspace.to_string()];
    for member in declared_members(workspace, &read(workspace)) {
        let manifest = root.join(&member).join("Cargo.toml");
        manifests.push(
            manifest
                .to_str()
                .expect("a member manifest path is UTF-8")
                .to_string(),
        );
    }
    manifests
}

/// Member manifests are read too, not just workspace roots.
///
/// A path escape written straight into a member manifest was invisible to this
/// gate for a day, and went red only when a different gate pushed the same line
/// up into the workspace root (`wamn-gn57`). An escape is now caught in the
/// manifest that declares it.
#[test]
fn no_guest_workspace_declares_a_dependency_outside_itself() {
    let mut escapes = Vec::new();
    for workspace in GUEST_WORKSPACES {
        let root = Path::new(workspace)
            .parent()
            .expect("a workspace manifest sits in a directory");
        for manifest in workspace_manifests(workspace) {
            let directory = Path::new(&manifest)
                .parent()
                .expect("a manifest sits in a directory")
                .to_path_buf();
            for (line, declared, kind) in declared_dependency_paths(&read(&manifest)) {
                if leaves_workspace(root, &directory, &declared) {
                    escapes.push(format!(
                        "{manifest}:{line}: [{}] path = {declared:?}\n    {}",
                        table_name(kind),
                        cost(kind)
                    ));
                }
            }
        }
    }
    assert!(
        escapes.is_empty(),
        "a guest workspace declares a dependency outside itself. Each escape carries the cost \
         that applies to the table it sits in:\n{}",
        escapes.join("\n")
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
#[ignore = "requires: WAMN_DIGEST_REPRO_A, WAMN_DIGEST_REPRO_B"]
fn one_commit_built_in_two_checkouts_yields_identical_guest_digests() {
    wamn_test_postgres::require_prerequisites(&["WAMN_DIGEST_REPRO_A", "WAMN_DIGEST_REPRO_B"]);
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
         does not test both profiles, which is exactly how this defect stayed invisible (wamn-10yt.61)",
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
/// `docs/operations/building.md#guest-artifact-comparisons` builds
/// the same tree with both selections and supplies their artifact plans.
/// The application selection names declared guests. The all selection includes
/// every workspace guest. Each guest uses its own Cargo invocation to keep its
/// features independent of other selected packages.
/// This test compares shared packages and refuses missing artifacts from the all selection.
#[test]
#[ignore = "requires: WAMN_DIGEST_PROFILE_APP_PLAN, WAMN_DIGEST_PROFILE_ALL_PLAN"]
fn one_commit_built_under_two_profiles_yields_identical_guest_digests() {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_DIGEST_PROFILE_APP_PLAN",
        "WAMN_DIGEST_PROFILE_ALL_PLAN",
    ]);
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

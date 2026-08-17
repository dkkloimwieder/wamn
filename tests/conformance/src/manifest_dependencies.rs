//! Guards workspace-owned dependency identities against package-local duplication.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The whitespace-stripped spelling of a dependency `path` that points at the
/// declaring crate's own directory. Cargo has no other notation for it, and the
/// closing quote is part of the literal so that `path = "./x"` and
/// `path = "../x"` do not match.
const OWN_DIRECTORY: &str = "path=\".\"";

/// The self dev-dependency of record, from `wamn-0h0g.15.104`.
const SELF_DEV_DEPENDENCY: &str = "wamn-catalog = { path = \".\", features = [\"test-util\"] }";

fn workspace_dependency_names(workspace_manifest: &Path) -> HashSet<String> {
    let source = std::fs::read_to_string(workspace_manifest).expect("read workspace Cargo.toml");
    let mut names = HashSet::new();
    let mut workspace_dependencies = false;

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            workspace_dependencies = trimmed == "[workspace.dependencies]";
            continue;
        }
        if !workspace_dependencies || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((raw_name, _)) = trimmed.split_once('=') {
            names.insert(raw_name.trim().trim_matches('"').to_string());
        }
    }

    names
}

fn workspace_members(workspace_manifest: &Path) -> Vec<(String, PathBuf)> {
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(workspace_manifest)
        .output()
        .expect("run cargo metadata for dependency-identity guard");
    assert!(
        output.status.success(),
        "cargo metadata failed for {}:\n{}",
        workspace_manifest.display(),
        String::from_utf8_lossy(&output.stderr)
    );

    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse cargo metadata");
    let members: HashSet<&str> = metadata["workspace_members"]
        .as_array()
        .expect("workspace_members array")
        .iter()
        .map(|member| member.as_str().expect("workspace member id"))
        .collect();

    metadata["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .filter(|package| {
            members.contains(
                package["id"]
                    .as_str()
                    .expect("package id in cargo metadata"),
            )
        })
        .map(|package| {
            (
                package["name"]
                    .as_str()
                    .expect("package name in cargo metadata")
                    .to_string(),
                PathBuf::from(
                    package["manifest_path"]
                        .as_str()
                        .expect("package manifest_path in cargo metadata"),
                ),
            )
        })
        .collect()
}

fn identity_violations(
    manifest: &Path,
    source: &str,
    package_name: &str,
    governed_names: &HashSet<String>,
    workspace_name: &str,
) -> Vec<String> {
    let mut violations = Vec::new();
    let mut dependency_table = false;
    let mut dev_dependency_table = false;

    for (line_index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let table = &trimmed[1..trimmed.len() - 1];
            let kind = table.rsplit_once('.').map_or(table, |(_, tail)| tail);
            dependency_table = matches!(
                kind,
                "dependencies" | "dev-dependencies" | "build-dependencies"
            );
            dev_dependency_table = kind == "dev-dependencies";
            continue;
        }
        if !dependency_table || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some((raw_name, declaration)) = trimmed.split_once('=') else {
            continue;
        };
        let name = raw_name.trim().trim_matches('"');
        let compact: String = declaration.split_whitespace().collect();
        let governed = name.starts_with("wamn-")
            || governed_names.contains(name)
            || compact.contains("package=\"wamn-");
        if !governed {
            continue;
        }

        // `wamn-0h0g.15.104`: a DEV-dependency on this same package by its own
        // directory is a governed construction, not drift. A crate's `tests/`
        // directory is a separate compilation unit, so under resolver 2 it can
        // only see its parent's `test-util` feature if the crate dev-depends on
        // itself by path — which is what keeps the M-TEST-UTIL fence intact for
        // every crate downstream, since a dev-dependency feature never reaches
        // the production graph. It has no workspace identity to inherit: the
        // workspace entry is what it would be a copy of.
        //
        // This is not a loophole, and all three conditions are load-bearing.
        // Promoting the declaration to a normal or build dependency, naming any
        // other crate, or pointing at any other path is ordinary duplication
        // and still fails below.
        if dev_dependency_table && name == package_name && compact.contains(OWN_DIRECTORY) {
            continue;
        }

        if !compact.contains("workspace=true") {
            violations.push(format!(
                "{}:{}: `{name}` must inherit its {workspace_name} workspace identity",
                manifest.display(),
                line_index + 1
            ));
        }
    }

    violations
}

fn assert_workspace_inheritance(workspace_manifest: &Path, workspace_name: &str) {
    let governed_names = workspace_dependency_names(workspace_manifest);
    let mut violations = Vec::new();
    for (package_name, manifest) in workspace_members(workspace_manifest) {
        let source = std::fs::read_to_string(&manifest).expect("read member Cargo.toml");
        violations.extend(identity_violations(
            &manifest,
            &source,
            &package_name,
            &governed_names,
            workspace_name,
        ));
    }

    assert!(
        violations.is_empty(),
        "duplicated governed dependency identities:\n{}",
        violations.join("\n")
    );
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tests/conformance lives two levels below the repository root")
        .to_path_buf()
}

/// Scans a single-table fixture as if it were a member of `wamn-catalog`. Every
/// identity the exemption concerns is `wamn-` prefixed, so an empty governed set
/// still reaches the identity rule and the verdict cannot be an artifact of the
/// workspace's own dependency list.
fn fixture_violations(table: &str, entry: &str) -> Vec<String> {
    identity_violations(
        Path::new("fixture/Cargo.toml"),
        &format!("[{table}]\n{entry}\n"),
        "wamn-catalog",
        &HashSet::new(),
        "native",
    )
}

#[test]
fn governed_dependency_identities_are_workspace_owned() {
    let repository = repository_root();

    assert_workspace_inheritance(&repository.join("Cargo.toml"), "native");
    assert_workspace_inheritance(&repository.join("components/Cargo.toml"), "component");
}

#[test]
fn the_catalog_self_dev_dependency_is_still_the_construction_this_guard_exempts() {
    let manifest = repository_root().join("crates/catalog/model/Cargo.toml");
    let source = std::fs::read_to_string(&manifest).expect("read wamn-catalog Cargo.toml");
    assert!(
        source.contains(SELF_DEV_DEPENDENCY),
        "wamn-catalog no longer declares `{SELF_DEV_DEPENDENCY}`, so the M-TEST-UTIL fence \
         `wamn-0h0g.15.104` installed is either gone or spelled some other way"
    );

    let violations = identity_violations(
        &manifest,
        &source,
        "wamn-catalog",
        &HashSet::new(),
        "native",
    );
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn the_self_dev_dependency_exemption_admits_nothing_else() {
    assert!(
        fixture_violations("dev-dependencies", SELF_DEV_DEPENDENCY).is_empty(),
        "the governed construction itself must be admitted"
    );

    let mutants = [
        // Promoted onto a normal dependency: the one change `wamn-0h0g.15.104`
        // forbids outright, because it deletes the fence for every crate
        // downstream of wamn-catalog.
        ("dependencies", SELF_DEV_DEPENDENCY),
        // No build script needs a crate's own test-only surface either.
        ("build-dependencies", SELF_DEV_DEPENDENCY),
        // Another governed crate, reached through the declaring crate's own
        // directory: a path that cannot resolve to it, so nothing about the
        // separate `tests/` compilation unit excuses it.
        (
            "dev-dependencies",
            "wamn-flow = { path = \".\", features = [\"test-util\"] }",
        ),
        // This package, but by a path that is not its own directory.
        ("dev-dependencies", "wamn-catalog = { path = \"../model\" }"),
        // Plain duplication of a workspace-owned identity.
        (
            "dev-dependencies",
            "wamn-flow = { path = \"../execution/flow-model\", version = \"0.1.0\" }",
        ),
    ];

    for (table, entry) in mutants {
        let violations = fixture_violations(table, entry);
        assert_eq!(
            violations.len(),
            1,
            "`{entry}` under `[{table}]` must be refused"
        );
        assert!(
            violations[0].ends_with("must inherit its native workspace identity"),
            "`{entry}` under `[{table}]` was refused for the wrong reason: {}",
            violations[0]
        );
    }
}

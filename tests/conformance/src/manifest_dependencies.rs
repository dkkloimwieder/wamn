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

/// One dependency declaration, in either spelling Cargo allows: an inline
/// `name = { .. }` entry, or a dotted single-dependency table whose header names
/// the crate and whose keys arrive on the lines below it.
struct Declaration<'a> {
    line: usize,
    name: &'a str,
    /// The declaration's keys with all whitespace stripped, so that
    /// `workspace = true` and `path = "."` are matched the same way in both
    /// spellings.
    keys: String,
    dev: bool,
}

/// Splits a table header into the dependency-table keyword it carries and, for
/// the single-dependency form, the crate its remaining segments name.
///
/// A dependency table is spelled either with the keyword last (`[dependencies]`,
/// `[workspace.dependencies]`, `[target.'cfg(unix)'.dependencies]`) or with the
/// keyword ahead of the one crate it declares, as in
/// `[dev-dependencies.wamn-catalog]`. Reading only the last segment mistook that
/// crate name for the table kind, so every key of a dotted table escaped the
/// identity rule (`wamn-0h0g.15.115`).
fn dependency_table_kind(table: &str) -> Option<(&str, Option<&str>)> {
    let mut rest = table;
    loop {
        let (segment, tail) = match rest.split_once('.') {
            Some((segment, tail)) => (segment, Some(tail)),
            None => (rest, None),
        };
        if matches!(
            segment,
            "dependencies" | "dev-dependencies" | "build-dependencies"
        ) {
            return Some((segment, tail));
        }
        rest = tail?;
    }
}

fn dependency_declarations(source: &str) -> Vec<Declaration<'_>> {
    let mut declarations = Vec::new();
    let mut dependency_table = false;
    let mut dev_dependency_table = false;
    let mut dotted: Option<Declaration<'_>> = None;

    for (line_index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            declarations.extend(dotted.take());
            let kind = dependency_table_kind(&trimmed[1..trimmed.len() - 1]);
            dependency_table = kind.is_some();
            dev_dependency_table = matches!(kind, Some(("dev-dependencies", _)));
            if let Some((_, Some(name))) = kind {
                dotted = Some(Declaration {
                    line: line_index + 1,
                    name,
                    keys: String::new(),
                    dev: dev_dependency_table,
                });
            }
            continue;
        }
        if !dependency_table || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // A dotted table names its crate in the header, so the lines below it are
        // that one declaration's keys rather than declarations of their own.
        if let Some(declaration) = dotted.as_mut() {
            declaration.keys.extend(trimmed.split_whitespace());
            continue;
        }

        let Some((raw_name, value)) = trimmed.split_once('=') else {
            continue;
        };
        declarations.push(Declaration {
            line: line_index + 1,
            name: raw_name.trim().trim_matches('"'),
            keys: value.split_whitespace().collect(),
            dev: dev_dependency_table,
        });
    }
    declarations.extend(dotted);

    declarations
}

/// Whether one declaration duplicates a governed identity that it should instead
/// inherit from the workspace.
fn duplicates_governed_identity(
    declaration: &Declaration<'_>,
    package_name: &str,
    governed_names: &HashSet<String>,
) -> bool {
    let governed = declaration.name.starts_with("wamn-")
        || governed_names.contains(declaration.name)
        || declaration.keys.contains("package=\"wamn-");
    if !governed {
        return false;
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
    if declaration.dev
        && declaration.name == package_name
        && declaration.keys.contains(OWN_DIRECTORY)
    {
        return false;
    }

    !declaration.keys.contains("workspace=true")
}

fn identity_violations(
    manifest: &Path,
    source: &str,
    package_name: &str,
    governed_names: &HashSet<String>,
    workspace_name: &str,
) -> Vec<String> {
    let mut violations = Vec::new();
    for declaration in dependency_declarations(source) {
        if duplicates_governed_identity(&declaration, package_name, governed_names) {
            violations.push(format!(
                "{}:{}: `{}` must inherit its {workspace_name} workspace identity",
                manifest.display(),
                declaration.line,
                declaration.name
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

#[test]
fn a_dotted_single_dependency_table_is_scanned_by_the_identity_rule() {
    // The keyword leads these headers, so reading only the last segment took
    // `wamn-flow` for the table kind and every key below it escaped the rule.
    for table in [
        "dependencies.wamn-flow",
        "target.'cfg(unix)'.dependencies.wamn-flow",
    ] {
        let refused = fixture_violations(table, "path = \"../flow-model\"");
        assert_eq!(
            refused.len(),
            1,
            "`[{table}]` must be judged as the dependency its header names"
        );
        assert!(
            refused[0].starts_with("fixture/Cargo.toml:1:"),
            "`[{table}]` must be reported at the header naming its crate: {}",
            refused[0]
        );
        assert!(
            refused[0].ends_with("`wamn-flow` must inherit its native workspace identity"),
            "`[{table}]` was refused for the wrong reason: {}",
            refused[0]
        );
    }

    // Inheritance is spelled inside the table in this form, and it counts
    // wherever it appears among the table's lines.
    let admitted = fixture_violations(
        "dependencies.wamn-flow",
        "default-features = false\nworkspace = true",
    );
    assert!(
        admitted.is_empty(),
        "an inheriting dotted table must be admitted: {}",
        admitted.join("\n")
    );

    // The target-cfg spelling keeps its meaning, and no manifest in the tree
    // carries one, so only a fixture holds it: there the entry line is judged,
    // not the header.
    let entry = "wamn-flow = { path = \"../flow-model\" }";
    let targeted = fixture_violations("target.'cfg(unix)'.dependencies", entry);
    assert_eq!(
        targeted.len(),
        1,
        "a target-cfg dependency table must still reach the identity rule"
    );
    assert!(
        targeted[0].starts_with("fixture/Cargo.toml:2:"),
        "an inline entry is reported at its own line: {}",
        targeted[0]
    );
}

#[test]
fn the_dotted_spelling_does_not_widen_the_self_dev_dependency_exemption() {
    // One exemption, two spellings: the dotted form is judged by the same three
    // conjuncts and must not become a second, looser one.
    let admitted = fixture_violations("dev-dependencies.wamn-catalog", "path = \".\"");
    assert!(
        admitted.is_empty(),
        "the governed construction must be admitted in either spelling: {}",
        admitted.join("\n")
    );

    let mutants = [
        // Not a DEV table.
        ("dependencies.wamn-catalog", "path = \".\""),
        // Not this package.
        ("dev-dependencies.wamn-flow", "path = \".\""),
        // Not this package's own directory.
        ("dev-dependencies.wamn-catalog", "path = \"../model\""),
    ];

    for (table, entry) in mutants {
        let violations = fixture_violations(table, entry);
        assert_eq!(
            violations.len(),
            1,
            "`[{table}]` with `{entry}` must be refused"
        );
        assert!(
            violations[0].ends_with("must inherit its native workspace identity"),
            "`[{table}]` with `{entry}` was refused for the wrong reason: {}",
            violations[0]
        );
    }
}

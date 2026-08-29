//! Guards the workspace-owned Wasmtime and async-nats type universes.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
// Two DIFFERENT facts that a patch release pulls apart, so they cannot share a
// constant. `RESOLVED` is what the lockfile actually resolves the family to;
// `REQUIREMENT` is the caret requirement the workspace manifest declares, which
// upstream wasmCloud owns and which a patch bump must NOT move (wamn-ufjr is
// lockfile-only). Before 47.0.4 they were equal, which is why one constant
// served both and hid the distinction.
const WASMTIME_RESOLVED: &str = "47.0.4";
const WASMTIME_REQUIREMENT: &str = "47.0.3";
const ASYNC_NATS_VERSION: &str = "0.49.1";

const DIRECT_CONSUMERS: [(&str, &[(&str, &str)]); 4] = [
    // execution/host takes `wasmtime-wasi` ONLY. It carried `wasmtime-wasi-http`
    // once and this table was not repointed when the dependency was dropped, so
    // the guard demanded a dep the crate does not declare and no source under
    // it references. Re-adding the dependency to satisfy this row would declare
    // a dead dep to silence a false assertion (wamn-0h0g.15.191).
    (
        "crates/execution/host/Cargo.toml",
        &[("wasmtime-wasi", "dependencies")],
    ),
    (
        "tests/conformance/Cargo.toml",
        &[("wasmtime-wasi", "dependencies")],
    ),
    (
        "crates/platform/runtime/Cargo.toml",
        &[
            ("wasmtime-wasi", "dependencies"),
            ("wasmtime-wasi-http", "dependencies"),
        ],
    ),
    // The live router-tap proof instantiates the P2 HTTP host surface directly;
    // unlike the deleted system traceproof dependency, this is a source-backed
    // consumer (wamn-0h0g.24.5).
    (
        "tests/integration/Cargo.toml",
        &[
            ("wasmtime-wasi", "dependencies"),
            ("wasmtime-wasi-http", "dev-dependencies"),
        ],
    ),
    // `tests/system/Cargo.toml` held a fifth row until `wamn-k9ea`. traceproof
    // reached `wasmtime-wasi-http` only to drive wash-runtime's P2/P3 `wasi:http`
    // host surfaces; re-aiming that gate at `wamn:connection/http` deleted the
    // last reference, so the dependency went with it. Removed here for the same
    // reason `wamn-0h0g.15.191` gives above — a row kept alive by re-declaring a
    // dead dep asserts nothing.
];

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: Vec<String>,
    resolve: CargoResolve,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
    version: String,
    manifest_path: String,
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CargoResolve {
    nodes: Vec<CargoNode>,
}

#[derive(Debug, Deserialize)]
struct CargoNode {
    id: String,
    deps: Vec<CargoDependency>,
}

#[derive(Debug, Deserialize)]
struct CargoDependency {
    pkg: String,
}

#[derive(Debug)]
struct LockPackage {
    name: String,
    version: String,
    source: Option<String>,
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tests/conformance lives two levels below the repository root")
        .to_path_buf()
}

fn dependency_declarations(manifest: &Path, table_name: &str) -> BTreeMap<String, String> {
    let source = fs::read_to_string(manifest)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", manifest.display()));
    let mut in_table = false;
    let mut declarations = BTreeMap::new();

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_table = trimmed == format!("[{table_name}]");
            continue;
        }
        if !in_table || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((name, declaration)) = trimmed.split_once('=') {
            declarations.insert(
                name.trim().trim_matches('"').to_owned(),
                declaration.split_whitespace().collect(),
            );
        }
    }

    declarations
}

fn expected_direct_dependencies() -> BTreeMap<String, BTreeSet<String>> {
    DIRECT_CONSUMERS
        .iter()
        .map(|(manifest, dependencies)| {
            let package = dependency_declarations(&repository().join(manifest), "package")
                .get("name")
                .unwrap_or_else(|| panic!("{manifest} must declare package.name"))
                .trim_matches('"')
                .to_owned();
            let dependencies = dependencies
                .iter()
                .map(|(dependency, _)| (*dependency).to_owned())
                .collect();
            (package, dependencies)
        })
        .collect()
}

/// Resolved ONCE per test binary and shared.
///
/// `cargo metadata` takes the cargo PACKAGE CACHE LOCK. Three tests here need it
/// and `cargo test` runs them in parallel, so calling it per test made them
/// contend with each other -- and the failure text is `Blocking waiting for file
/// lock on package cache` followed by a panic, which is indistinguishable from a
/// real assertion failure. One `OnceLock` collapses three invocations into one
/// (wamn-0h0g.15.191).
fn cargo_metadata(root: &Path) -> &'static CargoMetadata {
    static METADATA: OnceLock<CargoMetadata> = OnceLock::new();
    METADATA.get_or_init(|| cargo_metadata_uncached(root))
}

fn cargo_metadata_uncached(root: &Path) -> CargoMetadata {
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .output()
        .expect("run cargo metadata for Wasmtime source identity");
    assert!(
        output.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse cargo metadata")
}

fn lock_packages(lock_path: &Path) -> Vec<LockPackage> {
    let source = fs::read_to_string(lock_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", lock_path.display()));
    let mut packages = Vec::new();
    let mut name = None;
    let mut version = None;
    let mut package_source = None;

    for line in source.lines().chain(["[[package]]"]) {
        if line == "[[package]]" {
            if let Some(name) = name.take() {
                packages.push(LockPackage {
                    name,
                    version: version
                        .take()
                        .expect("Cargo.lock package must declare a version"),
                    source: package_source.take(),
                });
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("name = \"") {
            name = value.strip_suffix('"').map(str::to_owned);
        } else if let Some(value) = line.strip_prefix("version = \"") {
            version = value.strip_suffix('"').map(str::to_owned);
        } else if let Some(value) = line.strip_prefix("source = \"") {
            package_source = value.strip_suffix('"').map(str::to_owned);
        }
    }

    packages
}

fn assert_single_wasmtime_family<'a>(
    origin: &str,
    packages: impl Iterator<Item = (&'a str, &'a str, Option<&'a str>)>,
) {
    let family: Vec<_> = packages
        .filter(|(name, _, _)| name == &"wasmtime" || name.starts_with("wasmtime-"))
        .collect();
    assert!(
        !family.is_empty(),
        "{origin} has no resolved Wasmtime family"
    );

    let sources: BTreeSet<_> = family
        .iter()
        .map(|(name, _, source)| {
            source.unwrap_or_else(|| panic!("{origin} package {name} has no external source"))
        })
        .collect();
    assert_eq!(
        sources,
        BTreeSet::from([CRATES_IO_SOURCE]),
        "{origin} resolves multiple or non-canonical Wasmtime source identities"
    );

    let versions: BTreeSet<_> = family.iter().map(|(_, version, _)| *version).collect();
    assert_eq!(
        versions,
        BTreeSet::from([WASMTIME_RESOLVED]),
        "{origin} resolves multiple or non-canonical Wasmtime versions"
    );

    for required in [
        "wasmtime",
        "wasmtime-wasi",
        "wasmtime-wasi-http",
        "wasmtime-wasi-io",
    ] {
        let matches = family
            .iter()
            .filter(|(name, _, _)| *name == required)
            .count();
        assert_eq!(
            matches, 1,
            "{origin} must resolve exactly one `{required}` package"
        );
    }
}

fn assert_single_async_nats<'a>(
    origin: &str,
    packages: impl Iterator<Item = (&'a str, &'a str, Option<&'a str>)>,
) {
    let resolved: Vec<_> = packages
        .filter(|(name, _, _)| name == &"async-nats")
        .collect();
    assert_eq!(
        resolved.len(),
        1,
        "{origin} must resolve exactly one `async-nats` package"
    );
    let (_, version, source) = resolved[0];
    assert_eq!(
        version, ASYNC_NATS_VERSION,
        "{origin} must resolve async-nats {ASYNC_NATS_VERSION}"
    );
    assert_eq!(
        source,
        Some(CRATES_IO_SOURCE),
        "{origin} must resolve async-nats from crates.io"
    );
}

#[test]
fn direct_wasmtime_consumers_inherit_workspace_source_contract() {
    let root = repository();
    let workspace = dependency_declarations(&root.join("Cargo.toml"), "workspace.dependencies");

    for dependency in ["wasmtime-wasi", "wasmtime-wasi-http"] {
        let expected = format!("\"{WASMTIME_REQUIREMENT}\"");
        assert_eq!(
            workspace.get(dependency),
            Some(&expected),
            "workspace must own the canonical `{dependency}` registry version"
        );
    }

    for (manifest, dependencies) in DIRECT_CONSUMERS {
        for (dependency, table) in dependencies {
            let declarations = dependency_declarations(&root.join(manifest), table);
            assert_eq!(
                declarations.get(*dependency).map(String::as_str),
                Some("{workspace=true}"),
                "{manifest} [{table}] must inherit `{dependency}` from the workspace"
            );
        }
    }
}

#[test]
fn resolved_wasmtime_type_universe_is_single_and_canonical() {
    let root = repository();
    let metadata = cargo_metadata(&root);
    let packages_by_id: BTreeMap<_, _> = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect();
    let workspace_members: BTreeSet<_> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();

    assert_single_wasmtime_family(
        "cargo metadata",
        metadata.packages.iter().map(|package| {
            (
                package.name.as_str(),
                package.version.as_str(),
                package.source.as_deref(),
            )
        }),
    );
    assert_single_wasmtime_family(
        "Cargo.lock",
        lock_packages(&root.join("Cargo.lock"))
            .iter()
            .map(|package| {
                (
                    package.name.as_str(),
                    package.version.as_str(),
                    package.source.as_deref(),
                )
            }),
    );

    let mut actual_direct = BTreeMap::<String, BTreeSet<String>>::new();
    for node in &metadata.resolve.nodes {
        if !workspace_members.contains(node.id.as_str()) {
            continue;
        }
        let consumer = packages_by_id
            .get(node.id.as_str())
            .unwrap_or_else(|| panic!("metadata omits workspace package {}", node.id));
        let dependencies: BTreeSet<_> = node
            .deps
            .iter()
            .filter_map(|dependency| {
                let package = packages_by_id
                    .get(dependency.pkg.as_str())
                    .unwrap_or_else(|| {
                        panic!("metadata omits resolved package {}", dependency.pkg)
                    });
                matches!(
                    package.name.as_str(),
                    "wasmtime-wasi" | "wasmtime-wasi-http"
                )
                .then(|| package.name.clone())
            })
            .collect();
        if !dependencies.is_empty() {
            assert!(
                Path::new(&consumer.manifest_path).starts_with(&root),
                "direct Wasmtime consumer {} is outside the workspace",
                consumer.name
            );
            actual_direct.insert(consumer.name.clone(), dependencies);
        }
    }

    assert_eq!(
        actual_direct,
        expected_direct_dependencies(),
        "retained direct Wasmtime consumers or their required host interfaces drifted"
    );
}

#[test]
fn resolved_async_nats_universe_is_single_and_canonical() {
    let root = repository();
    let workspace = dependency_declarations(&root.join("Cargo.toml"), "workspace.dependencies");
    let expected = format!("{{version=\"{ASYNC_NATS_VERSION}\",default-features=false}}");
    assert_eq!(
        workspace.get("async-nats").map(String::as_str),
        Some(expected.as_str()),
        "workspace must own the canonical async-nats registry version"
    );

    let metadata = cargo_metadata(&root);
    assert_single_async_nats(
        "cargo metadata",
        metadata.packages.iter().map(|package| {
            (
                package.name.as_str(),
                package.version.as_str(),
                package.source.as_deref(),
            )
        }),
    );
    assert_single_async_nats(
        "Cargo.lock",
        lock_packages(&root.join("Cargo.lock"))
            .iter()
            .map(|package| {
                (
                    package.name.as_str(),
                    package.version.as_str(),
                    package.source.as_deref(),
                )
            }),
    );
}

//! Guards the workspace-owned Wasmtime and async-nats type universes.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
// The lockfile resolution and the workspace requirement are separate facts.
// Upstream 2.9.0 declares 47.0.4; a later lockfile-only patch can resolve higher.
const WASMTIME_RESOLVED: &str = "47.0.4";
const WASMTIME_REQUIREMENT: &str = "47.0.4";
const ASYNC_NATS_VERSION: &str = "0.49.1";

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    resolve: CargoResolve,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
    version: String,
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CargoResolve {
    nodes: Vec<CargoNode>,
}

#[derive(Debug, Deserialize)]
struct CargoNode {
    id: String,
    features: Vec<String>,
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
fn workspace_owns_canonical_wasmtime_source_contract() {
    let root = repository();
    let workspace = dependency_declarations(&root.join("Cargo.toml"), "workspace.dependencies");

    assert_eq!(
        workspace.get("wasmtime").map(String::as_str),
        Some(
            "{version=\"47.0.4\",default-features=false,features=[\"cache\",\"parallel-compilation\"]}"
        ),
        "workspace must own the cache-enabled canonical `wasmtime` requirement"
    );

    for dependency in ["wasmtime-wasi", "wasmtime-wasi-http"] {
        let expected = format!("\"{WASMTIME_REQUIREMENT}\"");
        assert_eq!(
            workspace.get(dependency),
            Some(&expected),
            "workspace must own the canonical `{dependency}` registry version"
        );
    }
}

#[test]
fn resolved_wasmtime_type_universe_is_single_and_canonical() {
    let root = repository();
    let metadata = cargo_metadata(&root);
    let wasmtime = metadata
        .packages
        .iter()
        .find(|package| package.name == "wasmtime")
        .expect("metadata must resolve the Wasmtime core package");
    let wasmtime_node = metadata
        .resolve
        .nodes
        .iter()
        .find(|node| node.id == wasmtime.id)
        .expect("metadata must resolve the Wasmtime core feature set");
    assert!(
        wasmtime_node
            .features
            .iter()
            .any(|feature| feature == "cache"),
        "the serving engine requires Wasmtime's persistent compilation cache"
    );

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

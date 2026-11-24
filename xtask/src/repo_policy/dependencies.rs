//! Dependency boundaries and identities in each workspace.

use super::Problems;
use anyhow::{Context as _, ensure};
use cargo_metadata::{DependencyKind, Metadata, PackageId};
use std::collections::{HashMap, hash_map::Entry};
use std::path::Path;

const WORKFLOW_GRAPH_REFUSED: &[&str] = &[
    "wamn-postgres",
    "wamn-provision",
    "wamn-kubernetes",
    "wamn-engine",
    "wamn-plugin",
    "wamn-http",
    "wamn-http-client",
    "wamn-identity-client",
    "wamn-broker",
    "wamn-oci",
    "wamn-build",
    "wamn-codegen",
];

const WITHOUT_POSTGRES: &[&str] = &["wamn-schema", "wamn-codegen", "wamn-build"];

const WITHOUT_WASM_RUNTIME: &[&str] = &["wamn-application", "wamn-postgres", "wamn-cdc-reader"];

const TEST_INFRASTRUCTURE: &str = "wamn-test-infrastructure";
const TEST_AND_TOOL_PACKAGES: &[&str] = &[
    TEST_INFRASTRUCTURE,
    "wamn-fixture-package",
    "wamn-grant-tests",
    "wamn-journey-tests",
    "wamn-receiving-tests",
    "wamn-wms-tests",
    "xtask",
    "connection-http-standard",
    "fixture",
    "fixture-overlay",
];

pub(crate) fn check_boundaries(root: &Path) -> anyhow::Result<()> {
    for manifest in ["Cargo.toml", "apps/Cargo.toml"] {
        check_workspace_boundaries(&root.join(manifest))?;
    }
    Ok(())
}

fn check_workspace_boundaries(manifest: &Path) -> anyhow::Result<()> {
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(manifest)
        .features(cargo_metadata::CargoOpt::AllFeatures)
        .other_options(vec!["--locked".into(), "--offline".into()])
        .exec()
        .with_context(|| format!("resolve dependencies from {}", manifest.display()))?;
    let members = metadata.workspace_packages();
    if let Some(workflow) = members
        .iter()
        .find(|package| package.name.as_str() == "wamn-workflow-graph")
    {
        check_paths(
            &metadata,
            [&workflow.id],
            |name| WORKFLOW_GRAPH_REFUSED.contains(&name),
            |edge| edge == DependencyKind::Normal,
        )?;
    }
    let normal_or_build = |edge| matches!(edge, DependencyKind::Normal | DependencyKind::Build);
    let without_postgres = members
        .iter()
        .filter(|package| WITHOUT_POSTGRES.contains(&package.name.as_str()))
        .map(|package| &package.id);
    check_paths(
        &metadata,
        without_postgres,
        |name| matches!(name, "wamn-postgres" | "postgres-protocol"),
        normal_or_build,
    )?;
    let without_runtime = members
        .iter()
        .filter(|package| WITHOUT_WASM_RUNTIME.contains(&package.name.as_str()))
        .map(|package| &package.id);
    check_paths(
        &metadata,
        without_runtime,
        wasm_runtime_refused,
        normal_or_build,
    )?;

    let products = members
        .into_iter()
        .filter(|package| !TEST_AND_TOOL_PACKAGES.contains(&package.name.as_str()))
        .map(|package| &package.id);
    check_paths(
        &metadata,
        products,
        |name| name == TEST_INFRASTRUCTURE,
        normal_or_build,
    )
}

fn wasm_runtime_refused(name: &str) -> bool {
    matches!(name, "wasmtime" | "wash-runtime") || name.starts_with("wasmtime-")
}

fn forbidden_dependency(path: &[&str]) -> anyhow::Error {
    anyhow::anyhow!(
        "{} links forbidden dependency {}: {}",
        path[0],
        path[path.len() - 1],
        path.join(" -> ")
    )
}

fn check_paths<'a>(
    metadata: &'a Metadata,
    starts: impl IntoIterator<Item = &'a PackageId>,
    refused: impl Fn(&str) -> bool,
    follows: impl Fn(DependencyKind) -> bool,
) -> anyhow::Result<()> {
    let resolve = metadata.resolve.as_ref().context("resolved dependencies")?;
    let nodes: HashMap<_, _> = resolve.nodes.iter().map(|node| (&node.id, node)).collect();
    let names: HashMap<_, _> = metadata
        .packages
        .iter()
        .map(|package| (&package.id, package.name.as_str()))
        .collect();
    let mut parents = HashMap::new();
    let mut pending = Vec::new();
    for start in starts {
        pending.push((start, None));
        while let Some((id, parent)) = pending.pop() {
            let Entry::Vacant(entry) = parents.entry(id) else {
                continue;
            };
            entry.insert(parent);
            let name = names[id];
            if refused(name) {
                let mut path: Vec<_> = std::iter::successors(Some(id), |id| parents[id])
                    .map(|id| names[id])
                    .collect();
                path.reverse();
                return Err(forbidden_dependency(&path));
            }
            pending.extend(nodes[id].deps.iter().rev().filter_map(|dep| {
                dep.dep_kinds
                    .iter()
                    .any(|edge| follows(edge.kind))
                    .then_some((&dep.pkg, Some(id)))
            }));
        }
    }
    Ok(())
}

pub(super) fn report_problems(root: &Path, problems: &mut Problems) {
    for manifest in ["Cargo.toml", "apps/Cargo.toml"] {
        if let Err(error) = workspace(&root.join(manifest), problems) {
            problems.push(format!("{manifest}: {error:#}"));
        }
    }
    if let Err(error) = runtime(root) {
        problems.push(format!("runtime dependencies: {error:#}"));
    }
}

fn workspace(manifest: &Path, problems: &mut Problems) -> anyhow::Result<()> {
    let source: toml::Value = toml::from_str(&std::fs::read_to_string(manifest)?)?;
    let governed = source["workspace"]["dependencies"]
        .as_table()
        .context("workspace dependencies")?;
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(manifest)
        .no_deps()
        .other_options(vec!["--locked".into(), "--offline".into()])
        .exec()?;
    for package in metadata.workspace_packages() {
        let source: toml::Value =
            toml::from_str(&std::fs::read_to_string(&package.manifest_path)?)?;
        declarations(
            &source,
            package.name.as_str(),
            governed,
            package.manifest_path.as_std_path(),
            problems,
        );
    }
    // The serving runtime requires the persistent compilation cache.
    if let Some(wasmtime) = governed.get("wasmtime") {
        ensure!(
            wasmtime["features"]
                .as_array()
                .is_some_and(|features| features
                    .iter()
                    .any(|feature| feature.as_str() == Some("cache"))),
            "Wasmtime requires the cache feature"
        );
    }
    Ok(())
}

fn declarations(
    value: &toml::Value,
    package: &str,
    governed: &toml::Table,
    manifest: &Path,
    problems: &mut Problems,
) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, value) in table {
        if matches!(
            key.as_str(),
            "dependencies" | "dev-dependencies" | "build-dependencies"
        ) {
            for (name, declaration) in value.as_table().into_iter().flatten() {
                let renamed = declaration
                    .get("package")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(name);
                if !(renamed.starts_with("wamn-") || governed.contains_key(name)) {
                    continue;
                }
                let own_test = key == "dev-dependencies"
                    && name == package
                    && declaration.get("path").and_then(toml::Value::as_str) == Some(".");
                problems.require(
                    own_test
                        || declaration.get("workspace").and_then(toml::Value::as_bool)
                            == Some(true),
                    || {
                        format!(
                            "{}: {name} must inherit its workspace identity",
                            manifest.display()
                        )
                    },
                );
            }
        } else {
            declarations(value, package, governed, manifest, problems);
        }
    }
}

// Cross-package version agreement and source identity are not cargo-deny bans.
fn runtime(root: &Path) -> anyhow::Result<()> {
    let source: toml::Value = toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml"))?)?;
    let dependencies = &source["workspace"]["dependencies"];
    let version = dependencies["wasmtime"]["version"]
        .as_str()
        .context("Wasmtime version")?
        .trim_start_matches('=');
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .other_options(vec!["--locked".into(), "--offline".into()])
        .exec()?;
    let runtime = metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == "wash-runtime")
        .context("wash-runtime dependency")?;
    let upstream = &dependencies["wash-runtime"];
    let upstream_source = format!(
        "git+{}?tag={}#",
        upstream["git"].as_str().context("runtime repository")?,
        upstream["tag"].as_str().context("runtime tag")?
    );
    ensure!(
        runtime
            .source
            .as_ref()
            .is_some_and(|source| source.repr.starts_with(&upstream_source)),
        "wash-runtime must resolve directly to its workspace release tag"
    );
    for package in metadata.packages.iter().filter(|package| {
        package.name.as_str() == "async-nats"
            || package.name.as_str() == "wasmtime"
            || package.name.as_str().starts_with("wasmtime-")
    }) {
        let version = if package.name.as_str() == "async-nats" {
            dependencies["async-nats"]["version"]
                .as_str()
                .context("async-nats version")?
                .trim_start_matches('=')
        } else {
            version
        };
        ensure!(
            package.version.to_string() == version
                && package
                    .source
                    .as_ref()
                    .is_some_and(cargo_metadata::Source::is_crates_io),
            "{} must use its workspace version from crates.io",
            package.name
        );
    }
    Ok(())
}

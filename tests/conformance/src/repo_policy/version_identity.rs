//! Every governed first-party version identity stays at the MVP `0.1` line,
//! and the tree holds one version of each WAMN WIT package.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use super::Problems;

const MVP_CARGO_VERSION: &str = "0.1.0";

pub(super) fn report_problems(root: &Path, problems: &mut Problems) {
    for manifest in [root.join("Cargo.toml"), root.join("apps/Cargo.toml")] {
        match cargo_metadata(&manifest) {
            Ok(metadata) => workspace_version_violations(&metadata, problems),
            Err(problem) => problems.push(problem),
        }
    }
    wamn_wit_packages_have_one_version(root, problems);
}

fn cargo_metadata(workspace_manifest: &Path) -> Result<Value, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
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
        .map_err(|error| format!("run cargo metadata for the MVP version lint: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed for {}:\n{}",
            workspace_manifest.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| format!("parse cargo metadata: {error}"))
}

fn workspace_version_violations(metadata: &Value, problems: &mut Problems) {
    let members: HashSet<&str> = metadata["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();

    for package in metadata["packages"].as_array().into_iter().flatten() {
        if !package["id"]
            .as_str()
            .is_some_and(|id| members.contains(id))
        {
            continue;
        }
        let version = package["version"].as_str().unwrap_or_default();
        problems.require(version == MVP_CARGO_VERSION, || {
            format!(
                "workspace package {} is version {version}, expected {MVP_CARGO_VERSION}",
                package["name"].as_str().unwrap_or_default()
            )
        });
    }
}

fn tracked_files(repository: &Path, patterns: &[&str]) -> Result<Vec<PathBuf>, String> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(repository)
        .args(["ls-files", "-z", "--"])
        .args(patterns)
        .output()
        .map_err(|error| format!("list tracked {patterns:?} files: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git ls-files failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .filter_map(|path| std::str::from_utf8(path).ok())
        .filter(|path| !Path::new(path).starts_with("docs/archive"))
        .filter(|path| repository.join(path).is_file())
        .map(|path| repository.join(path))
        .collect())
}

/// A WAMN WIT package keeps its own version (`docs/history/operation-ids.md`),
/// and the tree holds one version of it. Copies of that version may repeat.
/// A YAML manifest names a package on separate lines (`namespace: wamn`,
/// `package:`, `version:`), and those count too (`wamn-3vi3`).
fn wamn_wit_packages_have_one_version(root: &Path, problems: &mut Problems) {
    let files = match tracked_files(root, &["*.wit"]) {
        Ok(files) => files,
        Err(problem) => {
            problems.push(problem);
            return;
        }
    };
    let mut versions: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for path in files {
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                problems.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        for (line_index, line) in source.lines().enumerate() {
            let Some(declaration) = line.trim().strip_prefix("package ") else {
                continue;
            };
            let declaration = declaration
                .split_once(';')
                .map_or(declaration.trim(), |(package, _)| package.trim());
            let package_name = declaration
                .split_once('@')
                .map_or(declaration, |(name, _)| name);
            if !package_name.starts_with("wamn:") {
                continue;
            }

            let version = declaration
                .split_once('@')
                .map_or("", |(_, version)| version);
            versions
                .entry(package_name.to_owned())
                .or_default()
                .entry(version.to_owned())
                .or_insert_with(|| format!("{}:{}", path.display(), line_index + 1));
        }
    }
    problems.require(!versions.is_empty(), || {
        "WAMN WIT package list must not be empty".to_owned()
    });
    match tracked_files(root, &["*.yaml", "*.yml"]) {
        Ok(files) => {
            for path in files {
                match std::fs::read_to_string(&path) {
                    Ok(source) => yaml_wamn_packages(&path, &source, &mut versions),
                    Err(error) => problems.push(format!("{}: {error}", path.display())),
                }
            }
        }
        Err(problem) => problems.push(problem),
    }
    for (package, found) in &versions {
        problems.require(found.len() == 1, || {
            let found = found
                .iter()
                .map(|(version, place)| format!("@{version} at {place}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("WAMN WIT package `{package}` has more than one version in the tree: {found}")
        });
    }
}

/// One YAML list item that names a WIT package: its `namespace`, `package`
/// and `version` keys sit at one indentation, each on its own line.
#[derive(Default)]
struct YamlPackage {
    column: usize,
    line: usize,
    namespace: Option<String>,
    package: Option<String>,
    version: Option<String>,
}

fn yaml_wamn_packages(
    path: &Path,
    source: &str,
    versions: &mut BTreeMap<String, BTreeMap<String, String>>,
) {
    let mut record = |item: Option<YamlPackage>| {
        let Some(item) = item else { return };
        if let (Some("wamn"), Some(package), Some(version)) =
            (item.namespace.as_deref(), item.package, item.version)
        {
            versions
                .entry(format!("wamn:{package}"))
                .or_default()
                .entry(version)
                .or_insert_with(|| format!("{}:{}", path.display(), item.line));
        }
    };
    let mut current: Option<YamlPackage> = None;
    for (line_index, line) in source.lines().enumerate() {
        let indent = line.len() - line.trim_start().len();
        let mut text = line.trim_start();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let mut column = indent;
        let item_start = text.starts_with("- ");
        if item_start {
            text = text[2..].trim_start();
            column = line.len() - text.len();
        }
        if current
            .as_ref()
            .is_some_and(|item| column < item.column || (item_start && column == item.column))
        {
            record(current.take());
        }
        let Some((key, value)) = text.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        if !matches!(key, "namespace" | "package" | "version") {
            continue;
        }
        let item = current.get_or_insert_with(|| YamlPackage {
            column,
            line: line_index + 1,
            ..YamlPackage::default()
        });
        if item.column != column {
            continue;
        }
        let slot = match key {
            "namespace" => &mut item.namespace,
            "package" => &mut item.package,
            _ => &mut item.version,
        };
        *slot = Some(value.to_owned());
    }
    record(current);
}

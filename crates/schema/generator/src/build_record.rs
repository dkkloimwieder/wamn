//! Package identity, dependency pins, compiler identity and output digests.
use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use wamn_digest::canonical_json_bytes;
/// The completed build record.
pub const BUILD_RECORD_FILE: &str = "build.json";
/// The build record schema.
pub const BUILD_RECORD_SCHEMA: &str = "wamn.build/v1";
/// One package build: `build.json`, schema `wamn.build/v1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildRecord {
    /// Always [`BUILD_RECORD_SCHEMA`].
    pub schema: String,
    pub package: BuildPackage,
    /// The exact base packages, in the order of their dependency names.
    pub dependencies: Vec<BuildDependency>,
    pub generator: BuildGenerator,
    pub outputs: BuildOutputs,
}

/// The package that was built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildPackage {
    pub id: String,
    pub version: String,
}

/// One base package of an overlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildDependency {
    pub id: String,
    pub version: String,
    /// The digest of the base's canonical `wamn.json`, until a package
    /// artifact digest replaces it.
    pub digest: String,
}

/// The generator and toolchain identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildGenerator {
    pub id: String,
    pub toolchain: String,
    pub sqlx: String,
}

/// The build's outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildOutputs {
    /// The digest of the canonical `wamn.json`, hashed once.
    pub manifest: String,
    /// The `sha256:` digest of each output file, by its path relative to the
    /// package's build output directory. `build.json` is not one of them.
    pub files: BTreeMap<String, String>,
    /// The package's components. Empty until the build compiles them.
    pub components: Vec<BuildComponent>,
}

/// One built component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildComponent {
    pub name: String,
    pub path: String,
    pub sha256: String,
}

impl BuildRecord {
    /// The build record's canonical bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        canonical_json_bytes(&serde_json::to_value(self).expect("a build record always serializes"))
    }
}

/// Derive compiler identity from the package, toolchain and dependency lock.
pub fn generator() -> Result<BuildGenerator> {
    let toolchain: toml::Value = toml::from_str(include_str!("../../../../rust-toolchain.toml"))?;
    let lock: toml::Value = toml::from_str(include_str!("../../../../Cargo.lock"))?;
    let sqlx = lock["package"]
        .as_array()
        .context("Cargo.lock has no packages")?
        .iter()
        .find(|package| package["name"].as_str() == Some("sqlx"))
        .and_then(|package| package["version"].as_str())
        .context("Cargo.lock has no SQLx version")?;
    Ok(BuildGenerator {
        id: concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")).to_owned(),
        toolchain: toolchain["toolchain"]["channel"]
            .as_str()
            .context("toolchain has no channel")?
            .to_owned(),
        sqlx: sqlx.to_owned(),
    })
}
/// Read a completed package build.
pub fn read(package_output: &Path) -> Result<BuildRecord> {
    let path = package_output.join(BUILD_RECORD_FILE);
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let build_record: BuildRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a build record", path.display()))?;
    ensure!(
        build_record.schema == BUILD_RECORD_SCHEMA,
        "{} has schema {}, not {BUILD_RECORD_SCHEMA}",
        path.display(),
        build_record.schema
    );
    Ok(build_record)
}

pub fn physical_schema_fingerprint(package_output: &Path) -> Result<String> {
    let path = package_output.join("physical-schema.json");
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let physical: wamn_schema_control::physical_schema::PhysicalSchema =
        serde_json::from_slice(&bytes).context("parse the physical schema description")?;
    Ok(physical.fingerprint())
}

/// Whether a path belongs to the source tree, excluding generated output and dependencies.
///
/// The path can name a new or deleted file or directory.
#[must_use]
pub fn is_input_path(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|relative| {
        relative.components().all(|component| {
            !["target", "node_modules", "dist"]
                .iter()
                .any(|excluded| component.as_os_str() == *excluded)
        })
    })
}

/// Resolve a workspace build directory to its package source root.
///
/// # Errors
///
/// When the path does not name `<workspace>/target/wamn/<package>`.
pub fn source_root(output: &Path) -> Result<PathBuf> {
    ensure!(
        crate::output::is_build_output(output),
        "{} is not a workspace package output under target/wamn",
        output.display()
    );
    let workspace = output
        .ancestors()
        .nth(3)
        .filter(|path| !path.as_os_str().is_empty())
        .with_context(|| format!("{} has no package workspace parent", output.display()))?;
    let package = output
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("{} has no UTF-8 package name", output.display()))?;
    Ok(crate::output::dependency_root(
        &workspace.join(package),
        package,
    ))
}

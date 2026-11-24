//! Inputs shared by the build fingerprint and development watches.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use wamn_codegen::build::PreparedSource;
use wamn_digest::{Digest, Hasher, bytes_sha256, canonical_json_bytes, format_sha256};

/// Source directories and configuration used by the package and its selected guests.
pub fn paths(source: &PreparedSource) -> Result<BTreeSet<PathBuf>> {
    let root = source.root();
    let workspace = wamn_codegen::package_workspace(root);
    let repository = workspace.parent().context("package workspace parent")?;
    let mut inputs = [
        root.to_owned(),
        repository.join("apps/guests"),
        repository.join("wit"),
        repository.join("crates/authority/wit/deps"),
        workspace.join("Cargo.toml"),
        workspace.join("Cargo.lock"),
        repository.join("Cargo.toml"),
        repository.join("Cargo.lock"),
        repository.join("rust-toolchain.toml"),
        repository.join("mise.toml"),
        repository.join(".cargo"),
        workspace.join(".cargo"),
        repository.join("deploy/sql/record-history.sql"),
        repository.join("deploy/sql/record-history-app-grants.sql"),
    ]
    .into_iter()
    .filter(|path| path.exists())
    .collect::<BTreeSet<_>>();
    let packages = source
        .components()
        .map(|name| name.replace('_', "-"))
        .collect::<Vec<_>>();
    if !packages.is_empty() && workspace.join("Cargo.toml").is_file() {
        inputs.extend(crate::source_dependencies(
            &workspace.join("Cargo.toml"),
            &packages,
        )?);
    }
    if repository.join("Cargo.toml").is_file() {
        inputs.extend(crate::source_dependencies(
            &repository.join("Cargo.toml"),
            &["wamn-build".into()],
        )?);
    }
    Ok(inputs)
}

/// Hash inputs that can change generated artifacts, with paths relative to their workspace.
pub(crate) fn digest(
    source: &PreparedSource,
    inputs: &BTreeSet<PathBuf>,
    base_bytes: &[(Digest, Digest)],
    mode: crate::BuildMode,
    toolchain: &str,
) -> Result<Digest> {
    let workspace = wamn_codegen::package_workspace(source.root());
    let repository = workspace.parent().context("package workspace parent")?;
    let mut files = BTreeMap::new();
    for input in inputs {
        hash_tree(repository, source.root(), input, &mut files)?;
    }
    let baseline = wamn_codegen::migrations::committed_tree(source.root())?;
    let environment = std::env::vars_os()
        .filter(|(name, _)| {
            let name = name.to_string_lossy();
            matches!(
                name.as_ref(),
                "RUSTFLAGS" | "RUSTC" | "CARGO_ENCODED_RUSTFLAGS" | "CARGO_BUILD_RUSTFLAGS"
            ) || name.starts_with("CARGO_PROFILE_")
                || (name.starts_with("CARGO_TARGET_") && name != "CARGO_TARGET_DIR")
        })
        .map(|(name, value)| (name.into_encoded_bytes(), value.into_encoded_bytes()))
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect::<Vec<_>>();
    let value = serde_json::json!([
        files,
        source.document(),
        base_bytes,
        mode,
        wamn_codegen::build::GENERATOR_ID,
        toolchain,
        bytes_sha256(&baseline),
        environment,
    ]);
    Ok(bytes_sha256(&canonical_json_bytes(&value)))
}

fn hash_tree(
    repository: &Path,
    root: &Path,
    path: &Path,
    files: &mut BTreeMap<String, Digest>,
) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() || metadata.is_dir(),
        "build input must be a regular file or directory: {}",
        path.display()
    );
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some("target" | "generated" | "node_modules" | "dist" | ".git")
            ) || (path == root && matches!(name.to_str(), Some("web" | "tests")))
            {
                continue;
            }
            hash_tree(repository, root, &entry.path(), files)?;
        }
    } else {
        let relative = path
            .strip_prefix(repository)?
            .to_str()
            .context("build input path is not UTF-8")?;
        if !files.contains_key(relative) {
            files.insert(relative.to_owned(), file_digest(path)?);
        }
    }
    Ok(())
}

fn file_digest(path: &Path) -> Result<Digest> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut digest = Hasher::new();
    let mut buffer = [0; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(format_sha256(&digest.finalize()));
        }
        digest.update(&buffer[..read]);
    }
}

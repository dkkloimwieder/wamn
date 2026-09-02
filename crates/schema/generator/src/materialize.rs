//! Filesystem and PostgreSQL-backed package materialization.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use tokio_postgres::NoTls;
use wamn_schema_introspection::postgres::read_catalog;

use crate::{
    AuthoredSql, GeneratedPackage, GenerationInput, GenerationProvenance, PackageManifest,
    data_access_schemas, generate,
};

const GENERATOR_ID: &str = "wamn-schema-generator/0.1.0";
const TOOLCHAIN_ID: &str = "rust-1.98.0";

/// Whether materialization writes generated artifacts or checks committed bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializeMode {
    /// Write the exact generated artifact set.
    Write,
    /// Refuse unless committed artifacts equal the generated artifact set.
    Check,
}

#[derive(Debug)]
struct SourceFile {
    path: String,
    bytes: Vec<u8>,
}

/// Materialize one package from its manifest, authored SQL, and migrated database.
///
/// `database_url` names the already-migrated PostgreSQL database to introspect.
/// `source_commit` is recorded as supplied. It need not identify a clean worktree;
/// only an empty value or one containing whitespace is refused.
pub async fn materialize_package(
    mode: MaterializeMode,
    database_url: &str,
    source_commit: &str,
    package_root: &Path,
) -> Result<()> {
    ensure!(
        !source_commit.is_empty() && !source_commit.chars().any(char::is_whitespace),
        "source-commit must be one nonempty argument"
    );

    let manifest_path = package_root.join("wamn.json");
    let manifest_bytes =
        fs::read(&manifest_path).with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest =
        PackageManifest::from_slice(&manifest_bytes).context("parse package manifest")?;
    let source_files = load_authored_sql(package_root, &manifest)?;
    let authored_sql = source_files
        .iter()
        .map(|source| AuthoredSql::new(&source.path, &source.bytes))
        .collect::<Vec<_>>();

    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect to the already-migrated PostgreSQL database")?;
    let connection_task = tokio::spawn(connection);
    let schemas = data_access_schemas(&manifest_bytes).context("resolve application schemas")?;
    let schema_names = schemas.iter().map(String::as_str).collect::<Vec<_>>();
    let catalog_result = read_catalog(&client, &schema_names).await;
    drop(client);
    connection_task
        .await
        .context("join PostgreSQL connection task")?
        .context("drive PostgreSQL connection")?;
    let catalog = catalog_result.context("introspect package schemas")?;

    let package = generate(&GenerationInput::new(
        &catalog,
        &manifest_bytes,
        &authored_sql,
        GenerationProvenance::new(source_commit, GENERATOR_ID, TOOLCHAIN_ID),
    ))
    .context("generate package artifacts")?;
    let output_root = package_root.join("generated");
    let expected = expected_files(&package)?;

    match mode {
        MaterializeMode::Write => write_files(&output_root, &expected),
        MaterializeMode::Check => check_files(&output_root, &expected),
    }
}

fn load_authored_sql(package_root: &Path, manifest: &PackageManifest) -> Result<Vec<SourceFile>> {
    let mut paths = manifest
        .models
        .values()
        .flat_map(|model| model.operations.values())
        .filter_map(|operation| operation.authored_sql.as_ref())
        .flat_map(|authored| {
            std::iter::once(authored.default.as_str()).chain(
                authored
                    .variants
                    .iter()
                    .map(|variant| variant.path.as_str()),
            )
        })
        .collect::<BTreeSet<_>>();
    paths.extend(
        manifest
            .custom_operations
            .values()
            .flat_map(|operation| operation.statements.values())
            .map(|statement| statement.path.as_str()),
    );

    paths
        .into_iter()
        .map(|path| {
            validate_authored_path(path)?;
            let absolute = package_root.join(path);
            let bytes = fs::read(&absolute)
                .with_context(|| format!("read authored query {}", absolute.display()))?;
            Ok(SourceFile {
                path: path.to_owned(),
                bytes,
            })
        })
        .collect()
}

fn validate_authored_path(path: &str) -> Result<()> {
    let path = Path::new(path);
    ensure!(
        (path.starts_with("query") || path.starts_with("command"))
            && path.extension().is_some_and(|extension| extension == "sql")
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
        "authored SQL path must be a safe package-relative query/*.sql or command/*.sql path: {}",
        path.display()
    );
    Ok(())
}

fn expected_files(package: &GeneratedPackage) -> Result<BTreeMap<PathBuf, &[u8]>> {
    package
        .files()
        .iter()
        .map(|file| {
            let relative = Path::new(file.path())
                .strip_prefix("generated")
                .with_context(|| {
                    format!("generated artifact escaped output root: {}", file.path())
                })?;
            ensure!(
                relative
                    .components()
                    .all(|component| matches!(component, Component::Normal(_))),
                "generated artifact has an unsafe relative path: {}",
                file.path()
            );
            Ok((relative.to_owned(), file.bytes()))
        })
        .collect()
}

fn write_files(output_root: &Path, expected: &BTreeMap<PathBuf, &[u8]>) -> Result<()> {
    refuse_unexpected(output_root, expected)?;
    for (relative, bytes) in expected {
        let path = output_root.join(relative);
        let parent = path
            .parent()
            .context("generated artifact path must have a parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("create generated directory {}", parent.display()))?;
        fs::write(&path, *bytes)
            .with_context(|| format!("write generated artifact {}", path.display()))?;
    }
    Ok(())
}

fn check_files(output_root: &Path, expected: &BTreeMap<PathBuf, &[u8]>) -> Result<()> {
    refuse_unexpected(output_root, expected)?;
    for (relative, expected_bytes) in expected {
        let path = output_root.join(relative);
        let actual = fs::read(&path)
            .with_context(|| format!("missing generated artifact {}", path.display()))?;
        ensure!(
            actual.as_slice() == *expected_bytes,
            "generated artifact differs: {}",
            path.display()
        );
    }
    Ok(())
}

fn refuse_unexpected(output_root: &Path, expected: &BTreeMap<PathBuf, &[u8]>) -> Result<()> {
    for relative in existing_files(output_root)? {
        ensure!(
            expected.contains_key(&relative),
            "unexpected existing generated file: {}",
            output_root.join(relative).display()
        );
    }
    Ok(())
}

fn existing_files(root: &Path) -> Result<Vec<PathBuf>> {
    if !root
        .try_exists()
        .with_context(|| format!("inspect {}", root.display()))?
    {
        return Ok(Vec::new());
    }
    ensure!(
        !fs::symlink_metadata(root)
            .with_context(|| format!("inspect {}", root.display()))?
            .file_type()
            .is_symlink(),
        "generated output root must not be a symlink: {}",
        root.display()
    );

    let mut pending = vec![root.to_owned()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)
            .with_context(|| format!("read generated directory {}", directory.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("enumerate generated directory {}", directory.display()))?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("inspect generated path {}", path.display()))?;
            if file_type.is_dir() {
                pending.push(path);
            } else {
                ensure!(
                    file_type.is_file(),
                    "generated output contains a non-file entry: {}",
                    path.display()
                );
                files.push(
                    path.strip_prefix(root)
                        .context("generated path must remain under its output root")?
                        .to_owned(),
                );
            }
        }
    }
    files.sort();
    Ok(files)
}

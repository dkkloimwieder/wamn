//! Filesystem and PostgreSQL-backed package materialization.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use wamn_schema::ir::CatalogIr;

use crate::authoring::package_manifest_path;
use crate::database::BuildDatabase;
use crate::{AuthoredSql, GeneratedPackage};
use wamn_component_policy::PackageManifest;

#[derive(Debug)]
pub(crate) struct SourceFile {
    pub(crate) path: String,
    pub(crate) bytes: Vec<u8>,
}

/// Introspect the package-owned schemas in one already-migrated PostgreSQL database.
pub async fn introspect_package(
    database: &dyn BuildDatabase,
    database_url: &str,
    package_root: &Path,
) -> Result<CatalogIr> {
    let (_, manifest) = load_manifest(package_root)?;
    let sources = load_authored_sql(&crate::output_root(package_root), &manifest)?;
    let authored = sources
        .iter()
        .map(|source| AuthoredSql::new(&source.path, &source.bytes))
        .collect::<Vec<_>>();
    introspect_with_manifest(database, database_url, &manifest, &authored).await
}

/// Introspect the schemas that `manifest` names, as [`introspect_package`] does.
pub(crate) async fn introspect_with_manifest(
    database: &dyn BuildDatabase,
    database_url: &str,
    manifest: &PackageManifest,
    authored_sql: &[AuthoredSql<'_>],
) -> Result<CatalogIr> {
    let schemas = application_schemas(manifest).context("resolve application schemas")?;

    let schema_names = schemas
        .iter()
        .map(wamn_package::canonical_name::CanonicalName::as_str)
        .collect::<Vec<_>>();
    let mut selected = manifest
        .models
        .values()
        .map(|model| model.table.to_string())
        .chain(
            manifest
                .internal_relations
                .values()
                .map(|relation| relation.table.to_string()),
        )
        .collect::<BTreeSet<_>>();
    for source in authored_sql {
        let sql = std::str::from_utf8(source.bytes())
            .with_context(|| format!("{} is not UTF-8", source.path()))?;
        let parsed = pg_query::parse(sql).with_context(|| format!("parse {}", source.path()))?;
        selected.extend(parsed.tables.into_iter().map(|(table, _)| table));
    }
    let selected = selected.into_iter().collect::<Vec<_>>();
    database
        .read_catalog(database_url, &schema_names, &selected)
        .await
}

/// Generate from compiled manifest bytes that the caller holds. `manifest` is
fn load_manifest(package_root: &Path) -> Result<(Vec<u8>, PackageManifest)> {
    let bytes = fs::read(package_manifest_path(package_root))?;
    let manifest = PackageManifest::from_slice(&bytes)?;
    Ok((bytes, manifest))
}

pub(crate) fn load_authored_sql(
    package_root: &Path,
    manifest: &PackageManifest,
) -> Result<Vec<SourceFile>> {
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

    read_sql_sources(package_root, paths)
}

pub(crate) fn read_sql_sources<'a>(
    package_root: &Path,
    paths: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<SourceFile>> {
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

pub(crate) fn expected_files(package: &GeneratedPackage) -> Result<BTreeMap<PathBuf, &[u8]>> {
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

/// Replace generated output with the compiler artifacts.
/// An unchanged file keeps its modification time.
pub(crate) fn write_files(output_root: &Path, expected: &BTreeMap<PathBuf, &[u8]>) -> Result<()> {
    for stale in existing_files(output_root)? {
        if !expected.contains_key(&stale) {
            let path = output_root.join(stale);
            fs::remove_file(&path)
                .with_context(|| format!("remove stale generated artifact {}", path.display()))?;
        }
    }
    for (relative, bytes) in expected {
        let path = output_root.join(relative);
        // An unchanged file keeps its modification time, so Cargo reuses the
        // build after a restarted session regenerates (wamn-llst.8). A failed
        // read falls through to the write, which reports its own error.
        if fs::read(&path).is_ok_and(|actual| actual.as_slice() == *bytes) {
            continue;
        }
        let parent = path
            .parent()
            .context("generated artifact path must have a parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("create generated directory {}", parent.display()))?;
        fs::write(&path, *bytes)
            .with_context(|| format!("write generated artifact {}", path.display()))?;
    }
    remove_empty_directories(output_root)?;
    Ok(())
}

/// Remove obsolete directories, including WIT dependencies whose last file disappeared.
fn remove_empty_directories(root: &Path) -> Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read generated directory {}", root.display()));
        }
    };
    for entry in entries {
        let entry = entry.context("read generated directory entry")?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        remove_empty_directories(&path)?;
        if fs::read_dir(&path)?.next().is_none() {
            fs::remove_dir(&path)
                .with_context(|| format!("remove empty generated directory {}", path.display()))?;
        }
    }
    Ok(())
}

pub(crate) fn existing_files(root: &Path) -> Result<Vec<PathBuf>> {
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

pub(crate) fn application_schemas(
    manifest: &PackageManifest,
) -> Result<Vec<wamn_package::canonical_name::CanonicalName>, wamn_package::manifest::ManifestError>
{
    let schemas = manifest
        .models
        .values()
        .map(|model| model.schema.clone().into())
        .chain(manifest.custom_operations.values().flat_map(|operation| {
            operation
                .relations
                .iter()
                .map(|relation| relation.schema.clone().into())
        }))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    // A composition with only stateless wrappers owns no SQL schema. Its bases
    // own the SQL and contribute their grants when the release is applied.
    if schemas.is_empty() && (manifest.declares_no_sql() || manifest.uses_only_base_sql()) {
        return Ok(schemas);
    }
    if schemas.is_empty() {
        return Err(wamn_package::manifest::ManifestError::new(
            wamn_package::manifest::ManifestErrorType::InvalidManifest,
            "manifest must name at least one valid application schema",
        ));
    }
    Ok(schemas)
}

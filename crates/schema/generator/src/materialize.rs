//! Filesystem and PostgreSQL-backed package materialization.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use tokio_postgres::NoTls;
use wamn_schema_introspection::ir::CatalogIr;
use wamn_schema_introspection::postgres::read_catalog_excluding_relations;

use crate::StatementTransactionality;
use crate::{
    AuthoredSql, GeneratedPackage, GenerationInput, GenerationProvenance, PackageManifest,
    data_access::application_schemas, generate, manifest::CONTROL_OWNED_RELATION_TABLES,
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
pub async fn materialize_package(
    mode: MaterializeMode,
    database_url: &str,
    package_root: &Path,
) -> Result<()> {
    materialize_after_introspection(
        mode,
        package_root,
        introspect_package(database_url, package_root),
    )
    .await
}

/// Introspect the package-owned schemas in one already-migrated PostgreSQL database.
pub async fn introspect_package(database_url: &str, package_root: &Path) -> Result<CatalogIr> {
    let (_, manifest) = load_manifest(package_root)?;
    let schemas = application_schemas(&manifest).context("resolve application schemas")?;

    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect to the already-migrated PostgreSQL database")?;
    let connection_task = tokio::spawn(connection);
    let schema_names = schemas.iter().map(String::as_str).collect::<Vec<_>>();
    let excluded_relations = schema_names
        .iter()
        .flat_map(|schema| {
            CONTROL_OWNED_RELATION_TABLES
                .iter()
                .map(move |table| (*schema, *table))
        })
        .collect::<Vec<_>>();
    let catalog_result =
        read_catalog_excluding_relations(&client, &schema_names, &excluded_relations).await;
    drop(client);
    connection_task
        .await
        .context("join PostgreSQL connection task")?
        .context("drive PostgreSQL connection")?;
    catalog_result.context("introspect package schemas")
}

/// Ask PostgreSQL which of these statements need a transaction.
///
/// `EXPLAIN (GENERIC_PLAN, FORMAT JSON)` plans a PARAMETERISED statement
/// without executing it and without supplying binds -- which is the whole
/// reason it exists (PostgreSQL 16+). A statement needs a transaction when its
/// plan carries a `ModifyTable` node (it writes; a data-modifying CTE appears
/// as a ModifyTable inside the CTE subplan) or a `LockRows` node (it takes a
/// row lock, and autocommit would drop that lock the instant the statement
/// returned).
///
/// The verdict is the SERVER'S. Nothing here reads SQL text.
async fn classify_statements(
    client: &tokio_postgres::Client,
    corpus: &std::collections::BTreeMap<String, Vec<u8>>,
    schemas: &[String],
) -> Result<StatementTransactionality> {
    // Plan with the SAME search_path the guest runs under, or an unqualified
    // relation the package owns cannot be resolved and every statement
    // referencing it fails to plan.
    let search_path = schemas
        .iter()
        .map(|schema| format!("\"{}\"", schema.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    if !search_path.is_empty() {
        client
            .batch_execute(&format!("SET search_path TO {search_path}, public"))
            .await
            .context("set the package search_path before planning")?;
    }
    let mut verdicts = std::collections::BTreeMap::new();
    for (path, bytes) in corpus {
        let sql = std::str::from_utf8(bytes)
            .with_context(|| format!("statement {path} is not UTF-8"))?;
        // simple_query, NOT query_one: the statement carries $1..$n, and the
        // extended protocol would treat those as parameters OF THE EXPLAIN and
        // refuse with "expected N parameters but got 0". GENERIC_PLAN exists
        // precisely so the planner supplies its own placeholders.
        let messages = client
            .simple_query(&format!("EXPLAIN (GENERIC_PLAN, FORMAT JSON) {sql}"))
            .await
            .with_context(|| format!("plan statement {path} against the migrated database"))?;
        let rendered = messages
            .iter()
            .find_map(|message| match message {
                tokio_postgres::SimpleQueryMessage::Row(row) => row.get(0),
                _ => None,
            })
            .with_context(|| format!("planning {path} returned no plan"))?;
        let plan: serde_json::Value = serde_json::from_str(rendered)
            .with_context(|| format!("parse the plan PostgreSQL returned for {path}"))?;
        verdicts.insert(path.clone(), plan_needs_transaction(&plan));
    }
    Ok(StatementTransactionality::from_paths(verdicts))
}

/// Walk a plan tree for the two node types that require a transaction.
fn plan_needs_transaction(plan: &serde_json::Value) -> bool {
    match plan {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(node)) = map.get("Node Type")
                && (node == "ModifyTable" || node == "LockRows")
            {
                return true;
            }
            map.values().any(plan_needs_transaction)
        }
        serde_json::Value::Array(items) => items.iter().any(plan_needs_transaction),
        _ => false,
    }
}

/// Materialize one package from an already-introspected catalog.
///
/// This stage reads the package's manifest and authored SQL, but performs no
/// database access. The supplied catalog is the sole schema input.
pub fn materialize_package_from_catalog(
    mode: MaterializeMode,
    catalog: &CatalogIr,
    package_root: &Path,
) -> Result<()> {
    materialize_package_classified(mode, catalog, package_root, &StatementTransactionality::default())
}

/// Materialize with PostgreSQL's verdict on which statements need a transaction.
///
/// Generation runs TWICE. The first pass exists only to obtain the SQL corpus:
/// the statements are generated from the catalog, so they cannot be planned
/// before they exist. The second pass emits the contracts carrying the verdicts
/// the server gave for that corpus. The bit is contract-only and never reaches
/// SQL bytes, so both passes produce an identical corpus.
pub fn materialize_package_classified(
    mode: MaterializeMode,
    catalog: &CatalogIr,
    package_root: &Path,
    transactional: &StatementTransactionality,
) -> Result<()> {
    let package = generate_package(catalog, package_root, transactional)?;
    let output_root = package_root.join("generated");
    let expected = expected_files(&package)?;

    match mode {
        MaterializeMode::Write => write_files(&output_root, &expected),
        MaterializeMode::Check => check_files(&output_root, &expected),
    }
}

fn generate_package(
    catalog: &CatalogIr,
    package_root: &Path,
    transactional: &StatementTransactionality,
) -> Result<crate::GeneratedPackage> {
    let (manifest_bytes, manifest) = load_manifest(package_root)?;
    let source_files = load_authored_sql(package_root, &manifest)?;
    let authored_sql = source_files
        .iter()
        .map(|source| AuthoredSql::new(&source.path, &source.bytes))
        .collect::<Vec<_>>();

    generate(&GenerationInput::new(
        catalog,
        &manifest_bytes,
        &authored_sql,
        GenerationProvenance::new(GENERATOR_ID, TOOLCHAIN_ID),
        transactional,
    ))
    .context("generate package artifacts")
}

/// Every statement this package will admit, keyed by the path its contracts
/// name: authored SQL at the package root, generated SQL under `generated/`.
fn statement_corpus(
    package_root: &Path,
    catalog: &CatalogIr,
) -> Result<std::collections::BTreeMap<String, Vec<u8>>> {
    let (_, manifest) = load_manifest(package_root)?;
    let mut corpus = std::collections::BTreeMap::new();
    for source in load_authored_sql(package_root, &manifest)? {
        corpus.insert(source.path.clone(), source.bytes.clone());
    }
    let discovery = generate_package(catalog, package_root, &StatementTransactionality::default())?;
    for file in discovery.files() {
        if file.path().ends_with(".sql") {
            corpus.insert(file.path().to_owned(), file.bytes().to_vec());
        }
    }
    Ok(corpus)
}

async fn materialize_after_introspection<F>(
    mode: MaterializeMode,
    package_root: &Path,
    introspection: F,
) -> Result<()>
where
    F: std::future::Future<Output = Result<CatalogIr>>,
{
    let catalog = introspection.await?;
    materialize_package_from_catalog(mode, &catalog, package_root)
}

/// Materialize against a live migrated database, asking the server which
/// statements need a transaction.
pub async fn materialize_package_verified(
    mode: MaterializeMode,
    database_url: &str,
    package_root: &Path,
) -> Result<()> {
    let catalog = introspect_package(database_url, package_root).await?;
    let corpus = statement_corpus(package_root, &catalog)?;

    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect to plan the package statements")?;
    let connection_task = tokio::spawn(connection);
    let (_, manifest) = load_manifest(package_root)?;
    let schemas = application_schemas(&manifest).context("resolve application schemas")?;
    let verdicts = classify_statements(&client, &corpus, &schemas).await;
    drop(client);
    connection_task
        .await
        .context("join PostgreSQL connection task")?
        .context("drive PostgreSQL connection")?;
    let verdicts = verdicts?;

    materialize_package_classified(mode, &catalog, package_root, &verdicts)
}

fn load_manifest(package_root: &Path) -> Result<(Vec<u8>, PackageManifest)> {
    let manifest_path = package_root.join("wamn.json");
    let manifest_bytes =
        fs::read(&manifest_path).with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest =
        PackageManifest::from_slice(&manifest_bytes).context("parse package manifest")?;
    Ok((manifest_bytes, manifest))
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use wamn_schema_introspection::ir::{Column, ColumnType, Constraint, Table};

    use super::*;

    const MANIFEST: &[u8] = br#"{
        "package":{"id":"test_package","version":"1.0.0"},
        "required_platform_policy_contract":{"id":"test_data_access","state":"satisfied"},
        "models":{
            "thing":{
                "schema":"application",
                "table":"thing",
                "owner":"test_package",
                "server_owned_fields":["id"],
                "operations":{
                    "get":{
                        "permission":"thing.get",
                        "error_details":{
                            "invalid_input":{"required":["field"]},
                            "not_found":{"required":["field","id"]},
                            "retry":{},
                            "timeout":{},
                            "permission_denied":{"required":["operation"]},
                            "internal_error":{}
                        },
                        "result":"one"
                    }
                }
            }
        },
        "connections":{"postgres":{"interface":"wamn:postgres@0.1.0"}},
        "components":{"test_package":{"connections":["postgres"]}}
    }"#;

    struct TestPackage {
        root: PathBuf,
    }

    impl TestPackage {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "wamn-schema-generator-materialize-{}-{name}",
                std::process::id()
            ));
            if root.exists() {
                fs::remove_dir_all(&root).expect("remove stale test package");
            }
            fs::create_dir_all(&root).expect("create test package");
            fs::write(root.join("wamn.json"), MANIFEST).expect("write test manifest");
            Self { root }
        }

        fn generated_snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
            let generated = self.root.join("generated");
            existing_files(&generated)
                .expect("enumerate generated files")
                .into_iter()
                .map(|relative| {
                    let bytes = fs::read(generated.join(&relative)).expect("read generated file");
                    (relative, bytes)
                })
                .collect()
        }
    }

    impl Drop for TestPackage {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).expect("remove test package");
        }
    }

    fn catalog() -> CatalogIr {
        CatalogIr::new(vec![Table::new(
            "application",
            "thing",
            vec![Column::new("id", ColumnType::Uuid, false, None, None)],
            vec![Constraint::primary_key("thing_id_pkey", ["id"]).expect("construct primary key")],
            Vec::new(),
        )])
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wrapper_and_split_paths_are_identical_without_reintrospection() {
        let wrapper = TestPackage::new("wrapper");
        let split = TestPackage::new("split");
        let catalog = catalog();
        let introspections = Cell::new(0_u8);

        materialize_after_introspection(MaterializeMode::Write, &wrapper.root, async {
            introspections.set(introspections.get() + 1);
            Ok(catalog.clone())
        })
        .await
        .expect("materialize through wrapper path");
        assert_eq!(introspections.get(), 1);

        materialize_package_from_catalog(MaterializeMode::Write, &catalog, &split.root)
            .expect("materialize split generation path");
        assert_eq!(
            introspections.get(),
            1,
            "generation unexpectedly repeated database introspection"
        );
        assert_eq!(wrapper.generated_snapshot(), split.generated_snapshot());

        let generated = split.generated_snapshot();
        let stale = generated.keys().next().expect("generated artifact exists");
        fs::write(split.root.join("generated").join(stale), b"stale")
            .expect("write stale generated artifact");
        let error = materialize_package_from_catalog(MaterializeMode::Check, &catalog, &split.root)
            .expect_err("exact mode accepted stale output");
        assert!(error.to_string().contains("generated artifact differs"));
    }
}

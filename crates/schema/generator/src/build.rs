//! Compile package declarations and described SQL to one output tree.
//!
//! Pass one compiles `wamn.k` to its authored form, migrates a fresh
//! verification database, describes every statement through SQLx,
//! derives the statement types and relations, and emits the canonical
//! `wamn.json` (RFC 8785, hashed once). Pass two generates every consumer from
//! those bytes and writes them to the package's build
//! output directory. Step 7 compiles the package's components
//! in xtask. `wamn web build` builds browser files separately.
//!
//! The verification database is created on the PostgreSQL server that the
//! request names and dropped when the build ends. The server keeps the
//! cluster-wide roles `wamn_app` and `wamn_db_owner` that the build creates
//! when they are absent.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Map, Value};
use sqlx::{AssertSqlSafe, Row as _, SqlSafeStr as _};
use wamn_digest::canonical_json_bytes;
use wamn_schema_introspection::ir::CatalogIr;
use wamn_schema_introspection::postgres::authored_object_name;

use crate::AuthoredSql;
use crate::authoring::{COMPILED_MANIFEST_NAME, compile_authored_manifest};
use crate::build_record::{
    BUILD_RECORD_SCHEMA, BuildDependency, BuildOutputs, BuildPackage, BuildRecord,
};
use crate::describe::{DescribedStatement, column_type, describe_statements};
use crate::manifest::{CrudAction, PackageManifest, ResultClass, canonical_operation_identity};
use crate::materialize::application_schemas;
use crate::materialize::{
    classify_statements, client_bindings, expected_files, introspect_with_manifest, write_files,
};
use wamn_digest::bytes_sha256;

const RECORD_HISTORY_SQL: &str = include_str!("../../../../deploy/sql/record-history.sql");
const RECORD_HISTORY_APP_GRANTS_SQL: &str =
    include_str!("../../../../deploy/sql/record-history-app-grants.sql");

/// One package build.
#[derive(Debug, Clone)]
pub struct BuildRequest {
    pub package_root: PathBuf,
    pub output_root: PathBuf,
    database_url: String,
    /// Allow dev composition with a built base that differs from its authored pin.
    /// Publication builds must leave this false.
    pub allow_stale_base_pins: bool,
}

impl BuildRequest {
    /// Build the package at `package_root` into `output_root`, the directory
    /// that holds one build output directory per package.
    ///
    /// `database_url` names a PostgreSQL server through a superuser URL. The
    /// build creates and drops its own database there. An overlay reads the
    /// `build.json` of each base package from `output_root`, so its bases are
    /// built first.
    #[must_use]
    pub fn new(package_root: &Path, output_root: &Path, database_url: &str) -> Self {
        Self {
            package_root: package_root.to_owned(),
            output_root: output_root.to_owned(),
            database_url: database_url.to_owned(),
            allow_stale_base_pins: false,
        }
    }
}

/// The default output root of the package at `package_root`:
/// `apps/target/wamn` for both applications and nested platform packages.
#[must_use]
pub fn default_output_root(package_root: &Path) -> PathBuf {
    crate::output::package_workspace(package_root).join("target/wamn")
}

/// The build output directory of `package` below `output_root`.
#[must_use]
pub fn package_output(output_root: &Path, package: &str) -> PathBuf {
    output_root.join(package)
}

/// Generate package output before component compilation.
/// The build record contains no compiled components.
///
/// # Errors
///
/// When the manifest does not compile, when a base package has no completed
/// build record, when the verification database cannot be created or migrated, when
/// a statement does not describe or a declared statement type is wider than
/// the described one, when generation refuses, or when the output cannot be
/// written.
pub async fn generate_package(request: &BuildRequest) -> Result<BuildRecord> {
    let (source, source_paths) = snapshot_source(&request.package_root)?;
    let package_root = source.path();
    let mut authored: Value = serde_json::from_slice(&authored_manifest(package_root)?)
        .context("parse the authored manifest")?;
    let provisional =
        ManifestSources::from_document(&authored).context("parse the authored manifest")?;
    let dependencies = dependencies(&request.output_root, &provisional)?;
    let bases = crate::inheritance::load_base_manifests(
        &request.output_root,
        &provisional.base_dependencies,
    )?;

    let database = VerificationDatabase::create(&request.database_url).await?;
    let built = pass_one_and_two(
        package_root,
        &database.url,
        &provisional,
        &mut authored,
        &bases,
        &request.output_root,
    )
    .await;
    database.drop().await?;
    let (canonical, mut files) = built?;
    for path in source_paths {
        if let std::collections::btree_map::Entry::Vacant(entry) = files.entry(path) {
            let bytes = fs::read(source.path().join(entry.key()))?;
            entry.insert(bytes);
        }
    }

    let package_output = package_output(&request.output_root, &provisional.package.id);
    let outputs = write_outputs(&package_output, &canonical, &files)?;
    let build_record = BuildRecord {
        schema: BUILD_RECORD_SCHEMA.to_owned(),
        package: BuildPackage {
            id: provisional.package.id.clone(),
            version: provisional.package.version.clone(),
        },
        dependencies,
        generator: crate::build_record::generator()?,
        outputs,
    };
    Ok(build_record)
}

/// Freeze the non-Rust inputs before compiling or applying migrations.
fn snapshot_source(root: &Path) -> Result<(tempfile::TempDir, Vec<PathBuf>)> {
    ensure!(
        !root.join("wamn.json").exists(),
        "{}: author the package in wamn.k",
        root.display()
    );
    let snapshot = tempfile::tempdir()?;
    let mut directories = vec![PathBuf::new()];
    let mut paths = Vec::new();
    while let Some(relative) = directories.pop() {
        for entry in fs::read_dir(root.join(&relative))? {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            let type_ = entry.file_type()?;
            let authored_tree = path.starts_with("migrations")
                || path.starts_with("command")
                || path.starts_with("query");
            if type_.is_dir() {
                if !matches!(
                    entry.file_name().to_str(),
                    Some(
                        "target"
                            | "node_modules"
                            | ".git"
                            | "web"
                            | "component"
                            | "data"
                            | "tests"
                            | "generated"
                    )
                ) {
                    directories.push(path);
                }
            } else if authored_tree
                || path.extension().is_some_and(|extension| extension == "k")
                || matches!(path.to_str(), Some("kcl.mod" | "kcl.mod.lock"))
            {
                ensure!(
                    type_.is_file(),
                    "authored input must be a regular file: {}",
                    entry.path().display()
                );
                let destination = snapshot.path().join(&path);
                paths.push(path);
                fs::create_dir_all(
                    destination
                        .parent()
                        .context("source input has no directory")?,
                )?;
                fs::write(destination, fs::read(entry.path())?)?;
            }
        }
    }
    Ok((snapshot, paths))
}

/// The two passes against the migrated verification database: the canonical
/// manifest bytes and the generated files, by path relative to the package's
/// build output directory.
async fn pass_one_and_two(
    package_root: &Path,
    database_url: &str,
    provisional: &ManifestSources,
    authored: &mut Value,
    bases: &[PackageManifest],
    output_root: &Path,
) -> Result<(Vec<u8>, BTreeMap<PathBuf, Vec<u8>>)> {
    // Pass one.
    migrate(database_url, package_root, provisional, bases, output_root).await?;
    let sources = provisional.load_sql(package_root)?;
    let described = describe_statements(
        database_url,
        sources
            .iter()
            .map(|source| (&source.path, &source.bytes))
            .collect::<Vec<_>>(),
    )
    .await?;
    apply_statement_types(authored, &described)?;
    let typed = serde_json::to_vec(authored).context("serialize the typed manifest")?;
    let mut manifest =
        PackageManifest::parse_authored(&typed).context("parse the typed manifest")?;
    let authored_sql = sources
        .iter()
        .map(|source| AuthoredSql::new(&source.path, &source.bytes))
        .collect::<Vec<_>>();
    let catalog = introspect_with_manifest(database_url, &manifest, &authored_sql).await?;
    // The build refuses correctness only (owner ruling 2026-10-18): a missing
    // pagination index is a warning.
    for warning in missing_paged_query_indexes(database_url, &catalog, &manifest).await? {
        eprintln!("warning: {warning}");
    }
    crate::generate::derive_model(&catalog, &mut manifest, &authored_sql, bases)
        .context("derive the statement relations")?;
    let canonical = canonical_json_bytes(&serde_json::to_value(&manifest)?);

    // Pass two: every consumer from the canonical bytes.
    for (name, operation) in &manifest.custom_operations {
        if let Some(source) = &operation.feed_source {
            for warning in crate::feed::missing_indexes(&catalog, name, source) {
                eprintln!("warning: {warning}");
            }
        }
    }
    let preliminary = crate::StatementTransactionality::from_paths(BTreeMap::new());
    let input = crate::GenerationInput::new(&catalog, &canonical, &authored_sql, &preliminary);
    let corpus = crate::generate::sql_corpus(&input, &manifest)?;
    let remaining = corpus
        .statements
        .iter()
        .filter(|(path, _)| !described.contains_key(*path))
        .collect::<Vec<_>>();
    let remaining = describe_statements(database_url, remaining).await?;
    let schemas = application_schemas(&manifest).context("resolve application schemas")?;
    let mut verdicts = classify_statements(database_url, &corpus.statements, &schemas).await?;
    verdicts.described = described;
    verdicts.described.extend(remaining);
    let input = crate::GenerationInput::new(&catalog, &canonical, &authored_sql, &verdicts);
    let mut package = crate::generate::generate_described(&input, &manifest, &corpus)?;
    let client = client_bindings(&manifest, &package)?;
    manifest.compiled_operations = package.take_operations();
    let canonical = canonical_json_bytes(&serde_json::to_value(&manifest)?);
    let mut files = expected_files(&package)?
        .into_iter()
        .map(|(path, bytes)| (path, bytes.to_vec()))
        .collect::<BTreeMap<_, _>>();
    for file in &client {
        let relative = Path::new(file.path())
            .strip_prefix("generated")
            .with_context(|| format!("client binding escaped output root: {}", file.path()))?;
        files.insert(relative.to_owned(), file.bytes().to_vec());
    }
    let physical = physical_schema(database_url, package_root, &manifest).await?;
    if !physical.columns.is_empty() {
        let history = wamn_schema_control::physical_schema::render(
            &(&manifest).into(),
            &[],
            &physical.columns,
        )?;
        let history = history
            .into_iter()
            .map(|statement| statement.sql)
            .collect::<Vec<_>>()
            .join(";\n");
        files.insert(PathBuf::from("record-history.sql"), history.into_bytes());
    }
    files.insert(
        PathBuf::from("physical-schema.json"),
        canonical_json_bytes(&serde_json::to_value(physical)?),
    );
    files.insert(
        PathBuf::from("wamn.k"),
        fs::read(package_root.join(crate::AUTHORED_MANIFEST))?,
    );
    for source in sources {
        files.insert(PathBuf::from(source.path), source.bytes);
    }
    let migrations = package_root.join("migrations");
    if migrations.is_dir() {
        for entry in fs::read_dir(migrations)? {
            let path = entry?.path();
            if path.is_file() {
                files.insert(
                    PathBuf::from("migrations").join(path.file_name().context("migration name")?),
                    fs::read(path)?,
                );
            }
        }
    }
    Ok((canonical, files))
}

async fn physical_schema(
    database_url: &str,
    package_root: &Path,
    manifest: &PackageManifest,
) -> Result<wamn_schema_control::physical_schema::PhysicalSchema> {
    use wamn_schema_control::physical_schema::{
        PhysicalSchema, RelationColumn, SELECT_RELATION_COLUMNS_SQL, render,
    };
    let mut migrations = Vec::new();
    let directory = package_root.join("migrations");
    if directory.exists() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.extension().is_some_and(|extension| extension == "sql") {
                migrations.push(wamn_schema_control::MigrationSource {
                    relative_path: path
                        .strip_prefix(package_root)?
                        .to_string_lossy()
                        .into_owned(),
                    bytes: fs::read(path)?,
                });
            }
        }
    }
    let client = wamn_postgres::pool(database_url, 4).await?;
    let result = async {
        let mut columns = BTreeMap::new();
        for (id, model) in &manifest.models {
            if model.owner != manifest.package.id {
                continue;
            }
            let rows =
                sqlx::query(AssertSqlSafe((SELECT_RELATION_COLUMNS_SQL).to_owned()).into_sql_str())
                    .bind(&model.schema)
                    .bind(&model.table)
                    .fetch_all(&client)
                    .await?;
            columns.insert(
                id.clone(),
                rows.iter()
                    .map(|row| RelationColumn {
                        name: row.get(0),
                        primary_key: row.get(1),
                        timestamptz: row.get(2),
                    })
                    .collect(),
            );
        }
        let statements = render(&manifest.into(), &migrations, &columns)?;
        Ok(PhysicalSchema {
            columns,
            statements,
        })
    }
    .await;
    client.close().await;
    result
}

/// One warning for each allowed sort of a paged query without its
/// (field, tie-breaker) index. The catalog includes indexes from every applied
/// package migration.
async fn missing_paged_query_indexes(
    database_url: &str,
    catalog: &CatalogIr,
    manifest: &PackageManifest,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    for (model_name, model) in &manifest.models {
        let Some(query) = model
            .operations
            .get(&CrudAction::Query)
            .filter(|query| query.result == ResultClass::Page)
        else {
            continue;
        };
        let operation =
            canonical_operation_identity(&manifest.package, &format!("{model_name}.query"))?;
        let pagination = query
            .pagination
            .as_ref()
            .with_context(|| format!("{operation} requires keyset pagination"))?;
        let table = catalog
            .tables()
            .iter()
            .find(|table| table.schema() == model.schema && table.name() == model.table)
            .with_context(|| {
                format!(
                    "{operation} references unknown {}.{}",
                    model.schema, model.table
                )
            })?;
        let mut fields = BTreeSet::from([pagination.default_sort.field.as_str()]);
        if let Some(sort) = &query.sort {
            fields.extend(sort.fields.iter().map(String::as_str));
        }
        let tie_breaker = pagination.tie_breaker.field.as_str();
        for field in fields {
            if table.indexes().iter().any(|index| {
                let [first, second] = index.columns() else {
                    return false;
                };
                first.name() == field
                    && second.name() == tie_breaker
                    && first.direction() == second.direction()
            }) {
                continue;
            }
            // The normalized IR sorts columns by name. PostgreSQL retains the
            // definition order that the authored naming rule requires.
            let client = wamn_postgres::pool(database_url, 4)
                .await
                .context("connect to name the missing pagination index")?;
            let columns = sqlx::query(AssertSqlSafe(("SELECT attribute.attname::text
                     FROM pg_catalog.pg_attribute AS attribute
                     JOIN pg_catalog.pg_class AS relation ON relation.oid = attribute.attrelid
                     JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace
                     WHERE namespace.nspname = $1 AND relation.relname = $2
                       AND attribute.attnum > 0 AND NOT attribute.attisdropped
                       AND attribute.attname::text = ANY($3::text[])
                     ORDER BY attribute.attnum").to_owned()).into_sql_str()).bind(&model.schema).bind(&model.table).bind(vec![field, tie_breaker]).fetch_all(&client)
                .await;
            client.close().await;
            let columns = columns
                .context("read the pagination index columns in table-definition order")?
                .iter()
                .map(|row| row.get::<String, _>(0))
                .collect::<Vec<_>>();
            let name = authored_object_name(table.name(), &columns, "idx");
            warnings.push(format!(
                "{operation} on {}.{} has no index for ORDER BY ({field}, {tie_breaker}). \
                 Add to the package's migrations: CREATE INDEX {name} ON {}.{} ({field}, {tie_breaker});",
                table.schema(),
                table.name(),
                table.schema(),
                table.name(),
            ));
        }
    }
    Ok(warnings)
}

/// The authored manifest bytes compiled from `wamn.k`.
fn authored_manifest(package_root: &Path) -> Result<Vec<u8>> {
    compile_authored_manifest(package_root)
}

/// The base packages of an overlay, from the build records of their builds.
fn dependencies(output_root: &Path, manifest: &ManifestSources) -> Result<Vec<BuildDependency>> {
    manifest
        .base_dependencies
        .values()
        .map(|dependency| {
            let base = crate::build_record::read(&package_output(output_root, &dependency.package))
                .with_context(|| format!("build the base package {} first", dependency.package))?;
            ensure!(
                base.package.id == dependency.package && base.package.version == dependency.version,
                "the base build is {} {}, and the package pins {} {}",
                base.package.id,
                base.package.version,
                dependency.package,
                dependency.version
            );
            Ok(BuildDependency {
                id: base.package.id,
                version: base.package.version,
                digest: base.outputs.manifest,
            })
        })
        .collect()
}

/// Every statement value object of a manifest document: each parameter and row
/// value of each custom operation statement, with the statement's name and path.
fn for_each_statement(
    manifest: &mut Value,
    mut visit: impl FnMut(&str, &mut Map<String, Value>) -> Result<()>,
) -> Result<()> {
    let Some(operations) = manifest
        .get_mut("custom_operations")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    for (operation_name, operation) in operations {
        let Some(statements) = operation
            .get_mut("statements")
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        for (statement_name, statement) in statements {
            let statement = statement.as_object_mut().with_context(|| {
                format!("statement {operation_name}.{statement_name} is not an object")
            })?;
            visit(&format!("{operation_name}.{statement_name}"), statement)?;
        }
    }
    Ok(())
}

/// The value objects of one statement member, `parameters` or `row`.
fn values_mut<'a>(
    statement: &'a mut Map<String, Value>,
    member: &str,
) -> impl Iterator<Item = &'a mut Map<String, Value>> {
    statement
        .get_mut(member)
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
}

/// Apply what describe says to the statement values of the authored manifest
/// (docs/architecture/data-access.md#application-declarations).
///
/// A value without a type takes the described type. A stated type must equal
/// it. A parameter without a nullability is not null, because SQLx describes
/// no parameter nullability. A row value without a nullability takes SQLx's,
/// and is nullable where SQLx cannot tell. For a stated row nullability the
/// owner's rule (wamn-00rts.3) holds: a `nullable: true` declaration on a column that SQLx says cannot be null is
/// wider, always safe, and kept, because SQLx can miss an outer join that
/// planning removed. A `nullable: false` declaration is refused only when SQLx
/// says the column can be null; when SQLx does not know, as for an
/// expression, the declaration stands. Every refusal of the package is
/// reported at once.
/// View columns carry no nullability evidence, so their declarations stand too.
/// An undeclared view value is nullable because PostgreSQL gives no evidence.
fn apply_statement_types(
    manifest: &mut Value,
    described: &BTreeMap<String, DescribedStatement>,
) -> Result<()> {
    let mut refusals = Vec::new();
    for_each_statement(manifest, |at, statement| {
        let path = statement
            .get("path")
            .and_then(Value::as_str)
            .with_context(|| format!("statement {at} names no path"))?
            .to_owned();
        let described = described
            .get(&path)
            .with_context(|| format!("statement {at} ({path}) was not described"))?;
        let at = format!("{at} ({path})");

        let parameters = values_mut(statement, "parameters").collect::<Vec<_>>();
        if parameters.len() == described.parameters.len() {
            for (value, type_name) in parameters.into_iter().zip(&described.parameters) {
                apply_type(&at, value, type_name, &mut refusals);
                value.entry("nullable").or_insert(Value::Bool(false));
            }
        } else {
            refusals.push(format!(
                "{at}: the statement takes {} parameters, and the manifest names {}",
                described.parameters.len(),
                parameters.len()
            ));
        }

        let row = values_mut(statement, "row").collect::<Vec<_>>();
        if row.len() != described.columns.len() {
            refusals.push(format!(
                "{at}: the statement returns {} columns, and the manifest names {}",
                described.columns.len(),
                row.len()
            ));
            return Ok(());
        }
        for (value, column) in row.into_iter().zip(&described.columns) {
            let name = value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if name != column.name {
                refusals.push(format!(
                    "{at}: the manifest names row value {name}, and the statement returns column {}",
                    column.name
                ));
                continue;
            }
            apply_type(&at, value, &column.type_name, &mut refusals);
            match value.get("nullable").map(Value::as_bool) {
                None => {
                    value.insert(
                        "nullable".to_owned(),
                        Value::Bool(column.nullable != Some(false)),
                    );
                }
                Some(Some(false)) if column.nullable == Some(true) && !column.view_column => refusals.push(format!(
                    "{at}: row value {name} is declared not null, and SQLx says its column can be null; declare it nullable"
                )),
                Some(Some(_)) => {}
                Some(None) => refusals.push(format!(
                    "{at}: row value {name} has a nullable that is not a boolean"
                )),
            }
        }
        Ok(())
    })?;
    if !refusals.is_empty() {
        bail!(
            "the statement declarations disagree with SQLx's description:\n{}",
            refusals.join("\n")
        );
    }
    Ok(())
}

/// Apply one described type to one statement value.
fn apply_type(
    at: &str,
    value: &mut Map<String, Value>,
    type_name: &str,
    refusals: &mut Vec<String>,
) {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let Some(derived) = column_type(type_name) else {
        refusals.push(format!(
            "{at}: {name} has PostgreSQL type {type_name}, which the manifest vocabulary has no type for"
        ));
        return;
    };
    match value.get("type").map(Value::as_str) {
        None => {
            value.insert("type".to_owned(), Value::from(derived.as_str()));
        }
        Some(Some(declared)) if declared == derived.as_str() => {}
        Some(declared) => refusals.push(format!(
            "{at}: {name} is declared {}, and the statement gives {}",
            declared.unwrap_or("a non-string type"),
            derived.as_str()
        )),
    }
}

/// Migrate the verification database to the package's dependency closure: the
/// base packages' migrations first, then the package's, then the history
/// tables of the relations the package keeps a log of.
async fn migrate(
    database_url: &str,
    package_root: &Path,
    manifest: &ManifestSources,
    bases: &[PackageManifest],
    output_root: &Path,
) -> Result<()> {
    let mut schemas = manifest.schemas();
    let mut directories = Vec::new();
    for base in bases {
        schemas.extend(base.models.values().map(|model| model.schema.clone()));
        directories.push(package_output(output_root, &base.package.id).join("migrations"));
    }
    schemas.sort();
    schemas.dedup();
    directories.push(package_root.join("migrations"));

    let client = wamn_postgres::pool(database_url, 4)
        .await
        .context("connect to the verification database")?;
    let result = async {
        if !schemas.is_empty() {
            let quoted = schemas
                .iter()
                .map(|schema| postgres_protocol::escape::escape_identifier(schema))
                .collect::<Vec<_>>();
            let mut sql = String::new();
            for schema in &quoted {
                writeln!(sql, "CREATE SCHEMA {schema};").expect("writing to a String cannot fail");
            }
            write!(
                sql,
                "ALTER DATABASE {} SET search_path TO {}, public",
                postgres_protocol::escape::escape_identifier(database_name(database_url)?.as_str()),
                quoted.join(", ")
            )
            .expect("writing to a String cannot fail");
            sqlx::raw_sql(AssertSqlSafe(sql.clone()).into_sql_str()).execute(&client)
                .await
                .context("create the package schemas")?;
        }
        // Public platform identity keys support application foreign keys.
        // These verification relations contain no system database or registry data.
        sqlx::raw_sql(AssertSqlSafe(("CREATE SCHEMA IF NOT EXISTS platform; CREATE TABLE IF NOT EXISTS platform.external_system (id uuid PRIMARY KEY); CREATE TABLE IF NOT EXISTS platform.file (id uuid PRIMARY KEY)").to_owned()).into_sql_str()).execute(&client).await.context("create platform references for package verification")?;
        for directory in directories {
            if !directory.exists() {
                continue;
            }
            let mut files = fs::read_dir(&directory)
                .with_context(|| format!("read migrations {}", directory.display()))?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| format!("read migrations {}", directory.display()))?;
            files.retain(|path| path.extension().is_some_and(|extension| extension == "sql"));
            files.sort();
            for file in files {
                let sql = fs::read_to_string(&file)
                    .with_context(|| format!("read migration {}", file.display()))?;
                sqlx::raw_sql(AssertSqlSafe(sql.clone()).into_sql_str()).execute(&client)
                    .await
                    .with_context(|| format!("apply migration {}", file.display()))?;
            }
        }
        create_history_tables(&client, manifest).await
    }
    .await;
    client.close().await;
    result
}

/// Create the history table of each relation the package keeps a log of.
async fn create_history_tables(client: &sqlx::PgPool, manifest: &ManifestSources) -> Result<()> {
    let logged = manifest
        .models
        .values()
        .filter(|model| model.owner == manifest.package.id && model.log_retention().is_some())
        .collect::<Vec<_>>();
    if logged.is_empty() {
        return Ok(());
    }
    sqlx::raw_sql(
        AssertSqlSafe(
            ("DO $application_role$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app NOLOGIN; \
               END IF; \
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; \
             END $application_role$")
                .to_owned(),
        )
        .into_sql_str(),
    )
    .execute(client)
    .await
    .context("create the application role")?;
    let history = sqlx::raw_sql(AssertSqlSafe((RECORD_HISTORY_SQL).to_owned()).into_sql_str())
        .execute(client)
        .await;
    // The frozen bootstrap's advisory lock is database-local, but roles are
    // cluster-wide. A concurrent creator has committed before this collision
    // returns; the failed implicit transaction left no history DDL behind.
    if history
        .as_ref()
        .err()
        .and_then(sqlx::Error::as_database_error)
        .is_some_and(|error| {
            error.code().as_deref() == Some("23505")
                && error.constraint() == Some("pg_authid_rolname_index")
        })
    {
        sqlx::raw_sql(AssertSqlSafe((RECORD_HISTORY_SQL).to_owned()).into_sql_str())
            .execute(client)
            .await
    } else {
        history
    }
    .context("apply record-history.sql")?;
    sqlx::raw_sql(AssertSqlSafe((RECORD_HISTORY_APP_GRANTS_SQL).to_owned()).into_sql_str())
        .execute(client)
        .await
        .context("apply record-history-app-grants.sql")?;
    for model in logged {
        sqlx::raw_sql(
            AssertSqlSafe(wamn_schema_control::physical_schema::history_table_sql(
                &model.into(),
            ))
            .into_sql_str(),
        )
        .execute(client)
        .await
        .with_context(|| format!("create history table of {}.{}", model.schema, model.table))?;
    }
    Ok(())
}

fn database_name(database_url: &str) -> Result<String> {
    Ok(wamn_postgres::identity(database_url)?.1)
}

/// A database that one build creates on the server and drops at its end.
struct VerificationDatabase {
    server_url: String,
    name: String,
    url: String,
}

impl VerificationDatabase {
    async fn create(server_url: &str) -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("read the clock")?
            .as_nanos();
        let name = format!(
            "wamn_build_{}_{nanos}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let url = wamn_postgres::database_configuration(server_url, &name)?;
        execute_on_server(
            server_url,
            &format!(
                "CREATE DATABASE {}",
                postgres_protocol::escape::escape_identifier(&name)
            ),
        )
        .await
        .context("create the verification database")?;
        Ok(Self {
            server_url: server_url.to_owned(),
            name,
            url,
        })
    }

    async fn drop(self) -> Result<()> {
        execute_on_server(
            &self.server_url,
            &format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                postgres_protocol::escape::escape_identifier(&self.name)
            ),
        )
        .await
        .with_context(|| format!("drop the verification database {}", self.name))
    }
}

async fn execute_on_server(server_url: &str, sql: &str) -> Result<()> {
    let client = wamn_postgres::pool(server_url, 4)
        .await
        .context("connect to the PostgreSQL server")?;
    let result = sqlx::raw_sql(AssertSqlSafe((sql).to_owned()).into_sql_str())
        .execute(&client)
        .await;
    client.close().await;
    result.map(|_| ()).with_context(|| format!("execute {sql}"))
}

/// Write the build output: every generated file, the canonical `wamn.json`,
/// and the captured authored inputs. The completed record is written by xtask
/// after component compilation.
fn write_outputs(
    package_output: &Path,
    canonical: &[u8],
    files: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<BuildOutputs> {
    let mut outputs = files
        .iter()
        .map(|(path, bytes)| (path.clone(), bytes.as_slice()))
        .collect::<BTreeMap<_, _>>();
    outputs.insert(PathBuf::from(COMPILED_MANIFEST_NAME), canonical);

    write_files(package_output, &outputs)?;
    let mut digests = BTreeMap::new();
    for (relative, bytes) in &outputs {
        let key = relative
            .to_str()
            .with_context(|| format!("output path is not UTF-8: {}", relative.display()))?
            .to_owned();
        digests.insert(key, bytes_sha256(bytes));
    }
    Ok(BuildOutputs {
        manifest: bytes_sha256(canonical),
        files: digests,
        components: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::describe::DescribedColumn;

    #[tokio::test]
    async fn verification_migration_accepts_a_platform_file_reference() {
        let manifest = json!({
            "package": {"id": "files_fixture", "version": "0.1.0"},
            "required_platform_policy_contract": {"id": "files_fixture_data_access", "state": "satisfied"},
            "connections": {},
            "components": {},
            "models": {"attachment": {
                "schema": "files_fixture", "table": "attachment", "owner": "files_fixture",
                "audit_log": {"columns": ["created_at"], "retention": "none"},
                "operations": {}
            }}
        });
        let manifest =
            PackageManifest::from_slice(&serde_json::to_vec(&manifest).unwrap()).unwrap();
        let root = std::env::temp_dir().join(format!("wamn-file-reference-{}", std::process::id()));
        fs::create_dir_all(root.join("migrations")).unwrap();
        fs::write(root.join("migrations/0001_initial.sql"),
            "CREATE TABLE files_fixture.attachment (id uuid CONSTRAINT attachment_id_pkey PRIMARY KEY, file_id uuid CONSTRAINT attachment_file_id_fkey REFERENCES platform.file(id), created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP)").unwrap();
        let database = wamn_test_postgres::database();
        let result = migrate(
            database.url(),
            &root,
            &ManifestSources::from_document(&serde_json::to_value(&manifest).unwrap()).unwrap(),
            &[],
            &default_output_root(&root),
        )
        .await;
        fs::remove_dir_all(root).unwrap();
        result.expect("the verification database permits a platform.file foreign key");
        let client = wamn_postgres::pool(database.url(), 4).await.unwrap();
        wamn_schema_introspection::postgres::read_catalog(&client, &["files_fixture"])
            .await
            .unwrap();
        let error = sqlx::raw_sql(AssertSqlSafe(("INSERT INTO files_fixture.attachment(id, file_id) VALUES (gen_random_uuid(), gen_random_uuid())").to_owned()).into_sql_str()).execute(&client).await.unwrap_err();
        assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("23503")
        );
        client.close().await;
    }

    #[tokio::test]
    async fn paged_query_warns_for_each_allowed_sort_field_without_an_index() {
        let database = wamn_test_postgres::database();
        let root = std::env::temp_dir().join(format!("wamn-paged-index-{}", std::process::id()));
        fs::create_dir(&root).expect("create the test package");
        fs::create_dir(root.join("migrations")).expect("create the migrations directory");
        let manifest = json!({
            "package": {"id": "sort_fixture", "version": "0.1.0"},
            "required_platform_policy_contract": {"id": "sort_fixture_data_access", "state": "satisfied"},
            "connections": {},
            "components": {"fixture": {"connections": ["postgres"]}},
            "models": {"order_row": {
                "schema": "receiving", "table": "order_row", "owner": "sort_fixture",
                "server_owned_fields": [],
                "audit_log": {"columns": ["created_at"], "retention": "none"},
                "operations": {"query": {
                    "feed": false,
                    "permission": "order_row.query", "result": "page",
                    "sort": {"fields": ["created_at", "status"], "directions": ["ascending", "descending"]},
                    "pagination": {"default_sort": {"field": "created_at", "direction": "ascending"}, "tie_breaker": {"field": "id"}},
                    "limit": {"default": 100, "minimum": 1, "maximum": 100}
                }}
            }}
        });
        fs::write(
            root.join("wamn.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .expect("write the package manifest");
        fs::write(
            root.join("migrations/0001_initial.sql"),
            "CREATE TABLE receiving.order_row (
                 id uuid CONSTRAINT order_row_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
                 status text NOT NULL DEFAULT 'open',
                 created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
             );",
        )
        .expect("write the table migration");
        fs::write(
            root.join("migrations/0002_default_index.sql"),
            "CREATE INDEX order_row_id_created_at_idx ON receiving.order_row (created_at, id);",
        )
        .expect("index the default sort only");
        let request = BuildRequest::new(&root, &root.join("target"), database.url());
        let built = generate_package(&request).await;
        let parsed = PackageManifest::from_slice(&serde_json::to_vec(&manifest).unwrap())
            .expect("parse the test manifest");
        let warned = missing_indexes(&root, &parsed).await;
        fs::write(
            root.join("migrations/0003_status_index.sql"),
            "CREATE INDEX order_row_id_status_idx ON receiving.order_row (status, id);",
        )
        .expect("index the other allowed sort in a later migration");
        let indexed = generate_package(&request).await;
        let silent = missing_indexes(&root, &parsed).await;
        fs::remove_dir_all(&root).expect("remove the test package");
        built.expect("a missing pagination index is a warning, not a refusal");
        assert_eq!(
            warned,
            vec![
                "sort-fixture:order-row/query@0.1.0 on receiving.order_row has no index for ORDER BY (status, id). \
                 Add to the package's migrations: CREATE INDEX order_row_id_status_idx ON receiving.order_row (status, id);"
                    .to_owned()
            ]
        );
        indexed.expect("the build reads both indexes from later package migrations");
        assert!(silent.is_empty(), "{silent:?}");
    }

    /// The warnings for the package, on a fresh database migrated with its
    /// migrations.
    async fn missing_indexes(root: &Path, manifest: &PackageManifest) -> Vec<String> {
        let database = wamn_test_postgres::database();
        migrate(
            database.url(),
            root,
            &ManifestSources::from_document(&serde_json::to_value(manifest).unwrap()).unwrap(),
            &[],
            &default_output_root(root),
        )
        .await
        .expect("migrate the test package");
        let catalog = introspect_with_manifest(database.url(), manifest, &[])
            .await
            .expect("introspect the migrated package");
        missing_paged_query_indexes(database.url(), &catalog, manifest)
            .await
            .expect("list the missing indexes")
    }

    fn manifest(row_nullable: bool, row_type: Option<&str>) -> Value {
        let mut row = json!({"name": "quantity", "nullable": row_nullable});
        if let Some(row_type) = row_type {
            row["type"] = json!(row_type);
        }
        json!({"custom_operations": {"adjust": {"statements": {"select_quantity": {
            "path": "command/adjust/select_quantity.sql",
            "fetch": "one",
            "parameters": [{"name": "packaging_id", "nullable": false}],
            "row": [row],
        }}}}})
    }

    fn described(nullable: Option<bool>) -> BTreeMap<String, DescribedStatement> {
        BTreeMap::from([(
            "command/adjust/select_quantity.sql".to_owned(),
            DescribedStatement {
                parameters: vec!["UUID".to_owned()],
                columns: vec![DescribedColumn {
                    name: "quantity".to_owned(),
                    type_name: "INT8".to_owned(),
                    nullable,
                    view_column: false,
                }],
            },
        )])
    }

    #[test]
    fn an_undeclared_type_is_derived() {
        let mut document = manifest(false, None);
        apply_statement_types(&mut document, &described(Some(false))).expect("derive the types");
        let statement = &document["custom_operations"]["adjust"]["statements"]["select_quantity"];
        assert_eq!(statement["parameters"][0]["type"], "uuid");
        assert_eq!(statement["row"][0]["type"], "int64");
    }

    #[test]
    fn a_nullable_declaration_over_a_non_null_column_is_kept() {
        let mut document = manifest(true, Some("int64"));
        apply_statement_types(&mut document, &described(Some(false)))
            .expect("keep the declaration");
        let row = &document["custom_operations"]["adjust"]["statements"]["select_quantity"]["row"];
        assert_eq!(row[0]["nullable"], true);
    }

    #[test]
    fn a_not_null_declaration_over_a_nullable_column_is_refused() {
        let mut document = manifest(false, Some("int64"));
        let error = apply_statement_types(&mut document, &described(Some(true)))
            .expect_err("a not-null declaration on a nullable column was accepted");
        assert!(error.to_string().contains("declared not null"), "{error:#}");
    }

    #[test]
    fn a_not_null_declaration_stands_where_sqlx_does_not_know() {
        let mut document = manifest(false, Some("int64"));
        apply_statement_types(&mut document, &described(None)).expect("keep the declaration");
        let row = &document["custom_operations"]["adjust"]["statements"]["select_quantity"]["row"];
        assert_eq!(row[0]["nullable"], false);
    }

    #[test]
    fn an_unstated_nullability_is_derived() {
        for (described_nullable, row_nullable) in
            [(Some(true), true), (Some(false), false), (None, true)]
        {
            let mut document = manifest(false, None);
            let statement =
                &mut document["custom_operations"]["adjust"]["statements"]["select_quantity"];
            for member in ["parameters", "row"] {
                statement[member][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("nullable");
            }
            apply_statement_types(&mut document, &described(described_nullable))
                .expect("derive the nullability");
            let statement =
                &document["custom_operations"]["adjust"]["statements"]["select_quantity"];
            assert_eq!(statement["parameters"][0]["nullable"], false);
            assert_eq!(statement["row"][0]["nullable"], row_nullable);
        }
    }

    #[test]
    fn a_different_declared_type_is_refused() {
        let mut other_type = manifest(false, Some("int32"));
        let error = apply_statement_types(&mut other_type, &described(Some(false)))
            .expect_err("a different declared type was accepted");
        assert!(
            error
                .to_string()
                .contains("quantity is declared int32, and the statement gives int64"),
            "{error:#}"
        );
    }

    #[test]
    fn view_columns_keep_declared_nullability_and_require_the_reported_type() {
        for nullable in [false, true] {
            let mut description = described(Some(true));
            description.values_mut().next().unwrap().columns[0].view_column = true;
            let mut document = manifest(nullable, Some("int64"));
            apply_statement_types(&mut document, &description).expect("accept a view column");
            assert_eq!(
                document["custom_operations"]["adjust"]["statements"]["select_quantity"]["row"][0]
                    ["nullable"],
                nullable
            );
            let mut document = manifest(nullable, Some("text"));
            let error = apply_statement_types(&mut document, &description).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("quantity is declared text, and the statement gives int64"),
                "{error:#}"
            );
        }
    }
}

/// Source metadata used before the database describes statement values.
#[derive(Debug, serde::Deserialize)]
struct ManifestSources {
    package: crate::manifest::PackageIdentity,
    #[serde(default)]
    base_dependencies: BTreeMap<String, crate::manifest::BaseDependencyRequirement>,
    models: BTreeMap<String, crate::manifest::ModelDeclaration>,
    #[serde(default)]
    custom_operations: BTreeMap<String, OperationSources>,
}

#[derive(Debug, serde::Deserialize)]
struct OperationSources {
    #[serde(default)]
    statements: BTreeMap<String, StatementSource>,
}

#[derive(Debug, serde::Deserialize)]
struct StatementSource {
    path: String,
}

impl ManifestSources {
    fn from_document(document: &Value) -> Result<Self> {
        Ok(serde::Deserialize::deserialize(document)?)
    }

    fn schemas(&self) -> Vec<String> {
        self.models
            .values()
            .map(|model| model.schema.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn load_sql(&self, root: &Path) -> Result<Vec<crate::materialize::SourceFile>> {
        let paths = self
            .models
            .values()
            .flat_map(|model| model.operations.values())
            .filter_map(|operation| operation.authored_sql.as_ref())
            .flat_map(|sql| {
                std::iter::once(sql.default.as_str())
                    .chain(sql.variants.iter().map(|variant| variant.path.as_str()))
            })
            .chain(
                self.custom_operations
                    .values()
                    .flat_map(|operation| operation.statements.values())
                    .map(|statement| statement.path.as_str()),
            )
            .collect::<BTreeSet<_>>();
        crate::materialize::read_sql_sources(root, paths)
    }
}

//! `wamn build`: the two-pass package build (docs/plan/platform-deploy.md §7.1).
//!
//! Pass one compiles `wamn.k` to its authored form, migrates a fresh
//! verification database, describes every authored statement through SQLx,
//! derives the statement types and relations, and emits the canonical
//! `wamn.json` (RFC 8785, hashed once). Pass two generates every consumer from
//! those bytes and writes them, with `build.json`, to the package's build
//! output directory. Step 7 compiles the package's components
//! ([`build_components`]), and step 8 builds the web client of a package that
//! has one into `web/` of the same directory.
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
use tokio_postgres::NoTls;
use wamn_execution_contract::canonical_json_bytes;
use wamn_schema_introspection::ir::CatalogIr;
use wamn_schema_introspection::postgres::authored_object_name;

use crate::authoring::{COMPILED_MANIFEST_NAME, compile_authored_manifest, is_authored};
pub use crate::components::{build_components, build_guest};
use crate::data_access::application_schemas;
use crate::describe::{DescribedStatement, column_type, describe_statements};
use crate::generate::sha256;
use crate::materialize::{
    classify_statements, client_bindings, existing_files, expected_files, generate_from_manifest,
    introspect_with_manifest, load_authored_sql, statement_corpus_from_manifest, write_files,
};
use crate::receipt::{
    BuildReceipt, RECEIPT_FILE, RECEIPT_SCHEMA, ReceiptDependency, ReceiptOutputs, ReceiptPackage,
};
use crate::{AuthoredSql, CrudAction, PackageManifest, ResultClass, canonical_operation_identity};

const RECORD_HISTORY_SQL: &str = include_str!("../../../../deploy/sql/record-history.sql");
const RECORD_HISTORY_APP_GRANTS_SQL: &str =
    include_str!("../../../../deploy/sql/record-history-app-grants.sql");
/// The hand-written manifest of a package that is not authored in `wamn.k`.
const HAND_WRITTEN_MANIFEST: &str = "wamn.json";
/// The type that stands in for an undeclared statement value type while the
/// build reads facts that do not depend on it.
const PLACEHOLDER_TYPE: &str = "text";
/// The web client's source directory, relative to the package root.
const WEB_SOURCE: &str = "web";
/// The web client build, relative to the package's build output directory
/// (§7.1 step 8, C10).
pub const WEB_OUTPUT: &str = "web";

/// One package build.
#[derive(Debug, Clone)]
pub struct BuildRequest {
    pub(crate) package_root: PathBuf,
    pub(crate) output_root: PathBuf,
    database_url: String,
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

/// Build one package and return its receipt, which is also written to
/// `build.json` in the package's build output directory: [`generate_package`],
/// then [`build_components`].
///
/// # Errors
///
/// When [`generate_package`] or [`build_components`] refuses.
pub async fn build_package(request: &BuildRequest) -> Result<BuildReceipt> {
    if let Some(receipt) = current_build(request)? {
        crate::materialize::materialize_client_consumers(
            &request.package_root,
            &receipt.package.id,
            &package_output(&request.output_root, &receipt.package.id).join("client-ts"),
        )?;
        eprintln!("{} is current", receipt.package.id);
        return Ok(receipt);
    }
    let receipt = generate_package(request).await?;
    build_components(request, &receipt.package.id).await
}

/// Find the matching inputs without compiling KCL or writing temporary files.
fn current_build(request: &BuildRequest) -> Result<Option<BuildReceipt>> {
    let directories = match fs::read_dir(&request.output_root) {
        Ok(directories) => directories,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read the package build output root"),
    };
    let digest = crate::receipt::input_digest(&request.package_root)?;
    for directory in directories {
        let output = directory?.path();
        let Ok(receipt) = crate::receipt::read(&output) else {
            continue;
        };
        if receipt.inputs.digest != digest
            // A receipt written before the web build must not make an unfinished build current.
            || (request.package_root.join("web/package.json").is_file()
                && receipt.outputs.web.is_none())
            || !receipt
                .outputs
                .files
                .keys()
                .all(|path| output.join(path).is_file())
            || !receipt
                .outputs
                .components
                .iter()
                .all(|component| output.join(&component.path).is_file())
            || receipt.outputs.web.as_ref().is_some_and(|digest| {
                web_digest(&output.join(WEB_OUTPUT)).as_ref().ok() != Some(digest)
            })
            || !receipt.dependencies.iter().all(|dependency| {
                crate::receipt::read_current(
                    &crate::output::dependency_root(&request.package_root, &dependency.id),
                    &package_output(&request.output_root, &dependency.id),
                )
                .is_ok_and(|base| {
                    base.package.version == dependency.version
                        && base.outputs.manifest == dependency.digest
                })
            })
        {
            continue;
        }
        // Generation writes a receipt before component compilation. Do not
        // accept that intermediate result after a failed component build.
        if crate::output::package_workspace(&request.package_root)
            .join("Cargo.toml")
            .is_file()
        {
            let manifest =
                PackageManifest::from_slice(&fs::read(output.join(COMPILED_MANIFEST_NAME))?)?;
            if !manifest.components.keys().all(|name| {
                receipt
                    .outputs
                    .components
                    .iter()
                    .any(|component| &component.name == name)
            }) {
                continue;
            }
            crate::components::base_components(
                &request.package_root,
                &request.output_root,
                &manifest,
            )?;
        }
        return Ok(Some(receipt));
    }
    Ok(None)
}

/// Every step of [`build_package`] except the component build (step 7): the
/// receipt it writes records no component.
///
/// # Errors
///
/// When the manifest does not compile, when a base package has no current
/// receipt, when the verification database cannot be created or migrated, when
/// a statement does not describe or a declared statement type is wider than
/// the described one, when generation refuses, or when the output cannot be
/// written.
pub async fn generate_package(request: &BuildRequest) -> Result<BuildReceipt> {
    let package_root = request.package_root.as_path();
    let inputs = crate::receipt::inputs(package_root)?;
    let mut authored: Value = serde_json::from_slice(&authored_manifest(package_root)?)
        .context("parse the authored manifest")?;
    let provisional =
        PackageManifest::from_slice(&serde_json::to_vec(&with_placeholder_types(&authored))?)
            .context("parse the authored manifest")?;
    let dependencies = dependencies(package_root, &request.output_root, &provisional)?;
    let bases =
        crate::inheritance::load_base_manifests(package_root, &request.output_root, &provisional)?;

    let database = VerificationDatabase::create(&request.database_url).await?;
    let built = pass_one_and_two(
        package_root,
        &database.url,
        &provisional,
        &mut authored,
        &bases,
    )
    .await;
    database.drop().await?;
    let (canonical, files) = built?;

    let package_output = package_output(&request.output_root, &provisional.package.id);
    let outputs = write_outputs(&package_output, &canonical, &files)?;
    let mut receipt = BuildReceipt {
        schema: RECEIPT_SCHEMA.to_owned(),
        package: ReceiptPackage {
            id: provisional.package.id.clone(),
            version: provisional.package.version.clone(),
        },
        inputs,
        dependencies,
        generator: crate::receipt::generator(),
        outputs,
    };
    let path = package_output.join(RECEIPT_FILE);
    // Client copying and the web build read generated output, so its receipt must exist first.
    fs::write(&path, receipt.to_bytes()).with_context(|| format!("write {}", path.display()))?;
    crate::materialize::materialize_client_consumers(
        package_root,
        &provisional.package.id,
        &package_output.join("client-ts"),
    )?;
    receipt.outputs.web = build_web(package_root, &package_output).await?;
    fs::write(&path, receipt.to_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(receipt)
}

/// The two passes against the migrated verification database: the canonical
/// manifest bytes and the generated files, by path relative to the package's
/// build output directory.
async fn pass_one_and_two(
    package_root: &Path,
    database_url: &str,
    provisional: &PackageManifest,
    authored: &mut Value,
    bases: &[PackageManifest],
) -> Result<(Vec<u8>, BTreeMap<PathBuf, Vec<u8>>)> {
    // Pass one.
    migrate(database_url, package_root, provisional).await?;
    let sources = load_authored_sql(package_root, provisional)?;
    let corpus = sources
        .iter()
        .map(|source| (source.path.clone(), source.bytes.clone()))
        .collect::<BTreeMap<_, _>>();
    let described = describe_statements(database_url, &corpus).await?;
    apply_statement_types(authored, &described)?;
    let typed = serde_json::to_vec(authored).context("serialize the typed manifest")?;
    let typed = crate::inheritance::complete_manifest(&typed, bases)?;
    let manifest = PackageManifest::from_slice(&typed).context("parse the typed manifest")?;
    let catalog = introspect_with_manifest(database_url, package_root, &manifest).await?;
    // The build refuses correctness only (owner ruling 2026-10-18): a missing
    // pagination index is a warning.
    for warning in missing_paged_query_indexes(database_url, &catalog, &manifest).await? {
        eprintln!("warning: {warning}");
    }
    let authored_sql = sources
        .iter()
        .map(|source| AuthoredSql::new(&source.path, &source.bytes))
        .collect::<Vec<_>>();
    let derived = crate::derive_manifest(&catalog, &typed, &authored_sql)
        .context("derive the statement relations")?;
    let canonical = canonical_json_bytes(
        &serde_json::from_slice(&derived).context("parse the derived manifest")?,
    );

    // Pass two: every consumer from the canonical bytes.
    let manifest =
        PackageManifest::from_slice(&canonical).context("parse the canonical manifest")?;
    let (corpus, grants) =
        statement_corpus_from_manifest(&catalog, &canonical, &manifest, package_root, bases)?;
    let schemas = application_schemas(&manifest).context("resolve application schemas")?;
    let (mut client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect to plan the package statements")?;
    let connection_task = tokio::spawn(connection);
    let verdicts = classify_statements(&mut client, &corpus, &schemas, &grants).await;
    drop(client);
    connection_task
        .await
        .context("join PostgreSQL connection task")?
        .context("drive PostgreSQL connection")?;
    let package = generate_from_manifest(
        &catalog,
        &canonical,
        &manifest,
        package_root,
        &verdicts?,
        bases,
    )?;
    let client = client_bindings(package_root, &manifest, &package)?;
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
    Ok((canonical, files))
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
            let (client, connection) = tokio_postgres::connect(database_url, NoTls)
                .await
                .context("connect to name the missing pagination index")?;
            let task = tokio::spawn(connection);
            let columns = client
                .query(
                    "SELECT attribute.attname::text
                     FROM pg_catalog.pg_attribute AS attribute
                     JOIN pg_catalog.pg_class AS relation ON relation.oid = attribute.attrelid
                     JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace
                     WHERE namespace.nspname = $1 AND relation.relname = $2
                       AND attribute.attnum > 0 AND NOT attribute.attisdropped
                       AND attribute.attname::text = ANY($3::text[])
                     ORDER BY attribute.attnum",
                    &[&model.schema, &model.table, &vec![field, tie_breaker]],
                )
                .await;
            drop(client);
            task.await
                .context("join the index naming connection")?
                .context("drive the index naming connection")?;
            let columns = columns
                .context("read the pagination index columns in table-definition order")?
                .iter()
                .map(|row| row.get::<_, String>(0))
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

/// The authored manifest bytes: `wamn.k` compiled, or a hand-written `wamn.json`.
fn authored_manifest(package_root: &Path) -> Result<Vec<u8>> {
    if is_authored(package_root) {
        compile_authored_manifest(package_root)
    } else {
        let path = package_root.join(HAND_WRITTEN_MANIFEST);
        fs::read(&path).with_context(|| format!("read {}", path.display()))
    }
}

/// The base packages of an overlay, from the receipts of their builds.
fn dependencies(
    package_root: &Path,
    output_root: &Path,
    manifest: &PackageManifest,
) -> Result<Vec<ReceiptDependency>> {
    manifest
        .base_dependencies
        .values()
        .map(|dependency| {
            let base = crate::receipt::read_current(
                &crate::output::dependency_root(package_root, &dependency.package),
                &package_output(output_root, &dependency.package),
            )
            .with_context(|| format!("build the base package {} first", dependency.package))?;
            ensure!(
                base.package.id == dependency.package && base.package.version == dependency.version,
                "the base build is {} {}, and the package pins {} {}",
                base.package.id,
                base.package.version,
                dependency.package,
                dependency.version
            );
            Ok(ReceiptDependency {
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

/// A copy of the authored manifest in which every statement value states a
/// type, so that it parses for the facts that do not depend on those types.
fn with_placeholder_types(authored: &Value) -> Value {
    let mut copy = authored.clone();
    for_each_statement(&mut copy, |_, statement| {
        for member in ["parameters", "row"] {
            for value in values_mut(statement, member) {
                value
                    .entry("type")
                    .or_insert_with(|| Value::from(PLACEHOLDER_TYPE));
            }
        }
        Ok(())
    })
    .expect("the placeholder visit refuses nothing");
    copy
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
                Some(Some(false)) if column.nullable == Some(true) => refusals.push(format!(
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
    manifest: &PackageManifest,
) -> Result<()> {
    let mut schemas = application_schemas(manifest).context("resolve application schemas")?;
    let mut directories = Vec::new();
    for dependency in manifest.base_dependencies.values() {
        let root = crate::output::dependency_root(package_root, &dependency.package);
        let authored: Value = serde_json::from_slice(&authored_manifest(&root)?)?;
        let base =
            PackageManifest::from_slice(&serde_json::to_vec(&with_placeholder_types(&authored))?)?;
        schemas.extend(application_schemas(&base)?);
        directories.push(root.join("migrations"));
    }
    schemas.sort();
    schemas.dedup();
    directories.push(package_root.join("migrations"));

    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect to the verification database")?;
    let task = tokio::spawn(connection);
    let result = async {
        if !schemas.is_empty() {
            let quoted = schemas
                .iter()
                .map(|schema| quote(schema))
                .collect::<Vec<_>>();
            let mut sql = String::new();
            for schema in &quoted {
                writeln!(sql, "CREATE SCHEMA {schema};").expect("writing to a String cannot fail");
            }
            write!(
                sql,
                "ALTER DATABASE {} SET search_path TO {}, public",
                quote(database_name(database_url)?.as_str()),
                quoted.join(", ")
            )
            .expect("writing to a String cannot fail");
            client
                .batch_execute(&sql)
                .await
                .context("create the package schemas")?;
        }
        // Public platform identity keys support application foreign keys.
        // These verification relations contain no system database or registry data.
        client.batch_execute("CREATE SCHEMA IF NOT EXISTS platform; CREATE TABLE IF NOT EXISTS platform.external_system (id uuid PRIMARY KEY); CREATE TABLE IF NOT EXISTS platform.file (id uuid PRIMARY KEY)").await.context("create platform references for package verification")?;
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
                client
                    .batch_execute(&sql)
                    .await
                    .with_context(|| format!("apply migration {}", file.display()))?;
            }
        }
        create_history_tables(&client, manifest).await
    }
    .await;
    drop(client);
    task.await
        .context("join the migration connection")?
        .context("drive the migration connection")?;
    result
}

/// Create the history table of each relation the package keeps a log of.
async fn create_history_tables(
    client: &tokio_postgres::Client,
    manifest: &PackageManifest,
) -> Result<()> {
    let logged = manifest
        .models
        .values()
        .filter(|model| model.owner == manifest.package.id && model.log_retention().is_some())
        .collect::<Vec<_>>();
    if logged.is_empty() {
        return Ok(());
    }
    client
        .batch_execute(
            "DO $application_role$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app NOLOGIN; \
               END IF; \
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; \
             END $application_role$",
        )
        .await
        .context("create the application role")?;
    client
        .batch_execute(RECORD_HISTORY_SQL)
        .await
        .context("apply record-history.sql")?;
    client
        .batch_execute(RECORD_HISTORY_APP_GRANTS_SQL)
        .await
        .context("apply record-history-app-grants.sql")?;
    for model in logged {
        client
            .execute(
                "SELECT wamn_history.create_history_table($1, $2, false)",
                &[&model.schema, &model.table],
            )
            .await
            .with_context(|| format!("create history table of {}.{}", model.schema, model.table))?;
    }
    Ok(())
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn database_name(database_url: &str) -> Result<String> {
    let url = url::Url::parse(database_url).context("parse the database URL")?;
    Ok(url.path().trim_start_matches('/').to_owned())
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
        let mut url = url::Url::parse(server_url).context("parse the PostgreSQL server URL")?;
        url.set_path(&name);
        execute_on_server(server_url, &format!("CREATE DATABASE {}", quote(&name)))
            .await
            .context("create the verification database")?;
        Ok(Self {
            server_url: server_url.to_owned(),
            name,
            url: url.into(),
        })
    }

    async fn drop(self) -> Result<()> {
        execute_on_server(
            &self.server_url,
            &format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", quote(&self.name)),
        )
        .await
        .with_context(|| format!("drop the verification database {}", self.name))
    }
}

async fn execute_on_server(server_url: &str, sql: &str) -> Result<()> {
    let (client, connection) = tokio_postgres::connect(server_url, NoTls)
        .await
        .context("connect to the PostgreSQL server")?;
    let task = tokio::spawn(connection);
    let result = client.batch_execute(sql).await;
    drop(client);
    task.await
        .context("join the server connection")?
        .context("drive the server connection")?;
    result.with_context(|| format!("execute {sql}"))
}

/// Write the build output: every generated file, the canonical `wamn.json`,
/// and nothing else except the SQLx metadata. A file whose bytes are unchanged
/// keeps its modification time. The receipt goes first, so an interrupted
/// build leaves none.
fn write_outputs(
    package_output: &Path,
    canonical: &[u8],
    files: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<ReceiptOutputs> {
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
        digests.insert(key, sha256(bytes));
    }
    Ok(ReceiptOutputs {
        manifest: sha256(canonical),
        files: digests,
        components: Vec::new(),
        web: None,
    })
}

/// Step 8: `pnpm build` the web client of the package, when it has one, into
/// [`WEB_OUTPUT`] of its build output directory, and return the digest of the
/// built files. The web client resolves its client package in the default
/// output root, `target/wamn/<package>/client-ts`, which pass two writes when
/// the request names that root. `write_outputs` removed the previous web build.
async fn build_web(package_root: &Path, package_output: &Path) -> Result<Option<String>> {
    let source = package_root.join(WEB_SOURCE);
    if !source.join("package.json").is_file() {
        return Ok(None);
    }
    let web = std::path::absolute(package_output.join(WEB_OUTPUT))
        .with_context(|| format!("resolve {}", package_output.display()))?;
    let output = tokio::process::Command::new("pnpm")
        .current_dir(&source)
        .args(["run", "build", "--outDir"])
        .arg(&web)
        .arg("--emptyOutDir")
        .output()
        .await
        .with_context(|| format!("run pnpm build in {}", source.display()))?;
    ensure!(
        output.status.success(),
        "pnpm build in {} failed:\n{}{}",
        source.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(Some(web_digest(&web)?))
}

fn web_digest(web: &Path) -> Result<String> {
    let mut files = BTreeMap::new();
    for relative in existing_files(web)? {
        let path = web.join(&relative);
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let key = relative
            .to_str()
            .with_context(|| format!("web path is not UTF-8: {}", relative.display()))?
            .to_owned();
        files.insert(key, sha256(&bytes));
    }
    ensure!(
        !files.is_empty(),
        "the web build has no file in {}",
        web.display()
    );
    Ok(sha256(&canonical_json_bytes(&serde_json::json!(files))))
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
        let result = migrate(database.url(), &root, &manifest).await;
        fs::remove_dir_all(root).unwrap();
        result.expect("the verification database permits a platform.file foreign key");
        let (client, connection) = tokio_postgres::connect(database.url(), NoTls)
            .await
            .unwrap();
        let task = tokio::spawn(connection);
        wamn_schema_introspection::postgres::read_catalog(&client, &["files_fixture"])
            .await
            .unwrap();
        let error = client.batch_execute("INSERT INTO files_fixture.attachment(id, file_id) VALUES (gen_random_uuid(), gen_random_uuid())").await.unwrap_err();
        assert_eq!(
            error.code(),
            Some(&tokio_postgres::error::SqlState::FOREIGN_KEY_VIOLATION)
        );
        drop(client);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn current_build_preserves_files_and_changed_inputs_rebuild() {
        // Use the package built by the pagination-index test below.
        let manifest = json!({
            "package": {"id": "sort_fixture", "version": "0.1.0"},
            "required_platform_policy_contract": {"id": "sort_fixture_data_access", "state": "satisfied"},
            "connections": {},
            "components": {"fixture": {"connections": ["postgres"]}},
            "models": {"order_row": {
                "feed": false,
                "schema": "receiving", "table": "order_row", "owner": "sort_fixture",
                "server_owned_fields": [],
                "audit_log": {"columns": ["created_at"], "retention": "none"},
                "operations": {"query": {
                    "permission": "order_row.query", "result": "page",
                    "sort": {"fields": ["created_at", "status"], "directions": ["ascending", "descending"]},
                    "pagination": {"default_sort": {"field": "created_at", "direction": "ascending"}, "tie_breaker": {"field": "id"}},
                    "limit": {"default": 100, "minimum": 1, "maximum": 100}
                }}
            }}
        });
        let manifest = serde_json::to_vec(&manifest).unwrap();
        PackageManifest::from_slice(&manifest).expect("parse the test manifest");
        let database = wamn_test_postgres::database();
        let root = std::env::temp_dir().join(format!("wamn-current-build-{}", std::process::id()));
        fs::create_dir_all(root.join("package")).unwrap();
        let package = root.join("package");
        fs::write(package.join("wamn.json"), manifest).unwrap();
        fs::create_dir(package.join("migrations")).unwrap();
        fs::write(
            package.join("migrations/0001_initial.sql"),
            "CREATE TABLE receiving.order_row (
                 id uuid CONSTRAINT order_row_id_pkey PRIMARY KEY DEFAULT gen_random_uuid(),
                 status text NOT NULL DEFAULT 'open',
                 created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
             );",
        )
        .unwrap();
        fs::write(
            package.join("migrations/0002_default_index.sql"),
            "CREATE INDEX order_row_id_created_at_idx ON receiving.order_row (created_at, id);",
        )
        .unwrap();
        let request = BuildRequest::new(&package, &root.join("target"), database.url());
        let first = build_package(&request).await.expect("build the package");
        let output = package_output(&request.output_root, &first.package.id);
        let snapshot = || {
            existing_files(&output)
                .unwrap()
                .into_iter()
                .map(|relative| {
                    let path = output.join(&relative);
                    (
                        relative,
                        (
                            fs::read(&path).unwrap(),
                            fs::metadata(path).unwrap().modified().unwrap(),
                        ),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        };
        // Fix old mtimes so even an immediate identical rewrite fails the test.
        for relative in existing_files(&output).unwrap() {
            fs::File::options()
                .write(true)
                .open(output.join(relative))
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
                .unwrap();
        }
        let before = snapshot();
        let second = build_package(&request)
            .await
            .expect("reuse the current build");
        assert_eq!(first, second);
        assert_eq!(before, snapshot(), "a current build wrote an output file");
        fs::write(package.join("input.txt"), "changed input").unwrap();
        let rebuilt = build_package(&request)
            .await
            .expect("rebuild the changed input");
        assert_ne!(first.inputs.digest, rebuilt.inputs.digest);
        assert_ne!(
            before[Path::new(RECEIPT_FILE)],
            snapshot()[Path::new(RECEIPT_FILE)]
        );
        fs::remove_file(output.join(COMPILED_MANIFEST_NAME)).unwrap();
        build_package(&request)
            .await
            .expect("replace a missing output");
        assert!(output.join(COMPILED_MANIFEST_NAME).is_file());
        fs::create_dir(package.join("web")).unwrap();
        fs::write(package.join("web/package.json"), "{}").unwrap();
        let mut unfinished = crate::receipt::read(&output).unwrap();
        unfinished.inputs.digest = crate::receipt::input_digest(&package).unwrap();
        fs::write(output.join(RECEIPT_FILE), unfinished.to_bytes()).unwrap();
        assert!(current_build(&request).unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
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
                "feed": false,
                "schema": "receiving", "table": "order_row", "owner": "sort_fixture",
                "server_owned_fields": [],
                "audit_log": {"columns": ["created_at"], "retention": "none"},
                "operations": {"query": {
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
        migrate(database.url(), root, manifest)
            .await
            .expect("migrate the test package");
        let catalog = introspect_with_manifest(database.url(), root, manifest)
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
    fn placeholder_types_leave_declared_types_alone() {
        let copy = with_placeholder_types(&manifest(false, Some("int64")));
        let statement = &copy["custom_operations"]["adjust"]["statements"]["select_quantity"];
        assert_eq!(statement["parameters"][0]["type"], PLACEHOLDER_TYPE);
        assert_eq!(statement["row"][0]["type"], "int64");
    }
}

//! Filesystem and PostgreSQL-backed package materialization.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
#[cfg(test)]
use wamn_scalar::SqlValueType;

use anyhow::{Context as _, Result, ensure};
use sqlx::{AssertSqlSafe, Row as _, SqlSafeStr as _};
use wamn_schema_introspection::ir::CatalogIr;
use wamn_schema_introspection::postgres::read_catalog_excluding_relations;

use crate::StatementTransactionality;
use crate::authoring::{
    COMPILED_MANIFEST, compile_authored_manifest, is_authored, package_manifest_path,
};
use crate::client_component::emit_ts_components;
use crate::client_ir::{ClientContractIr, published_routes};
use crate::client_plan::ClientPlan;
use crate::client_ts::{emit_ts_client, emit_ts_package_json};
use crate::generate::GeneratedFile;
use crate::manifest::{CONTROL_OWNED_RELATION_TABLES, PackageManifest};
use crate::{AuthoredSql, GeneratedPackage, GenerationInput};

pub(crate) const GENERATOR_ID: &str = "wamn-compiler/0.1.13";
pub(crate) const TOOLCHAIN_ID: &str = "rust-1.99.0";

#[derive(Debug)]
pub(crate) struct SourceFile {
    pub(crate) path: String,
    pub(crate) bytes: Vec<u8>,
}

/// Materialize one package from its manifest, authored SQL, and migrated database.
///
/// `database_url` names the already-migrated PostgreSQL database to introspect.
pub async fn materialize_package(database_url: &str, package_root: &Path) -> Result<()> {
    materialize_after_introspection(package_root, introspect_package(database_url, package_root))
        .await
}

/// Introspect the package-owned schemas in one already-migrated PostgreSQL database.
pub async fn introspect_package(database_url: &str, package_root: &Path) -> Result<CatalogIr> {
    let (_, manifest) = load_manifest(package_root)?;
    let sources = load_authored_sql(package_root, &manifest)?;
    let authored = sources
        .iter()
        .map(|source| AuthoredSql::new(&source.path, &source.bytes))
        .collect::<Vec<_>>();
    introspect_with_manifest(database_url, &manifest, &authored).await
}

/// Introspect the schemas that `manifest` names, as [`introspect_package`] does.
pub(crate) async fn introspect_with_manifest(
    database_url: &str,
    manifest: &PackageManifest,
    authored_sql: &[AuthoredSql<'_>],
) -> Result<CatalogIr> {
    let schemas = application_schemas(manifest).context("resolve application schemas")?;

    let client = sqlx::PgPool::connect(database_url)
        .await
        .context("connect to the migrated database")?;
    let schema_names = schemas.iter().map(String::as_str).collect::<Vec<_>>();
    let mut selected = manifest
        .models
        .values()
        .map(|model| model.table.clone())
        .chain(
            manifest
                .internal_relations
                .values()
                .map(|relation| relation.table.clone()),
        )
        .collect::<BTreeSet<_>>();
    for source in authored_sql {
        let sql = std::str::from_utf8(source.bytes())
            .with_context(|| format!("{} is not UTF-8", source.path()))?;
        let parsed = pg_query::parse(sql).with_context(|| format!("parse {}", source.path()))?;
        selected.extend(parsed.tables.into_iter().map(|(table, _)| table));
    }
    let selected = selected.into_iter().collect::<Vec<_>>();
    let unselected = sqlx::query(AssertSqlSafe("SELECT n.nspname::text, c.relname::text FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = ANY($1::text[]) AND c.relname <> ALL($2::text[]) AND c.relkind IN ('r', 'v')".to_owned()).into_sql_str())
        .bind(&schemas).bind(selected).fetch_all(&client).await.context("select the package's catalog relations")?;
    let unselected = unselected
        .iter()
        .map(|row| (row.get::<String, _>(0), row.get::<String, _>(1)))
        .collect::<Vec<_>>();
    let excluded_relations = schema_names
        .iter()
        .flat_map(|schema| {
            CONTROL_OWNED_RELATION_TABLES
                .iter()
                .map(move |table| (*schema, *table))
        })
        .chain(
            unselected
                .iter()
                .map(|(schema, table)| (schema.as_str(), table.as_str())),
        )
        .collect::<Vec<_>>();
    let result = read_catalog_excluding_relations(&client, &schema_names, &excluded_relations)
        .await
        .context("introspect package schemas");
    client.close().await;
    result
}

/// Classify effects without executing statements. Authored routine calls are writes.
pub(crate) async fn classify_statements(
    database_url: &str,
    corpus: &BTreeMap<String, Vec<u8>>,
    schemas: &[String],
) -> Result<StatementTransactionality> {
    use sqlx::Connection as _;
    let calls = corpus
        .iter()
        .map(|(path, bytes)| {
            let calls = crate::sql_analysis::called_routines(bytes)
                .map_err(|detail| anyhow::anyhow!("{path}: {detail}"))?;
            Ok((path.as_str(), calls))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let referenced = calls
        .values()
        .flatten()
        .filter_map(|name| name.rsplit('.').next())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut connection = sqlx::PgConnection::connect(database_url)
        .await
        .context("connect to plan statements")?;
    let routines = sqlx::query(AssertSqlSafe("SELECT n.nspname::text, p.proname::text FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = ANY($1::text[]) AND p.proname = ANY($2::text[])".to_owned()).into_sql_str())
        .bind(schemas).bind(referenced).fetch_all(&mut connection).await.context("read called authored routine names")?;
    let names = routines
        .iter()
        .flat_map(|row| {
            let schema: String = row.get(0);
            let name: String = row.get(1);
            [format!("{schema}.{name}"), name]
        })
        .collect::<BTreeSet<_>>();
    let mut paths = BTreeMap::new();
    for (path, bytes) in corpus {
        let sql =
            std::str::from_utf8(bytes).with_context(|| format!("statement {path} is not UTF-8"))?;
        if calls[path.as_str()].iter().any(|name| names.contains(name)) {
            paths.insert(path.clone(), true);
            continue;
        }
        // The simple protocol lets PostgreSQL's generic planner supply the placeholders.
        let mut rows = sqlx::raw_sql(
            AssertSqlSafe(format!("EXPLAIN (GENERIC_PLAN, FORMAT JSON) {sql}")).into_sql_str(),
        )
        .fetch_all(&mut connection)
        .await
        .with_context(|| format!("plan statement {path}"))?;
        let row = rows
            .pop()
            .with_context(|| format!("planning {path} returned no plan"))?;
        let plan: serde_json::Value = row
            .try_get(0)
            .with_context(|| format!("read the plan of {path}"))?;
        paths.insert(path.clone(), plan_needs_transaction(&plan));
    }
    connection
        .close()
        .await
        .context("close the planning connection")?;
    Ok(StatementTransactionality::from_paths(paths))
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
pub fn materialize_package_from_catalog(catalog: &CatalogIr, package_root: &Path) -> Result<()> {
    materialize_package_classified(
        catalog,
        package_root,
        &StatementTransactionality::from_paths(BTreeMap::new()),
    )
}

/// Materialize with PostgreSQL's verdict on which statements need a transaction.
///
/// This entry point shares the SQL preparation path with the package build.
pub fn materialize_package_classified(
    catalog: &CatalogIr,
    package_root: &Path,
    transactional: &StatementTransactionality,
) -> Result<()> {
    let package = generate_package(catalog, package_root, transactional)?;
    let output_root = crate::output_root(package_root);
    let (_, manifest) = load_manifest(package_root)?;
    let client = client_bindings(package_root, &manifest, &package)?;
    let mut expected = expected_files(&package)?;
    // The compiled manifest is one more file of the owned set.
    let compiled_manifest = if is_authored(package_root) {
        let (bytes, _) = load_compiled_manifest(package_root, catalog)?;
        Some(bytes)
    } else {
        None
    };
    if let Some(bytes) = &compiled_manifest {
        let relative = Path::new(COMPILED_MANIFEST)
            .strip_prefix("generated")
            .context("the compiled manifest lies under generated/")?;
        expected.insert(relative.to_owned(), bytes.as_slice());
    }
    for file in &client {
        let relative = Path::new(file.path())
            .strip_prefix("generated")
            .with_context(|| format!("client binding escaped output root: {}", file.path()))?;
        expected.insert(relative.to_owned(), file.bytes());
    }

    write_files(&output_root, &expected)
}

/// The TypeScript bindings and components that this package declares.
///
/// Materialization owns these files with the rest of the generated output.
/// It removes files that it no longer produces.
/// Contracts and generated routes come from the package still in memory.
/// Authored routes come from `publication/attachments.json`.
/// Route templates are release facts, so bindings use them directly.
pub(crate) fn client_bindings(
    package_root: &Path,
    manifest: &PackageManifest,
    package: &GeneratedPackage,
) -> Result<Vec<GeneratedFile>> {
    const CONTRACTS: &str = "generated/contracts/";

    let contracts = package
        .files()
        .iter()
        .filter_map(|file| {
            file.path()
                .strip_prefix(CONTRACTS)
                .map(|relative| (relative.to_owned(), file.bytes().to_vec()))
        })
        .collect::<BTreeMap<_, _>>();
    // A route names its generated input schema, which this run produced and
    // has not written yet, so the reference resolves against these bytes.
    let routes = published_routes(
        &package_root.join("publication/attachments.json"),
        &mut |path| {
            package
                .file(path)
                .map(|file| crate::route_schema::parse(path, file.bytes()))
                .transpose()
        },
        &mut |reference| {
            let file = package
                .file(reference)
                .ok_or_else(|| crate::route_schema::RouteSchemaError::unknown(reference))?;
            crate::route_schema::parse(reference, file.bytes())
        },
    )
    .context("read the release's published routes")?;
    let ir = ClientContractIr::from_release_contracts(&manifest.package.id, &contracts, &routes)
        .context("project the client-contract IR")?;
    let mut files = Vec::new();
    // A package opts in to TypeScript by declaring the package name that a
    // browser application imports. Generation never infers one.
    if let Some(client) = &manifest.client_package {
        files.extend(emit_ts_client(&ir).context("emit the TypeScript client bindings")?);
        files.extend(
            emit_ts_components(&ClientPlan::from_ir(&ir)).context("emit the browser components")?,
        );
        files.push(emit_ts_package_json(
            &client.name,
            &manifest.package.version,
        ));
    }
    Ok(files)
}

fn generate_package(
    catalog: &CatalogIr,
    package_root: &Path,
    transactional: &StatementTransactionality,
) -> Result<crate::GeneratedPackage> {
    let (manifest_bytes, manifest) = load_compiled_manifest(package_root, catalog)?;
    generate_from_manifest(
        catalog,
        &manifest_bytes,
        &manifest,
        package_root,
        transactional,
    )
}

/// Generate from compiled manifest bytes that the caller holds. `manifest` is
/// their parse. The authored SQL is read from `package_root`.
pub(crate) fn generate_from_manifest(
    catalog: &CatalogIr,
    manifest_bytes: &[u8],
    manifest: &PackageManifest,
    package_root: &Path,
    transactional: &StatementTransactionality,
) -> Result<crate::GeneratedPackage> {
    let source_files = load_authored_sql(package_root, manifest)?;
    let authored_sql = source_files
        .iter()
        .map(|source| AuthoredSql::new(&source.path, &source.bytes))
        .collect::<Vec<_>>();

    crate::generate::generate_compiled(
        &GenerationInput::new(catalog, manifest_bytes, &authored_sql, transactional),
        manifest,
    )
    .context("generate package artifacts")
}

async fn materialize_after_introspection<F>(package_root: &Path, introspection: F) -> Result<()>
where
    F: std::future::Future<Output = Result<CatalogIr>>,
{
    let catalog = introspection.await?;
    materialize_package_from_catalog(&catalog, package_root)
}

/// Materialize against a live migrated database, asking the server which
/// statements need a transaction.
pub async fn materialize_package_verified(database_url: &str, package_root: &Path) -> Result<()> {
    let catalog = introspect_package(database_url, package_root).await?;
    materialize_package_verified_with_catalog(&catalog, database_url, package_root).await
}

/// Materialize a catalog the caller already holds, asking the server which
/// statements need a transaction.
///
/// A caller that shares one database across several packages must supply its
/// own catalog, narrowed to what this package owns. Re-introspecting here would
/// hand the package every relation and field its neighbours installed, and the
/// generated artifacts would declare them.
pub async fn materialize_package_verified_with_catalog(
    catalog: &CatalogIr,
    database_url: &str,
    package_root: &Path,
) -> Result<()> {
    let (manifest_bytes, manifest) = load_compiled_manifest(package_root, catalog)?;
    let sources = load_authored_sql(package_root, &manifest)?;
    let authored = sources
        .iter()
        .map(|source| AuthoredSql::new(&source.path, &source.bytes))
        .collect::<Vec<_>>();
    let preliminary = StatementTransactionality::from_paths(BTreeMap::new());
    let input = GenerationInput::new(catalog, &manifest_bytes, &authored, &preliminary);
    let corpus = crate::generate::sql_corpus(&input, &manifest)?;
    let schemas = application_schemas(&manifest)?;
    let mut verdicts = classify_statements(database_url, &corpus.statements, &schemas).await?;
    verdicts.described =
        crate::describe::describe_statements(database_url, &corpus.statements).await?;
    let input = GenerationInput::new(catalog, &manifest_bytes, &authored, &verdicts);
    let package = crate::generate::generate_described(&input, &manifest, &corpus)?;
    write_files(
        &crate::output_root(package_root),
        &expected_files(&package)?,
    )
}

/// The manifest bytes and their parse. A package authored in `wamn.k` compiles
/// it here, so generation reads the source and never the committed output.
fn load_manifest(package_root: &Path) -> Result<(Vec<u8>, PackageManifest)> {
    let manifest_bytes = if is_authored(package_root) {
        compile_authored_manifest(package_root)?
    } else {
        let manifest_path = package_manifest_path(package_root);
        fs::read(&manifest_path).with_context(|| format!("read {}", manifest_path.display()))?
    };
    let manifest = if is_authored(package_root) {
        PackageManifest::parse_authored(&manifest_bytes)
    } else {
        PackageManifest::from_slice(&manifest_bytes)
    }
    .context("parse package manifest")?;
    Ok((manifest_bytes, manifest))
}

/// The compiled manifest bytes and their parse. A package authored in `wamn.k`
/// compiles it here and adds the members that generation derives over this
/// catalog. Any other package reads its hand-written `wamn.json`.
fn load_compiled_manifest(
    package_root: &Path,
    catalog: &CatalogIr,
) -> Result<(Vec<u8>, PackageManifest)> {
    let (bytes, manifest) = load_manifest(package_root)?;
    if !is_authored(package_root) {
        return Ok((bytes, manifest));
    }
    let sources = load_authored_sql(package_root, &manifest)?;
    let authored_sql = sources
        .iter()
        .map(|source| AuthoredSql::new(&source.path, &source.bytes))
        .collect::<Vec<_>>();
    let bases = crate::inheritance::load_base_manifests(
        &crate::build::default_output_root(package_root),
        &manifest.base_dependencies,
    )?;
    let mut manifest = manifest;
    crate::generate::derive_model(catalog, &mut manifest, &authored_sql, &bases)
        .context("derive the compiled manifest")?;
    let compiled = wamn_digest::canonical_json_bytes(&serde_json::to_value(&manifest)?);
    Ok((compiled, manifest))
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

/// Write the generated TypeScript clients of the host routes
/// (docs/architecture/execution.md#host-run-routes).
///
/// The clients are build output, so Git does not hold them
/// (docs/architecture/overview.md#application-ownership). The contracts come
/// from [`wamn_catalog::HostRouteSet::contract_files`], which resolves each
/// set's shared shapes to one operation, input, result and errors file per
/// route. The routes come from the catalog's set, so each operation reaches the
/// path the host serves it at.
///
/// `output_root` is a build output root, [`crate::host_route_client_output_root`]
/// for the web packages. The application client goes to
/// `<output_root>/wamn_control/client-ts` and the control client to
/// `<output_root>/wamn_control/control/client-ts`. The resolved per-operation
/// contract files go beside each client in `contracts/`. A file there that the
/// contracts no longer generate is removed. When `output_root` lies in a pnpm
/// workspace, each client is then copied to the packages that name it,
/// `wamn_control` or `wamn_control_org`, in `"wamn": { "clients": [...] }`.
pub fn materialize_host_route_client(output_root: &Path) -> Result<()> {
    let output = output_root.join(wamn_catalog::host_route_package());
    materialize_host_route_set(&output, wamn_catalog::HostRouteSet::Application)?;
    materialize_host_route_set(&output.join("control"), wamn_catalog::HostRouteSet::Control)?;
    // `wamn build` does not make these clients, so they have no build record.
    copy_to_consumers(output_root, "wamn_control", &output.join("client-ts"))?;
    copy_to_consumers(
        output_root,
        "wamn_control_org",
        &output.join("control/client-ts"),
    )
}

/// One set's client. The two sets share operation ids, such as `user.list`,
/// so each set has its own contracts, output and package.
fn materialize_host_route_set(output: &Path, set: wamn_catalog::HostRouteSet) -> Result<()> {
    let contracts = set
        .contract_files()
        .context("resolve the host route contracts")?;
    write_build_output(
        &output.join("contracts"),
        &contracts
            .iter()
            .map(|(key, bytes)| (PathBuf::from(key), bytes.as_slice()))
            .collect(),
    )?;
    let routes = set
        .attachments()
        .map(|(_, attachment)| {
            let template = attachment.definition["route"]["path"]
                .as_str()
                .context("a host route names its path")?
                .to_owned();
            Ok((
                attachment.operation.clone(),
                crate::client_ir::RouteIr {
                    method: String::new(),
                    template,
                    input_schema: None,
                    terminal_operation: Some(attachment.operation.clone()),
                    direct: true,
                    response: crate::client_ir::ResponseIr::default(),
                    replay: None,
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let ir = ClientContractIr::from_release_contracts(
        wamn_catalog::host_route_package(),
        &contracts,
        &routes,
    )
    .context("project the client-contract IR of the host routes")?;
    for route in routes.keys() {
        ensure!(
            ir.models
                .iter()
                .flat_map(|model| &model.operations)
                .any(|operation| &operation.operation == route),
            "the host route {route} has no contract"
        );
    }
    let mut files = emit_ts_client(&ir).context("emit the TypeScript client bindings")?;
    files.push(emit_ts_package_json(
        set.client_package(),
        wamn_catalog::host_route_version(),
    ));
    let mut expected = BTreeMap::new();
    for file in &files {
        let relative = Path::new(file.path())
            .strip_prefix("generated/client-ts")
            .with_context(|| format!("client binding escaped output root: {}", file.path()))?;
        expected.insert(relative.to_owned(), file.bytes());
    }
    write_build_output(&output.join("client-ts"), &expected)
}

/// Copy an application's generated client into its declared workspace consumers.
///
/// Consumers receive source files only. Their own packages supply dependencies.
///
/// # Errors
///
/// Names the consumer and application when the requested client is unavailable.
pub fn materialize_client_consumers(package_root: &Path, app: &str, source: &Path) -> Result<()> {
    if let Some(output) = source.parent()
        && crate::output::is_build_output(output)
    {
        crate::build_record::read_current(package_root, output)?;
    }
    copy_to_consumers(package_root, app, source)
}

/// Copy the client at `source` into the packages of the workspace that holds
/// `within` and name `app` in their `"wamn": { "clients": [...] }`.
fn copy_to_consumers(within: &Path, app: &str, source: &Path) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Workspace {
        packages: Vec<PathBuf>,
    }
    #[derive(Default, serde::Deserialize)]
    struct Clients {
        #[serde(default)]
        clients: Vec<String>,
    }
    #[derive(serde::Deserialize)]
    struct Consumer {
        name: String,
        #[serde(default)]
        wamn: Clients,
    }

    let within = std::path::absolute(within)?;
    let Some(repository) = within
        .ancestors()
        .find(|path| path.join("pnpm-workspace.yaml").is_file())
    else {
        return Ok(());
    };
    let workspace: Workspace =
        serde_yaml::from_slice(&fs::read(repository.join("pnpm-workspace.yaml"))?)
            .context("read the workspace packages")?;
    for relative in workspace.packages {
        let directory = repository.join(relative);
        let manifest = directory.join("package.json");
        let consumer: Consumer = serde_json::from_slice(
            &fs::read(&manifest).with_context(|| format!("read {}", manifest.display()))?,
        )
        .with_context(|| format!("read {}", manifest.display()))?;
        if !consumer.wamn.clients.iter().any(|client| client == app) {
            continue;
        }
        let copy = || -> Result<()> {
            ensure!(
                source.join("index.ts").is_file(),
                "the generated client is unavailable"
            );
            let files = existing_files(source)?
                .into_iter()
                .filter(|path| path != Path::new("package.json"))
                .map(|path| Ok((path.clone(), fs::read(source.join(path))?)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            let expected = files
                .iter()
                .map(|(path, bytes)| (path.clone(), bytes.as_slice()))
                .collect();
            write_build_output(&directory.join("src/generated").join(app), &expected)
        };
        copy().with_context(|| {
            format!(
                "cannot produce client {app} for consuming package {}",
                consumer.name
            )
        })?;
    }
    Ok(())
}

/// Write `expected` below `directory` and remove every other file there.
///
/// Each web package writes the host route clients before it reads them, and
/// `pnpm -r` runs the packages at the same time. So a changed file is written
/// beside its path and renamed over it, and a reader never sees a partial
/// file. An unchanged file keeps its modification time.
fn write_build_output(directory: &Path, expected: &BTreeMap<PathBuf, &[u8]>) -> Result<()> {
    for relative in existing_files(directory)? {
        // A partial file belongs to a writer that is still running.
        let partial = relative
            .extension()
            .is_some_and(|extension| extension == "partial");
        if !partial && !expected.contains_key(&relative) {
            let path = directory.join(relative);
            // Another package's writer can remove the same file first.
            match fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error)
                        .with_context(|| format!("remove stale build output {}", path.display()));
                }
                _ => {}
            }
        }
    }
    for (relative, bytes) in expected {
        let path = directory.join(relative);
        if fs::read(&path).is_ok_and(|actual| actual.as_slice() == *bytes) {
            continue;
        }
        let parent = path
            .parent()
            .context("build output path must have a parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("create build output directory {}", parent.display()))?;
        let mut partial = path.clone().into_os_string();
        partial.push(format!(".{}.partial", std::process::id()));
        fs::write(&partial, *bytes)
            .with_context(|| format!("write build output {}", path.display()))?;
        fs::rename(&partial, &path)
            .with_context(|| format!("write build output {}", path.display()))?;
    }
    Ok(())
}

/// Replace generated output, retaining SQLx metadata prepared after generation.
/// An unchanged file keeps its modification time.
pub(crate) fn write_files(output_root: &Path, expected: &BTreeMap<PathBuf, &[u8]>) -> Result<()> {
    for stale in existing_files(output_root)? {
        if !expected.contains_key(&stale) && !stale.starts_with(".sqlx") {
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
        if !entry.file_type()?.is_dir() || entry.file_name() == ".sqlx" {
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use wamn_schema_introspection::ir::{Column, Constraint, Table};

    use super::*;

    const MANIFEST: &[u8] = br#"{
        "package":{"id":"test_package","version":"1.0.0"},
        "required_platform_policy_contract":{"id":"test_data_access","state":"satisfied"},
        "models":{
            "thing":{
                "schema":"application",
                "table":"thing",
                "owner":"test_package",
                "server_owned_fields":[],
                "audit_log":{"columns":[],"retention":"none"},
                "operations":{
                    "get":{
                        "permission":"thing.get",
                        "result":"one"
                    }
                }
            }
        },
        "connections":{},
        "components":{"test_package":{"connections":["postgres"]}}
    }"#;

    struct TestPackage {
        root: PathBuf,
        scratch: PathBuf,
    }

    impl TestPackage {
        fn new(name: &str) -> Self {
            let scratch = std::env::temp_dir().join(format!(
                "wamn-compiler-materialize-{}-{name}",
                std::process::id()
            ));
            if scratch.exists() {
                fs::remove_dir_all(&scratch).expect("remove stale test package");
            }
            let root = scratch.join("test_package");
            fs::create_dir_all(&root).expect("create test package");
            fs::write(root.join("wamn.json"), MANIFEST).expect("write test manifest");
            fs::write(
                root.join("Cargo.toml"),
                "[workspace]\nmembers = [\"generated/*\"]\n",
            )
            .expect("write app workspace");
            Self { root, scratch }
        }

        fn generated_snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
            let generated = crate::output_root(&self.root);
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
            fs::remove_dir_all(&self.scratch).expect("remove test package");
        }
    }

    fn consumer(package: &TestPackage) -> PathBuf {
        fs::write(
            package.scratch.join("pnpm-workspace.yaml"),
            "packages:\n  - web/consumer\n",
        )
        .unwrap();
        let directory = package.scratch.join("web/consumer");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("package.json"),
            br#"{
            "name": "@test/consumer", "wamn": {"clients": ["test_package"]}
        }"#,
        )
        .unwrap();
        directory
    }

    #[test]
    fn consumer_gets_plain_sources_and_removes_stale_files() {
        let package = TestPackage::new("consumer-source");
        let consumer = consumer(&package);
        let source = package.scratch.join("client-ts");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("index.ts"), "export const value = 1;\n").unwrap();
        fs::write(source.join("package.json"), "{}").unwrap();
        let output = consumer.join("src/generated/test_package");
        fs::create_dir_all(&output).unwrap();
        fs::write(output.join("obsolete.ts"), "old").unwrap();
        fs::write(output.join("package.json"), "{}").unwrap();
        materialize_client_consumers(&package.root, "test_package", &source).unwrap();
        assert_eq!(
            fs::read(output.join("index.ts")).unwrap(),
            fs::read(source.join("index.ts")).unwrap()
        );
        assert!(!output.join("package.json").exists());
        assert!(!output.join("obsolete.ts").exists());
        let modified = fs::metadata(output.join("index.ts"))
            .unwrap()
            .modified()
            .unwrap();
        materialize_client_consumers(&package.root, "test_package", &source).unwrap();
        assert_eq!(
            fs::metadata(output.join("index.ts"))
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
    }

    #[test]
    fn unavailable_client_names_the_consumer_and_application() {
        let package = TestPackage::new("consumer-missing");
        consumer(&package);
        let error = materialize_client_consumers(
            &package.root,
            "test_package",
            &package.scratch.join("missing"),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("@test/consumer"), "{message}");
        assert!(message.contains("test_package"), "{message}");
    }

    fn catalog() -> CatalogIr {
        CatalogIr::new(vec![Table::new(
            "application",
            "thing",
            vec![Column::new("id", SqlValueType::Uuid, false, None, None)],
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

        materialize_after_introspection(&wrapper.root, async {
            introspections.set(introspections.get() + 1);
            Ok(catalog.clone())
        })
        .await
        .expect("materialize through wrapper path");
        assert_eq!(introspections.get(), 1);

        materialize_package_from_catalog(&catalog, &split.root)
            .expect("materialize split generation path");
        assert_eq!(
            introspections.get(),
            1,
            "generation unexpectedly repeated database introspection"
        );
        assert_eq!(wrapper.generated_snapshot(), split.generated_snapshot());
    }

    #[test]
    fn stale_and_clean_materialization_produce_identical_outputs() {
        let stale = TestPackage::new("stale-output");
        let clean = TestPackage::new("clean-output");
        let output = crate::output_root(&stale.root);
        fs::create_dir_all(output.join("client-ts/components")).expect("create stale output");
        fs::write(output.join("client-ts/components/obsolete.tsx"), b"stale")
            .expect("write an obsolete component");
        fs::write(output.join("build.json"), b"stale").expect("write an obsolete build record");
        materialize_package_from_catalog(&catalog(), &stale.root).expect("replace stale output");
        materialize_package_from_catalog(&catalog(), &clean.root).expect("generate clean output");
        assert_eq!(stale.generated_snapshot(), clean.generated_snapshot());
    }

    #[test]
    fn renaming_a_model_removes_its_previous_wit_package_directory() {
        let package = TestPackage::new("removed-model-wit");
        let output = crate::output_root(&package.root);
        assert!(!output.exists());
        materialize_package_from_catalog(&catalog(), &package.root).expect("generate model WIT");
        let dependency = output.join("wit/deps/test-package-thing");
        assert!(dependency.join("package.wit").is_file());

        let mut manifest: serde_json::Value = serde_json::from_slice(MANIFEST).unwrap();
        let models = manifest["models"].as_object_mut().unwrap();
        let mut model = models.remove("thing").unwrap();
        model["operations"]["get"]["permission"] = serde_json::json!("replacement.get");
        models.insert("replacement".to_owned(), model);
        fs::write(
            package.root.join("wamn.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .expect("rename the model");
        materialize_package_from_catalog(&catalog(), &package.root)
            .expect("regenerate the renamed model");
        assert!(
            !dependency.exists(),
            "an empty dependency directory makes wit-bindgen reject the package"
        );
        assert!(
            output
                .join("wit/deps/test-package-replacement/package.wit")
                .is_file()
        );
    }

    #[test]
    fn materialization_removes_empty_dependencies_and_preserves_sqlx_metadata() {
        let package = TestPackage::new("empty-wit-dependency");
        let output = crate::output_root(&package.root);
        let empty = output.join("wit/deps/obsolete");
        fs::create_dir_all(&empty).expect("create an already empty WIT dependency");
        fs::create_dir_all(output.join(".sqlx")).expect("create the SQLx directory");
        let metadata = output.join(".sqlx/query.json");
        fs::write(&metadata, b"metadata").expect("write SQLx metadata");

        materialize_package_from_catalog(&catalog(), &package.root)
            .expect("regenerate the package");
        assert!(!empty.exists(), "an already empty dependency survives");
        assert!(
            output
                .join("wit/deps/test-package-thing/package.wit")
                .is_file()
        );
        assert_eq!(fs::read(metadata).unwrap(), b"metadata");
    }

    #[test]
    fn rewriting_identical_bytes_keeps_the_modification_time_and_a_changed_byte_rewrites() {
        let package = TestPackage::new("unchanged-mtime");
        let catalog = catalog();
        materialize_package_from_catalog(&catalog, &package.root).expect("materialize the package");
        let generated = package.generated_snapshot();
        let (relative, original) = generated.iter().next().expect("generated artifact exists");
        let path = crate::output_root(&package.root).join(relative);
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_hours(24);
        let set_old_mtime = || {
            fs::File::options()
                .write(true)
                .open(&path)
                .and_then(|file| file.set_modified(old))
                .expect("set an old modification time");
        };
        let mtime = || {
            fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .expect("read the modification time")
        };

        set_old_mtime();
        materialize_package_from_catalog(&catalog, &package.root)
            .expect("materialize the unchanged package");
        assert_eq!(mtime(), old, "identical bytes were rewritten");

        let mut changed = original.clone();
        *changed.last_mut().expect("generated artifact is not empty") ^= 1;
        fs::write(&path, &changed).expect("change one generated byte");
        set_old_mtime();
        materialize_package_from_catalog(&catalog, &package.root)
            .expect("materialize over the changed byte");
        assert_ne!(mtime(), old, "a changed byte was not rewritten");
        assert_eq!(&fs::read(&path).expect("read the rewritten file"), original);
    }
}

pub(crate) fn application_schemas(
    manifest: &PackageManifest,
) -> Result<Vec<String>, crate::ManifestError> {
    let schemas = manifest
        .models
        .values()
        .map(|model| model.schema.clone())
        .chain(manifest.custom_operations.values().flat_map(|operation| {
            operation
                .relations
                .iter()
                .map(|relation| relation.schema.clone())
        }))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    // A composition with only stateless wrappers owns no SQL schema. Its bases
    // own the SQL and contribute their grants when the release is applied.
    if schemas.is_empty() && (manifest.declares_no_sql() || manifest.uses_only_base_sql()) {
        return Ok(schemas);
    }
    if schemas.is_empty()
        || !schemas
            .iter()
            .all(|schema| crate::manifest::validate_identifier(schema, "schema").is_ok())
    {
        return Err(crate::ManifestError::new(
            crate::ManifestErrorType::InvalidManifest,
            "manifest must name at least one valid application schema",
        ));
    }
    Ok(schemas)
}

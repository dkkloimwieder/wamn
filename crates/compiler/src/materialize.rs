//! Filesystem and PostgreSQL-backed package materialization.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use sqlx::{AssertSqlSafe, Row as _, SqlSafeStr as _};
use wamn_schema_introspection::ir::CatalogIr;
use wamn_schema_introspection::postgres::read_catalog_excluding_relations;

use crate::StatementTransactionality;
use crate::authoring::package_manifest_path;
use crate::client_component::emit_ts_components;
use crate::client_ir::{ClientContractIr, published_routes};
use crate::client_plan::ClientPlan;
use crate::client_ts::{emit_ts_client, emit_ts_package_json};
use crate::generate::GeneratedFile;
use crate::manifest::{CONTROL_OWNED_RELATION_TABLES, PackageManifest};
use crate::{AuthoredSql, GeneratedPackage};

#[derive(Debug)]
pub(crate) struct SourceFile {
    pub(crate) path: String,
    pub(crate) bytes: Vec<u8>,
}

/// Introspect the package-owned schemas in one already-migrated PostgreSQL database.
pub async fn introspect_package(database_url: &str, package_root: &Path) -> Result<CatalogIr> {
    let (_, manifest) = load_manifest(package_root)?;
    let sources = load_authored_sql(&crate::output_root(package_root), &manifest)?;
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

    let client = wamn_postgres::pool(database_url, 4)
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
    let mut connection = wamn_postgres::connect(database_url)
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

/// The TypeScript bindings and components that this package declares.
///
/// Materialization owns these files with the rest of the generated output.
/// It removes files that it no longer produces.
/// Contracts and generated routes come from the package still in memory.
/// Authored routes come from `publication/attachments.json`.
/// Route templates are release facts, so bindings use them directly.
pub(crate) fn client_bindings(
    manifest: &PackageManifest,
    package: &GeneratedPackage,
) -> Result<Vec<GeneratedFile>> {
    let attachments = serde_json::from_slice(
        package
            .file(crate::publication::ATTACHMENTS)
            .context("missing final attachments")?
            .bytes(),
    )?;
    let declarations = serde_json::from_slice(
        package
            .file(crate::publication::COMPONENTS)
            .context("missing final components")?
            .bytes(),
    )?;
    let routes = published_routes(attachments, &declarations).context("read final routes")?;
    let ir = ClientContractIr::from_operations(&manifest.package.id, package.operations(), &routes)
        .context("project client fields")?;
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
/// client files go below `client-ts/`. A file there that the
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
    let mut parts = BTreeMap::<String, BTreeMap<String, serde_json::Value>>::new();
    for (path, bytes) in contracts {
        let Some((module, file)) = path.split_once('/') else {
            continue;
        };
        let Some((name, part)) = file
            .strip_suffix(".json")
            .and_then(|file| file.rsplit_once('.'))
        else {
            continue;
        };
        parts
            .entry(format!("{module}.{name}"))
            .or_default()
            .insert(part.to_owned(), serde_json::from_slice(&bytes)?);
    }
    let descriptions = parts
        .into_iter()
        .map(|(name, parts)| {
            serde_json::from_value(serde_json::to_value(parts)?)
                .map(|description| (name, description))
        })
        .collect::<Result<BTreeMap<String, crate::OperationDescription>, _>>()?;
    let ir = ClientContractIr::from_operations(
        wamn_catalog::host_route_package(),
        &descriptions,
        &routes,
    )?;
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
            .strip_prefix("generated")
            .with_context(|| format!("client binding escaped output root: {}", file.path()))?;
        expected.insert(relative.to_owned(), file.bytes());
    }
    write_build_output(output, &expected)
}

/// Copy an application's generated client into its declared workspace consumers.
///
/// Consumers receive source files only. Their own packages supply dependencies.
///
/// # Errors
///
/// Names the consumer and application when the requested client is unavailable.
pub fn materialize_client_consumers(package_root: &Path, app: &str, source: &Path) -> Result<()> {
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

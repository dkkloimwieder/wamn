//! Compile package declarations and SQL evidence into one output tree.

use wamn_package::manifest::CrudAction;
use wamn_package::manifest::ResultClass;
use wamn_package::manifest::canonical_operation_identity;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, Row as _, SqlSafeStr as _};
use wamn_component_policy::PackageManifest;
use wamn_digest::{Digest, bytes_sha256, canonical_json_bytes};
use wamn_schema_introspection::ir::CatalogIr;
use wamn_schema_introspection::postgres::authored_object_name;

use crate::AuthoredSql;
use crate::authoring::{COMPILED_MANIFEST_NAME, compile_authored_manifest};
use crate::describe::{DescribedStatement, column_type, describe_statements};
use crate::describe_cache::{self, Evidence};
use crate::materialize::{expected_files, introspect_with_manifest, write_files};
use crate::publication::{ATTACHMENTS, COMPONENTS};

const RECORD_HISTORY_SQL: &str = include_str!("../../../deploy/sql/record-history.sql");
const RECORD_HISTORY_APP_GRANTS_SQL: &str =
    include_str!("../../../deploy/sql/record-history-app-grants.sql");

/// Identity of the code and SQL helpers compiled into this generator.
pub const GENERATOR_ID: &str = include_str!(concat!(env!("OUT_DIR"), "/generator-id"));

/// Source of the database facts used by one build.
#[derive(Clone, Copy)]
pub enum DescribeMode<'a> {
    Cached,
    Refresh(&'a str),
    Check(&'a str),
}

impl<'a> DescribeMode<'a> {
    /// Select the requested describe operation, requiring a URL for live operations.
    pub fn from_options(url: Option<&'a str>, refresh: bool, check: bool) -> Result<Self> {
        if !refresh && !check {
            return Ok(Self::Cached);
        }
        let url = url
            .filter(|url| !url.is_empty())
            .context("set --database-url or DATABASE_URL")?;
        if refresh {
            Ok(Self::Refresh(url))
        } else {
            Ok(Self::Check(url))
        }
    }
}

impl std::fmt::Debug for DescribeMode<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Cached => "Cached",
            Self::Refresh(_) => "Refresh",
            Self::Check(_) => "Check",
        })
    }
}

/// One package build.
#[derive(Debug)]
pub struct BuildRequest<'a> {
    pub package_root: PathBuf,
    pub output_root: PathBuf,
    pub describe: DescribeMode<'a>,
}

impl<'a> BuildRequest<'a> {
    /// Build the package at `package_root` into a directory below `output_root`.
    pub fn new(package_root: &Path, output_root: &Path, describe: DescribeMode<'a>) -> Self {
        Self {
            package_root: package_root.to_owned(),
            output_root: output_root.to_owned(),
            describe,
        }
    }

    /// Generate artifacts and refresh the committed evidence from PostgreSQL.
    pub fn refresh(package_root: &Path, output_root: &Path, database_url: &'a str) -> Self {
        Self::new(
            package_root,
            output_root,
            DescribeMode::Refresh(database_url),
        )
    }
}

/// The frozen authored document and its source files.
#[derive(Debug)]
pub struct PreparedSource {
    root: PathBuf,
    snapshot: tempfile::TempDir,
    paths: Vec<PathBuf>,
    document: Value,
    sources: ManifestSources,
    wit: crate::authored_wit::Sources,
}

impl PreparedSource {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn package(&self) -> &wamn_package::manifest::PackageIdentity {
        &self.sources.package
    }

    pub fn bases(
        &self,
    ) -> &BTreeMap<
        wamn_package::manifest::source_name::SourceName,
        wamn_package::manifest::BaseDependencyRequirement,
    > {
        &self.sources.base_dependencies
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.document["components"]
            .as_object()
            .into_iter()
            .flat_map(|components| components.keys().map(String::as_str))
    }

    pub fn document(&self) -> &Value {
        &self.document
    }

    /// Refuse changes to the files frozen for KCL and code generation.
    pub fn validate_snapshot(&self) -> Result<()> {
        let paths = source_paths(&self.root)?;
        ensure!(
            paths == self.paths,
            "package sources changed during build; retry"
        );
        for path in paths {
            ensure!(
                fs::read(self.root.join(&path))? == fs::read(self.snapshot.path().join(&path))?,
                "{} changed during build; retry",
                self.root.join(path).display()
            );
        }
        Ok(())
    }
}

/// Freeze the package inputs and compile KCL once before discovery or generation.
pub fn prepare_source(root: &Path) -> Result<PreparedSource> {
    let (snapshot, paths) = snapshot_source(root)?;
    let mut document = serde_json::from_slice(&compile_authored_manifest(snapshot.path())?)
        .context("parse the authored manifest")?;
    let wit = crate::authored_wit::Sources::read(snapshot.path(), &paths, &mut document)?;
    set_statement_paths(&mut document)?;
    let sources = ManifestSources::from_document(&document)?;
    Ok(PreparedSource {
        root: root.to_owned(),
        snapshot,
        paths,
        document,
        sources,
        wit,
    })
}

/// The default output root of the package at `package_root`:
/// `apps/target/wamn` for both applications and nested platform packages.
#[must_use]
pub fn default_output_root(package_root: &Path) -> PathBuf {
    crate::output::package_workspace(package_root).join("target/wamn")
}

/// The build output directory of `package` below `output_root`.
#[must_use]
pub fn package_output(output_root: &Path, package: &wamn_package::PackageName) -> PathBuf {
    output_root.join(package.as_str())
}

/// Generate package output without compiling its components.
pub async fn generate_package(request: &BuildRequest<'_>) -> Result<PackageManifest> {
    let source = prepare_source(&request.package_root)?;
    let bases = crate::inheritance::load_base_manifests(&request.output_root, source.bases())?;
    let (manifest, _) = generate_prepared(request, &source, &bases, None).await?;
    Ok(manifest)
}

/// Generate artifacts from the same authored document that selected the build inputs.
pub async fn generate_prepared(
    request: &BuildRequest<'_>,
    source: &PreparedSource,
    bases: &[PackageManifest],
    development: Option<(
        &wamn_package::package_version::PackageVersion,
        &BTreeMap<wamn_package::PackageName, Digest>,
    )>,
) -> Result<(PackageManifest, BTreeMap<PathBuf, Digest>)> {
    let root = source.snapshot.path();
    let provisional = &source.sources;
    let mut authored = source.document.clone();
    if let Some((version, pins)) = development {
        authored["package"]["version"] = version.as_str().into();
        if let Some(dependencies) = authored
            .get_mut("base_dependencies")
            .and_then(Value::as_object_mut)
        {
            for dependency in dependencies.values_mut() {
                let package: wamn_package::PackageName = dependency["package"]
                    .as_str()
                    .context("base package id")?
                    .parse()?;
                let built = bases
                    .iter()
                    .find(|base| base.package.id == package)
                    .with_context(|| format!("base {package} was not built"))?;
                dependency["version"] = built.package.version.as_str().into();
                dependency["digest"] = pins[&package].to_string().into();
            }
        }
    }
    crate::migrations::validate_committed(&source.root, root).await?;
    crate::migrations::validate_history(root).await?;
    let sql = provisional.load_sql(root)?;
    let output = package_output(&request.output_root, &provisional.package.id);
    let generation = Generation {
        root,
        output: &output,
        sources: &sql,
        bases,
    };
    let input = describe_cache::inputs(root, &source.document, bases, &request.output_root)?;
    let generated = match request.describe {
        DescribeMode::Cached => {
            let cached = describe_cache::read(&source.root)?;
            let evidence = describe_cache::decode(&cached, input)?;
            let manifest = typed_manifest(authored, &evidence.statements)?;
            generate_from_evidence(&generation, manifest, evidence, None).await?
        }
        DescribeMode::Refresh(url) | DescribeMode::Check(url) => {
            let mut built = generate_live(
                url,
                &generation,
                provisional,
                &authored,
                input,
                &request.output_root,
            )
            .await?;
            if built.appended_history {
                ensure!(
                    matches!(request.describe, DescribeMode::Refresh(_)),
                    "generated history changed; run wamn build --refresh-describe"
                );
                // Verify the final migration sequence and record the evidence it produces.
                let input =
                    describe_cache::inputs(root, &source.document, bases, &request.output_root)?;
                built = generate_live(
                    url,
                    &generation,
                    provisional,
                    &authored,
                    input,
                    &request.output_root,
                )
                .await?;
                ensure!(
                    !built.appended_history,
                    "generated history did not stabilize"
                );
            }
            let bytes = describe_cache::encode(&built.evidence)?;
            if matches!(request.describe, DescribeMode::Check(_)) {
                let cached = describe_cache::read(&source.root)?;
                ensure!(
                    bytes == cached,
                    "describe cache differs from PostgreSQL; run wamn build --refresh-describe"
                );
                return Ok((built.manifest, BTreeMap::new()));
            }
            source.validate_snapshot()?;
            persist_history(&source.root, &built.files)?;
            describe_cache::write(&source.root, &bytes)?;
            built
        }
    };
    ensure!(
        !generated.appended_history,
        "stale describe cache for generated history; run wamn build --refresh-describe"
    );
    let Generated {
        manifest,
        mut files,
        ..
    } = generated;
    source
        .wit
        .stage(root, manifest.package.version.as_str(), &mut files)?;
    for path in &source.paths {
        if path.starts_with("component/wit") {
            continue;
        }
        if let std::collections::btree_map::Entry::Vacant(entry) = files.entry(path.clone()) {
            entry.insert(fs::read(root.join(path))?);
        }
    }
    let canonical = canonical_json_bytes(&serde_json::to_value(&manifest)?);
    crate::fingerprint::invalidate(&output)?;
    let outputs = write_outputs(&output, &canonical, &files)?;
    Ok((manifest, outputs))
}

fn persist_history(root: &Path, files: &BTreeMap<PathBuf, Vec<u8>>) -> Result<()> {
    for (relative, bytes) in files.iter().filter(|(path, _)| path.starts_with("history")) {
        let path = root.join(relative);
        match fs::read(&path) {
            Ok(existing) => {
                ensure!(
                    existing == *bytes,
                    "generated migration changed: {}",
                    path.display()
                );
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let parent = path.parent().context("history migration parent")?;
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        std::io::Write::write_all(&mut temporary, bytes)?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    fs::read(&path)? == *bytes,
                    "generated migration changed: {}",
                    path.display()
                );
            }
            Err(error) => return Err(error.into()),
        }
        fs::File::open(parent)?.sync_all()?;
        fs::File::open(root)?.sync_all()?;
    }
    Ok(())
}

/// Freeze the non-Rust inputs before compiling or applying migrations.
fn snapshot_source(root: &Path) -> Result<(tempfile::TempDir, Vec<PathBuf>)> {
    let paths = source_paths(root)?;
    let snapshot = tempfile::tempdir()?;
    for path in &paths {
        let destination = snapshot.path().join(path);
        fs::create_dir_all(
            destination
                .parent()
                .context("source input has no directory")?,
        )?;
        fs::copy(root.join(path), destination)?;
    }
    Ok((snapshot, paths))
}

fn source_paths(root: &Path) -> Result<Vec<PathBuf>> {
    ensure!(
        !root.join("wamn.json").exists(),
        "{}: author the package in wamn.k",
        root.display()
    );
    let mut directories = vec![PathBuf::new()];
    let mut paths = Vec::new();
    while let Some(relative) = directories.pop() {
        for entry in fs::read_dir(root.join(&relative))? {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            let type_ = entry.file_type()?;
            if path.starts_with("component") {
                if type_.is_dir()
                    && (Path::new(crate::authored_wit::DEPS).starts_with(&path)
                        || path.starts_with(crate::authored_wit::DEPS))
                {
                    directories.push(path);
                } else if (path == Path::new("component/wit/authored.wit")
                    || path.starts_with(crate::authored_wit::DEPS))
                    && path.extension().is_some_and(|extension| extension == "wit")
                {
                    ensure!(
                        type_.is_file(),
                        "authored WIT must be a regular file: {}",
                        entry.path().display()
                    );
                    paths.push(path);
                }
                continue;
            }
            let authored_tree = path.starts_with("migrations")
                || path.starts_with("history")
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
                paths.push(path);
            }
        }
    }
    paths.sort_unstable();
    Ok(paths)
}

struct Generated {
    manifest: PackageManifest,
    files: BTreeMap<PathBuf, Vec<u8>>,
    evidence: Evidence,
    appended_history: bool,
}

struct Generation<'a> {
    root: &'a Path,
    output: &'a Path,
    sources: &'a [crate::materialize::SourceFile],
    bases: &'a [PackageManifest],
}

fn typed_manifest(
    mut authored: Value,
    descriptions: &BTreeMap<String, DescribedStatement>,
) -> Result<PackageManifest> {
    apply_statement_types(&mut authored, descriptions)?;
    PackageManifest::parse_authored(&serde_json::to_vec(&authored)?)
        .context("parse the typed manifest")
}

async fn generate_live(
    server: &str,
    generation: &Generation<'_>,
    provisional: &ManifestSources,
    authored: &Value,
    input: Digest,
    output_root: &Path,
) -> Result<Generated> {
    let Generation {
        root,
        sources,
        bases,
        ..
    } = *generation;
    let database = VerificationDatabase::create(server).await?;
    let built = async {
        let url = database.url.as_str();
        let mut connection = wamn_postgres::connect(url).await?;
        let postgres: i32 =
            sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
                .fetch_one(&mut connection)
                .await?;
        drop(connection);
        let postgres = u32::try_from(postgres)?;
        describe_cache::postgres_identity(postgres)?;
        migrate(url, root, provisional, bases, output_root).await?;
        let described = describe_statements(
            url,
            sources.iter().map(|source| (&source.path, &source.bytes)),
        )
        .await?;
        let sql_digests = sources
            .iter()
            .map(|source| (source.path.clone(), bytes_sha256(&source.bytes)))
            .collect();
        let manifest = typed_manifest(authored.clone(), &described)?;
        let sql = sources
            .iter()
            .map(|source| AuthoredSql::new(&source.path, &source.bytes))
            .collect::<Vec<_>>();
        let catalog = introspect_with_manifest(url, &manifest, &sql).await?;
        let columns = ordered_columns(url, &manifest).await?;
        let evidence = Evidence {
            inputs: input,
            postgres,
            catalog,
            statements: described,
            sql: sql_digests,
            columns,
        };
        generate_from_evidence(generation, manifest, evidence, Some(url)).await
    }
    .await;
    let dropped = database.drop().await;
    let generated = built?;
    dropped?;
    Ok(generated)
}

async fn generate_from_evidence(
    generation: &Generation<'_>,
    mut manifest: PackageManifest,
    mut evidence: Evidence,
    live: Option<&str>,
) -> Result<Generated> {
    let Generation {
        root,
        output,
        sources,
        bases,
    } = *generation;
    for source in sources {
        ensure!(
            evidence.sql.get(&source.path) == Some(&bytes_sha256(&source.bytes)),
            "missing or stale describe entry for {}; run wamn build --refresh-describe",
            source.path,
        );
    }
    let authored_sql = sources
        .iter()
        .map(|source| AuthoredSql::new(&source.path, &source.bytes))
        .collect::<Vec<_>>();
    let catalog = &evidence.catalog;
    crate::generate::derive_model(catalog, &mut manifest, &authored_sql, bases)
        .context("derive the statement relations")?;
    let canonical = canonical_json_bytes(&serde_json::to_value(&manifest)?);
    for warning in missing_paged_query_indexes(catalog, &manifest, &evidence.columns)? {
        eprintln!("warning: {warning}");
    }
    for (name, operation) in &manifest.custom_operations {
        if let Some(source) = &operation.feed_source {
            for warning in crate::feed::missing_indexes(catalog, name, source) {
                eprintln!("warning: {warning}");
            }
        }
    }
    let mut verdicts = crate::StatementTransactionality::from_paths(BTreeMap::new());
    let input = crate::GenerationInput::new(catalog, &canonical, &authored_sql, &verdicts);
    let corpus = crate::generate::sql_corpus(&input, &manifest)?;
    if let Some(url) = live {
        let remaining = corpus
            .statements
            .iter()
            .filter(|(path, _)| !evidence.statements.contains_key(*path))
            .collect::<Vec<_>>();
        let described = describe_statements(url, remaining).await?;
        for (path, description) in described {
            let sql = bytes_sha256(&corpus.statements[&path]);
            evidence.sql.insert(path.clone(), sql);
            evidence.statements.insert(path, description);
        }
    }
    ensure!(
        corpus.statements.keys().eq(evidence.statements.keys())
            && corpus.statements.keys().eq(evidence.sql.keys()),
        "describe cache has missing or extra statements; run wamn build --refresh-describe"
    );
    for (path, sql) in &corpus.statements {
        ensure!(
            evidence.sql[path] == bytes_sha256(sql),
            "stale describe entry for {path}; run wamn build --refresh-describe"
        );
    }
    ensure!(
        evidence.columns.keys().eq(manifest.models.keys()),
        "describe cache has missing or extra history column facts; run wamn build --refresh-describe"
    );
    verdicts.described = std::mem::take(&mut evidence.statements);
    let input = crate::GenerationInput::new(catalog, &canonical, &authored_sql, &verdicts)
        .with_output_paths(output, output);
    let mut package = crate::generate::generate_described(&input, &manifest, &corpus)?;
    evidence.statements = verdicts.described;
    manifest.compiled_operations = package.take_operations();
    let mut generated = expected_files(&package)?;
    let routes = generated
        .remove(Path::new(ATTACHMENTS).strip_prefix("generated")?)
        .context("generated routes")?;
    let declarations = generated
        .remove(Path::new(COMPONENTS).strip_prefix("generated")?)
        .context("generated components")?;
    manifest.deploy = Some(wamn_package::manifest::deploy::CompiledDeploy {
        routes: serde_json::from_slice(routes)?,
        declarations: serde_json::from_slice(declarations)?,
        components: BTreeMap::new(),
        base_packages: BTreeMap::new(),
    });
    let mut files = generated
        .into_iter()
        .map(|(path, bytes)| (path, bytes.to_vec()))
        .collect::<BTreeMap<_, _>>();
    let history = crate::history::render(root, &manifest, &evidence.columns).await?;
    let migrations = crate::history::migrations(root, &history).await?;
    let mut appended_history = false;
    for (relative, bytes) in &migrations {
        let path = root.join(relative);
        if !path.exists() {
            appended_history = true;
            if live.is_some() {
                fs::create_dir_all(path.parent().context("history directory")?)?;
                fs::write(path, bytes)?;
            }
        }
    }
    files.extend(migrations);
    Ok(Generated {
        manifest,
        files,
        evidence,
        appended_history,
    })
}

async fn register_verification_principal(connection: &mut sqlx::PgConnection) -> Result<()> {
    let component = wamn_identity_protocol::PlatformComponent::ApplyPackage;
    sqlx::query("INSERT INTO platform.login_principal(login,principal_id) VALUES (current_user,$1) ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id")
        .bind(component.principal_id())
        .execute(connection)
        .await?;
    Ok(())
}

async fn ordered_columns(
    database_url: &str,
    manifest: &PackageManifest,
) -> Result<
    BTreeMap<wamn_package::manifest::source_name::SourceName, Vec<crate::history::RelationColumn>>,
> {
    use crate::history::{RelationColumn, SELECT_RELATION_COLUMNS_SQL};
    let client = wamn_postgres::pool(database_url, 4).await?;
    let result = async {
        let mut columns = BTreeMap::new();
        for (id, model) in &manifest.models {
            let rows = sqlx::query(SELECT_RELATION_COLUMNS_SQL)
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
        Ok(columns)
    }
    .await;
    client.close().await;
    result
}

/// One warning for each allowed sort of a paged query without its
/// (field, tie-breaker) index. The catalog includes indexes from every applied
/// package migration.
fn missing_paged_query_indexes(
    catalog: &CatalogIr,
    manifest: &PackageManifest,
    columns: &BTreeMap<
        wamn_package::manifest::source_name::SourceName,
        Vec<crate::history::RelationColumn>,
    >,
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
        let operation = canonical_operation_identity(
            &manifest.package,
            &wamn_package::local_operation::LocalOperation::try_from(format!(
                "{model_name}.query"
            ))?,
        )?;
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
            let columns = columns
                .get(model_name)
                .context("missing ordered columns")?
                .iter()
                .filter(|column| column.name == field || column.name == tie_breaker)
                .map(|column| column.name.clone())
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

fn set_statement_paths(document: &mut Value) -> Result<()> {
    let Some(operations) = document
        .get_mut("custom_operations")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    for (operation_name, operation) in operations {
        let local_name: wamn_package::local_operation::LocalOperation = operation_name.parse()?;
        let (group, local) = local_name
            .as_str()
            .split_once('.')
            .context("custom operation group and name")?;
        let directory = if operation["type"] == "projection" {
            "query"
        } else {
            "command"
        };
        if let Some(statements) = operation
            .get_mut("statements")
            .and_then(Value::as_object_mut)
        {
            for (name, statement) in statements {
                let name: wamn_package::manifest::source_name::SourceName = name.parse()?;
                statement["path"] = format!("{directory}/{group}/{local}/{name}.sql").into();
            }
        }
    }
    Ok(())
}

pub(crate) fn apply_statement_types(
    document: &mut Value,
    descriptions: &BTreeMap<String, DescribedStatement>,
) -> Result<()> {
    let Some(operations) = document
        .get_mut("custom_operations")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    for (operation_name, operation) in operations {
        let operation = operation.as_object_mut().context("custom operation")?;
        let Some(statements) = operation.remove("statements") else {
            continue;
        };
        let request = operation["input"]["fields"]
            .as_array()
            .context("WIT input fields")?;
        let Value::Object(mut statements) = statements else {
            bail!("custom statements must be an object");
        };
        for (name, statement) in &mut statements {
            let path = statement["path"].as_str().context("statement path")?;
            let at = format!("{operation_name}.{name} ({path})");
            let described = descriptions
                .get(path)
                .with_context(|| format!("{at}: no SQLx description"))?;
            let binds = statement["binds"]
                .as_array()
                .context("statement bind names")?;
            ensure!(
                binds.len() == described.parameters.len(),
                "{at}: KCL has {} binds and SQLx describes {}",
                binds.len(),
                described.parameters.len()
            );
            let mut parameters = Vec::with_capacity(binds.len());
            for (bind, type_name) in binds.iter().zip(&described.parameters) {
                let bind = bind.as_str().context("bind name")?;
                let ty = column_type(type_name).with_context(|| {
                    format!("{at}: unsupported bind type {type_name} for {bind}")
                })?;
                let mut fields = request.iter().filter(|field| {
                    field["path"]
                        .as_str()
                        .is_some_and(|path| path.rsplit('.').next() == Some(bind))
                });
                let nullable = if let Some(field) = fields.next() {
                    ensure!(
                        fields.next().is_none(),
                        "{at}: bind {bind} matches more than one WIT input field"
                    );
                    ensure!(
                        field["type"] == ty.as_str(),
                        "{at}: bind {bind} has WIT type {} and SQLx type {}",
                        field["type"],
                        ty.as_str()
                    );
                    field["nullable"] == true
                } else {
                    false
                };
                parameters.push(json!({"name": bind, "type": ty, "nullable": nullable}));
            }
            let mut row = Vec::with_capacity(described.columns.len());
            for column in &described.columns {
                let (column_name, nullable) = column
                    .contract()
                    .with_context(|| format!("{at}: column {}", column.name))?;
                let ty = crate::describe::result_type(&column.type_name).with_context(|| {
                    format!(
                        "{at}: unsupported result type {} for {column_name}",
                        column.type_name
                    )
                })?;
                let nullable = nullable.with_context(|| format!("{at}: SQLx cannot infer nullability. Add a ! or ? to the SQL alias for {column_name}"))?;
                row.push(json!({"name": column_name, "type": ty, "nullable": nullable}));
            }
            let statement = statement.as_object_mut().context("statement object")?;
            statement.remove("binds");
            statement.insert("parameters".into(), parameters.into());
            statement.insert("row".into(), row.into());
        }
        operation.insert("statements".into(), statements.into());
    }
    Ok(())
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
        schemas.extend(base.models.values().map(|model| model.schema.to_string()));
        directories.push(package_output(output_root, &base.package.id));
    }
    schemas.sort();
    schemas.dedup();
    directories.push(package_root.to_owned());

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
        // Platform helpers precede generated SQL in every verification database.
        create_history_tables(&client, manifest).await?;
        for (index, directory) in directories.into_iter().enumerate() {
            let mut migrator = crate::migrations::migrator(&directory).await?;
            migrator.dangerous_set_table_name(format!("_sqlx_verification_{index}"));
            let mut connection = wamn_postgres::connect(database_url).await?;
            register_verification_principal(&mut connection).await?;
            migrator.run_direct(None, &mut connection, false).await.context("migrate the verification database")?;
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
    sqlx::raw_sql(
        AssertSqlSafe(format!(
            "DO $application_role$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = {literal}) THEN \
                 CREATE ROLE {role} NOLOGIN; \
               END IF; \
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; \
             END $application_role$",
            literal = postgres_protocol::escape::escape_literal(wamn_role::APP_ROLE),
            role = postgres_protocol::escape::escape_identifier(wamn_role::APP_ROLE),
        ))
        .into_sql_str(),
    )
    .execute(client)
    .await
    .context("create the application role")?;
    sqlx::raw_sql(AssertSqlSafe((RECORD_HISTORY_SQL).to_owned()).into_sql_str())
        .execute(client)
        .await
        .context("apply record-history.sql")?;
    sqlx::raw_sql(AssertSqlSafe((RECORD_HISTORY_APP_GRANTS_SQL).to_owned()).into_sql_str())
        .execute(client)
        .await
        .context("apply record-history-app-grants.sql")?;
    for model in logged {
        let exists: bool = sqlx::query_scalar(
            "SELECT to_regclass(format('%I.%I', $1::text, $2::text)) IS NOT NULL",
        )
        .bind(&model.schema)
        .bind(&model.table)
        .fetch_one(client)
        .await?;
        if !exists {
            continue;
        }
        sqlx::raw_sql(AssertSqlSafe(crate::history::history_table_sql(model)).into_sql_str())
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
/// and the captured authored inputs.
fn write_outputs(
    package_output: &Path,
    canonical: &[u8],
    files: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<BTreeMap<PathBuf, Digest>> {
    let mut outputs = files
        .iter()
        .map(|(path, bytes)| (path.clone(), bytes.as_slice()))
        .collect::<BTreeMap<_, _>>();
    outputs.insert(PathBuf::from(COMPILED_MANIFEST_NAME), canonical);

    write_files(package_output, &outputs)?;
    Ok(outputs
        .into_iter()
        .map(|(path, bytes)| (path, bytes_sha256(bytes)))
        .collect())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    #[ignore = "requires: docker"]
    async fn verification_migration_accepts_a_platform_file_reference() {
        let server = wamn_test_infrastructure::containers::start_postgres()
            .await
            .unwrap();
        let database = server.create_database("tests").await.unwrap();
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
        let result = migrate(
            database.as_str(),
            &root,
            &ManifestSources::from_document(&serde_json::to_value(&manifest).unwrap()).unwrap(),
            &[],
            &default_output_root(&root),
        )
        .await;
        fs::remove_dir_all(root).unwrap();
        result.expect("the verification database permits a platform.file foreign key");
        let client = wamn_postgres::pool(database.as_str(), 4).await.unwrap();
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
}

/// Source metadata used before the database describes statement values.
#[derive(Debug, serde::Deserialize)]
struct ManifestSources {
    package: wamn_package::manifest::PackageIdentity,
    #[serde(default)]
    base_dependencies: BTreeMap<
        wamn_package::manifest::source_name::SourceName,
        wamn_package::manifest::BaseDependencyRequirement,
    >,
    models: BTreeMap<
        wamn_package::manifest::source_name::SourceName,
        wamn_package::manifest::ModelDeclaration,
    >,
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
            .map(|model| model.schema.to_string())
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

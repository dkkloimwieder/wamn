//! Apply one package-owned migration stream exactly once per immutable file.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use clap::Args;
use tokio_postgres::{NoTls, Transaction, error::SqlState};
use wamn_schema_control::{
    AppliedPackage, MigrationSource, PackageDirectory, PackageMigrationError, RecordedMigration,
    SqlStatement, plan_package_migrations,
};

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";
const LOCK_PACKAGE_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.package.lineage:' || $1 || ':' || $2, 0))";
const SELECT_PACKAGE_SQL: &str = "\
SELECT manifest_sha256, predecessor_version FROM catalog.packages \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
 FOR UPDATE";
const SELECT_MIGRATIONS_SQL: &str = "\
SELECT ordinal, relative_path, sha256 FROM catalog.package_migrations \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
 ORDER BY ordinal";
const SELECT_CURRENT_PACKAGE_VERSION_SQL: &str = "\
SELECT package.package_version FROM catalog.packages AS package \
 WHERE package.tenant_id = $1 AND package.package_id = $2 \
   AND NOT EXISTS (\
       SELECT 1 FROM catalog.packages AS successor \
        WHERE successor.tenant_id = package.tenant_id \
          AND successor.package_id = package.package_id \
          AND successor.predecessor_version = package.package_version\
   )";

/// Stable apply-package refusal prefix.
pub const APPLY_PACKAGE_REFUSAL: &str = "apply-package-refused";
/// Server refusal translated when release membership seals a package version.
pub const PACKAGE_VERSION_SEALED_REFUSAL: &str = "package-version-sealed";
/// A new coordinate must extend the one installed leaf for its package family.
pub const PREDECESSOR_NOT_CURRENT_REFUSAL: &str = "predecessor-not-current";

/// Remedy-distinct apply-package refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyPackageErrorKind {
    PackageVersionSealed,
    PredecessorNotCurrent,
    PredecessorPrefixMismatch,
}

impl ApplyPackageErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PackageVersionSealed => PACKAGE_VERSION_SEALED_REFUSAL,
            Self::PredecessorNotCurrent => PREDECESSOR_NOT_CURRENT_REFUSAL,
            Self::PredecessorPrefixMismatch => {
                wamn_schema_control::PackageMigrationErrorKind::PredecessorPrefixMismatch.as_str()
            }
        }
    }
}

/// Contextual failure at the package application boundary.
#[derive(Debug)]
pub struct ApplyPackageError {
    kind: ApplyPackageErrorKind,
    coordinate: String,
    predecessor_version: Option<String>,
    current_version: Option<String>,
    path: Option<String>,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ApplyPackageError {
    pub const fn kind(&self) -> ApplyPackageErrorKind {
        self.kind
    }

    pub fn coordinate(&self) -> &str {
        &self.coordinate
    }

    pub fn predecessor_version(&self) -> Option<&str> {
        self.predecessor_version.as_deref()
    }

    pub fn current_version(&self) -> Option<&str> {
        self.current_version.as_deref()
    }

    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }
}

impl fmt::Display for ApplyPackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{APPLY_PACKAGE_REFUSAL} ({}): coordinate={}",
            self.kind.as_str(),
            self.coordinate
        )?;
        if let Some(predecessor) = &self.predecessor_version {
            write!(formatter, "; predecessor-version={predecessor}")?;
        } else if self.kind == ApplyPackageErrorKind::PredecessorNotCurrent {
            formatter.write_str("; predecessor-version=<none>")?;
        }
        if let Some(current) = &self.current_version {
            write!(formatter, "; current-version={current}")?;
        }
        if let Some(path) = &self.path {
            write!(formatter, "; file={path}")?;
        }
        write!(formatter, "; {}", self.detail)
    }
}

impl std::error::Error for ApplyPackageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

/// Apply the immutable pending suffix from one package directory.
#[derive(Debug, Args)]
pub struct ApplyPackageArgs {
    /// Package root containing strict wamn.json and migrations/.
    #[arg(long)]
    pub package: PathBuf,

    /// Owner connection to the target project-environment database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub database_url: String,

    /// Tenant stored with the package and migration ledger rows.
    #[arg(long)]
    pub tenant: String,
}

#[derive(Debug)]
struct ApplyOutcome {
    migrations_applied: usize,
    changed: bool,
}

pub async fn run(args: ApplyPackageArgs) -> anyhow::Result<()> {
    ensure!(!args.tenant.is_empty(), "tenant must not be empty");
    let directory = read_package_directory(&args.package)?;
    let presented = plan_package_migrations(&directory, None)
        .context("validate package directory before database work")?;
    validate_migration_policy(&args.package, &directory, &presented)?;
    let coordinate = presented.coordinate.clone();
    let coordinate_text = format!(
        "{}@{}",
        coordinate.package_id(),
        coordinate.package_version()
    );

    let (mut client, connection) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect to project environment")?;
    let connection_task = tokio::spawn(connection);
    let result = apply(&mut client, &args.tenant, &coordinate_text, &directory).await;
    drop(client);
    if result.is_err() {
        connection_task.abort();
    } else {
        connection_task
            .await
            .context("join package database connection")?
            .context("drive package database connection")?;
    }
    let outcome = result?;
    println!(
        "applied {coordinate_text}: {} migration(s){}",
        outcome.migrations_applied,
        if !outcome.changed {
            " (already converged)"
        } else {
            ""
        }
    );
    Ok(())
}

fn validate_migration_policy(
    package_root: &Path,
    directory: &PackageDirectory,
    plan: &wamn_schema_control::PackageMigrationPlan,
) -> anyhow::Result<()> {
    let mut schemas = plan
        .models
        .iter()
        .map(|model| model.schema.as_str())
        .collect::<Vec<_>>();
    schemas.sort_unstable();
    schemas.dedup();
    ensure!(
        !schemas.is_empty(),
        "package-migration-schema-missing: manifest must map at least one model to an application schema"
    );
    for migration in &directory.migrations {
        let path = package_root.join(&migration.relative_path);
        wamn_schema_introspection::migration_policy::validate_migration_bytes_for_schemas(
            &path,
            &migration.bytes,
            &schemas,
        )
        .with_context(|| format!("validate {} before apply", migration.relative_path))?;
    }
    Ok(())
}

async fn apply(
    client: &mut tokio_postgres::Client,
    tenant: &str,
    coordinate_text: &str,
    directory: &PackageDirectory,
) -> anyhow::Result<ApplyOutcome> {
    let presented =
        plan_package_migrations(directory, None).context("validate package directory")?;
    let package_id = presented.coordinate.package_id().to_owned();
    let package_version = presented.coordinate.package_version().to_owned();
    let tx = client.transaction().await.context("begin package apply")?;
    tx.query_one(CLAIM_TENANT_SQL, &[&tenant])
        .await
        .context("claim package tenant")?;
    tx.query_one(LOCK_PACKAGE_SQL, &[&tenant, &package_id])
        .await
        .context("lock package family")?;

    let applied = load_applied_package(&tx, tenant, &package_id, &package_version).await?;
    let plan = if let Some(applied) = applied.as_ref() {
        plan_package_migrations(directory, Some(applied))
            .context("compare package bytes with immutable ledger")?
    } else {
        match current_package_version(&tx, tenant, &package_id).await? {
            None => presented,
            Some(current_version) => {
                if presented.predecessor_version.as_deref() != Some(current_version.as_str()) {
                    return Err(predecessor_not_current_error(
                        coordinate_text,
                        presented.predecessor_version.as_deref(),
                        &current_version,
                    )
                    .into());
                }
                let predecessor = load_applied_package(&tx, tenant, &package_id, &current_version)
                    .await?
                    .expect("the selected package-family leaf is an applied package");
                plan_package_migrations(directory, Some(&predecessor)).map_err(|source| {
                    predecessor_prefix_error(
                        coordinate_text,
                        &current_version,
                        Some(source),
                        presented
                            .pending
                            .first()
                            .map(|migration| migration.relative_path.as_str()),
                    )
                })?
            }
        }
    };
    let applied_count = plan.pending.len();
    let changed = !plan.is_noop();

    ensure_model_schemas(&tx, &plan).await?;
    for statement in &plan.statements {
        execute(&tx, statement, &coordinate_text).await?;
    }
    reconcile_entity_maps(&tx, &plan).await?;
    tx.commit().await.context("commit whole package suffix")?;
    Ok(ApplyOutcome {
        migrations_applied: applied_count,
        changed,
    })
}

async fn current_package_version(
    tx: &Transaction<'_>,
    tenant: &str,
    package_id: &str,
) -> anyhow::Result<Option<String>> {
    tx.query_opt(SELECT_CURRENT_PACKAGE_VERSION_SQL, &[&tenant, &package_id])
        .await
        .context("read current package-family leaf")
        .map(|row| row.map(|row| row.get(0)))
}

fn predecessor_not_current_error(
    coordinate: &str,
    declared_version: Option<&str>,
    current_version: &str,
) -> ApplyPackageError {
    ApplyPackageError {
        kind: ApplyPackageErrorKind::PredecessorNotCurrent,
        coordinate: coordinate.to_owned(),
        predecessor_version: declared_version.map(str::to_owned),
        current_version: Some(current_version.to_owned()),
        path: None,
        detail: "declare the current installed package version as predecessor_version".into(),
        source: None,
    }
}

fn predecessor_prefix_error(
    coordinate: &str,
    predecessor_version: &str,
    source: Option<PackageMigrationError>,
    fallback_path: Option<&str>,
) -> ApplyPackageError {
    let path = source
        .as_ref()
        .and_then(PackageMigrationError::path)
        .or(fallback_path)
        .map(str::to_owned);
    let detail = source.as_ref().map_or_else(
        || {
            "declared predecessor is not applied; apply that exact predecessor before upgrading"
                .to_owned()
        },
        |source| format!("declared predecessor does not match the cumulative prefix: {source}"),
    );
    ApplyPackageError {
        kind: ApplyPackageErrorKind::PredecessorPrefixMismatch,
        coordinate: coordinate.to_owned(),
        predecessor_version: Some(predecessor_version.to_owned()),
        current_version: None,
        path,
        detail,
        source: source.map(|source| Box::new(source) as Box<dyn std::error::Error + Send + Sync>),
    }
}

async fn ensure_model_schemas(
    tx: &Transaction<'_>,
    plan: &wamn_schema_control::PackageMigrationPlan,
) -> anyhow::Result<()> {
    let schemas = plan
        .models
        .iter()
        .map(|model| model.schema.as_str())
        .collect::<BTreeSet<_>>();
    for schema in schemas {
        let schema = wamn_schema_control::BareSchemaName::new(schema)
            .context("validate manifest model schema for creation")?;
        tx.batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {}", schema.quoted()))
            .await
            .with_context(|| format!("ensure package model schema {schema}"))?;
    }
    Ok(())
}

async fn reconcile_entity_maps(
    tx: &Transaction<'_>,
    plan: &wamn_schema_control::PackageMigrationPlan,
) -> anyhow::Result<()> {
    let schemas = plan
        .models
        .iter()
        .map(|model| model.schema.as_str())
        .collect::<BTreeSet<_>>();
    for schema in schemas {
        tx.batch_execute(&wamn_control_provision::sql::ensure_entity_map_sql(schema))
            .await
            .with_context(|| format!("ensure {schema}.wamn_entities"))?;
    }

    for model in &plan.models {
        let schema = wamn_schema_control::BareSchemaName::new(&model.schema)
            .context("validate manifest model schema for entity-map query")?;
        let mapping_sql = format!(
            "SELECT mapped.package_id, mapped.entity_id, mapped.table_name \
               FROM pg_catalog.pg_class AS relation \
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
               LEFT JOIN {}.wamn_entities AS mapped ON mapped.relation_oid = relation.oid \
              WHERE namespace.nspname = $1 AND relation.relname = $2 \
                AND relation.relkind = 'r'",
            schema.quoted()
        );
        let Some(mapping) = tx
            .query_opt(&mapping_sql, &[&model.schema, &model.table])
            .await
            .with_context(|| {
                format!(
                    "read entity map {}.{} ({})",
                    model.schema, model.table, model.model_id
                )
            })?
        else {
            bail!(
                "package-model-relation-missing: {} maps {}.{} but the migration stream did not create that table",
                model.model_id,
                model.schema,
                model.table
            );
        };
        let mapped_package = mapping.get::<_, Option<String>>(0);
        let mapped_entity = mapping.get::<_, Option<String>>(1);
        let mapped_table = mapping.get::<_, Option<String>>(2);
        if let (Some(mapped_package), Some(mapped_entity)) =
            (mapped_package.as_deref(), mapped_entity.as_deref())
        {
            if mapped_package != plan.coordinate.package_id()
                || mapped_entity != model.model_id.as_str()
            {
                bail!(
                    "package-entity-oid-rebind-refused: {}.{} is already mapped to {mapped_package}/{mapped_entity}; cannot rebind it to {}/{}",
                    model.schema,
                    model.table,
                    plan.coordinate.package_id(),
                    model.model_id
                );
            }
            if mapped_table.as_deref() == Some(model.table.as_str()) {
                continue;
            }
        }
        let mapped = tx
            .execute(
                &wamn_control_provision::sql::upsert_entity_map_sql(&model.schema),
                &[&plan.coordinate.package_id(), &model.model_id, &model.table],
            )
            .await
            .with_context(|| {
                format!(
                    "upsert entity map {}.{} ({})",
                    model.schema, model.table, model.model_id
                )
            })?;
        ensure!(mapped == 1, "package-entity-map-write-refused");
    }
    Ok(())
}

pub(crate) async fn load_applied_package(
    tx: &Transaction<'_>,
    tenant: &str,
    package_id: &str,
    package_version: &str,
) -> anyhow::Result<Option<AppliedPackage>> {
    let Some(package) = tx
        .query_opt(
            SELECT_PACKAGE_SQL,
            &[&tenant, &package_id, &package_version],
        )
        .await
        .context("read immutable package root")?
    else {
        return Ok(None);
    };
    let migrations = tx
        .query(
            SELECT_MIGRATIONS_SQL,
            &[&tenant, &package_id, &package_version],
        )
        .await
        .context("read immutable package migration prefix")?
        .into_iter()
        .map(|row| RecordedMigration {
            ordinal: u32::try_from(row.get::<_, i32>(0))
                .expect("package migration ordinals are positive integers"),
            relative_path: row.get(1),
            sha256: row.get(2),
        })
        .collect();
    Ok(Some(AppliedPackage {
        coordinate: wamn_catalog::PackageCoordinate::new(package_id, package_version)
            .expect("stored package coordinates passed database checks"),
        predecessor_version: package.get(1),
        manifest_sha256: package.get(0),
        migrations,
    }))
}

async fn execute(
    tx: &Transaction<'_>,
    statement: &SqlStatement,
    coordinate: &str,
) -> anyhow::Result<()> {
    let result = if statement.params.is_empty() {
        tx.batch_execute(&statement.sql).await
    } else {
        let params = crate::sql_params::as_postgres(&statement.params);
        tx.execute(&statement.sql, &params).await.map(|_| ())
    };
    match result {
        Ok(()) => Ok(()),
        Err(source) if is_package_version_sealed(&source) => Err(ApplyPackageError {
            kind: ApplyPackageErrorKind::PackageVersionSealed,
            coordinate: coordinate.to_owned(),
            predecessor_version: None,
            current_version: None,
            path: None,
            detail: "already belongs to an effective release; create and apply a new package version for additional migrations".into(),
            source: Some(Box::new(source)),
        }
        .into()),
        Err(source) => Err(source).with_context(|| format!("apply {}", statement.summary)),
    }
}

fn is_package_version_sealed(error: &tokio_postgres::Error) -> bool {
    error.as_db_error().is_some_and(|database| {
        database.code() == &SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE
            && database.message() == PACKAGE_VERSION_SEALED_REFUSAL
    })
}

pub(crate) fn read_package_directory(root: &Path) -> anyhow::Result<PackageDirectory> {
    let manifest_path = root.join("wamn.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let migrations_path = root.join("migrations");
    let entries = std::fs::read_dir(&migrations_path)
        .with_context(|| format!("read {}", migrations_path.display()))?;
    let mut migrations = Vec::new();
    for entry in entries {
        let entry = entry.context("read package migration directory entry")?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("inspect {}", entry.path().display()))?;
        ensure!(
            file_type.is_file(),
            "package migration entry is not a file: {}",
            entry.path().display()
        );
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("package migration file name is not UTF-8"))?;
        migrations.push(MigrationSource {
            relative_path: format!("migrations/{file_name}"),
            bytes: std::fs::read(entry.path())
                .with_context(|| format!("read {}", entry.path().display()))?,
        });
    }
    Ok(PackageDirectory {
        manifest_bytes,
        migrations,
    })
}

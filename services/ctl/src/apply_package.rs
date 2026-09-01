//! Apply one package-owned migration stream exactly once per immutable file.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use clap::Args;
use tokio_postgres::{NoTls, Transaction, error::SqlState};
use wamn_control_provision::operation_grants::{
    OPERATION_GRANT_LOCK_SQL, OPERATION_GRANT_TRANSACTION_PRELUDE_SQL,
    OperationGrantReconcileResult, operation_grant_floor_check_sql, reconcile_operation_grants_sql,
};
use wamn_schema_control::{
    AppliedPackage, MigrationSource, PackageDirectory, PackageMigrationError, RecordedMigration,
    SqlStatement, plan_package_migrations,
};
use wamn_schema_generator::{ModelDeclaration, PackageManifest};
use wamn_schema_introspection::migration_policy::{
    DefinitionAction, DefinitionKind, DefinitionMutation, MigrationPolicyError,
    MigrationPolicyErrorKind, inspect_migration_definition_mutations,
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
const SELECT_DEFINITION_OWNER_SQL: &str = "\
SELECT owner_package_id, client_field_extensible \
  FROM catalog.package_definition_owners \
 WHERE tenant_id = $1 AND schema_name = $2 AND relation_name = $3 \
   AND definition_kind = $4 AND definition_name = $5";
const INSERT_DEFINITION_OWNER_SQL: &str = "\
INSERT INTO catalog.package_definition_owners \
    (tenant_id, schema_name, relation_name, definition_kind, definition_name, \
     owner_package_id, client_field_extensible) \
VALUES ($1, $2, $3, $4, $5, $6, $7) \
ON CONFLICT (tenant_id, schema_name, relation_name, definition_kind, definition_name) \
DO NOTHING";
const SELECT_RELATION_PRESENT_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 FROM pg_catalog.pg_class AS relation \
    JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
    WHERE namespace.nspname = $1 AND relation.relname = $2 AND relation.relkind = 'r'\
)";
const SELECT_FIELD_PRESENT_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 FROM pg_catalog.pg_attribute AS field \
    JOIN pg_catalog.pg_class AS relation ON relation.oid = field.attrelid \
    JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
    WHERE namespace.nspname = $1 AND relation.relname = $2 \
      AND relation.relkind = 'r' AND field.attname = $3 \
      AND field.attnum > 0 AND NOT field.attisdropped\
)";
const SELECT_CONSTRAINT_PRESENT_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 FROM pg_catalog.pg_constraint AS definition \
    JOIN pg_catalog.pg_class AS relation ON relation.oid = definition.conrelid \
    JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
    WHERE namespace.nspname = $1 AND relation.relname = $2 \
      AND relation.relkind = 'r' AND definition.conname = $3 \
      AND definition.contype IN ('p', 'u', 'f', 'c')\
)";
const SELECT_RELATION_DEFINITIONS_SQL: &str = "\
SELECT definition_kind, definition_name FROM (\
    SELECT 'field'::text AS definition_kind, field.attname::text AS definition_name, \
           field.attnum::int AS ordering \
      FROM pg_catalog.pg_attribute AS field \
      JOIN pg_catalog.pg_class AS relation ON relation.oid = field.attrelid \
      JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
     WHERE namespace.nspname = $1 AND relation.relname = $2 \
       AND relation.relkind = 'r' AND field.attnum > 0 AND NOT field.attisdropped \
    UNION ALL \
    SELECT 'constraint'::text, definition.conname::text, 1000000 \
      FROM pg_catalog.pg_constraint AS definition \
      JOIN pg_catalog.pg_class AS relation ON relation.oid = definition.conrelid \
      JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
     WHERE namespace.nspname = $1 AND relation.relname = $2 \
       AND relation.relkind = 'r' AND definition.contype IN ('p', 'u', 'f', 'c')\
) AS definitions ORDER BY ordering, definition_kind, definition_name COLLATE \"C\"";

/// Stable apply-package refusal prefix.
pub const APPLY_PACKAGE_REFUSAL: &str = "apply-package-refused";
/// Server refusal translated when release membership seals a package version.
pub const PACKAGE_VERSION_SEALED_REFUSAL: &str = "package-version-sealed";
/// A new coordinate must extend the one installed leaf for its package family.
pub const PREDECESSOR_NOT_CURRENT_REFUSAL: &str = "predecessor-not-current";
/// An overlay attempted to mutate a definition owned by another package.
pub const BASE_DEFINITION_MUTATION_REFUSAL: &str = "base-definition-mutation-refused";
/// A shared relation did not publish additive client-field authority.
pub const RELATION_NOT_CLIENT_EXTENSIBLE_REFUSAL: &str = "relation-not-client-extensible";
/// A migration addition lacks its exact manifest ownership declaration.
pub const DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL: &str =
    "definition-owner-declaration-missing";
/// A live definition lacks or disagrees with its durable owner fact.
pub const DEFINITION_OWNER_CONFLICT_REFUSAL: &str = "definition-owner-conflict";
/// PostgreSQL did not expose the definition a migration reported creating.
pub const DEFINITION_NOT_FOUND_REFUSAL: &str = "definition-not-found";

/// Remedy-distinct apply-package refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyPackageErrorKind {
    PackageVersionSealed,
    PredecessorNotCurrent,
    PredecessorPrefixMismatch,
    BaseDefinitionMutation,
    RelationNotClientExtensible,
    DefinitionOwnerDeclarationMissing,
    DefinitionOwnerConflict,
    DefinitionNotFound,
}

impl ApplyPackageErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PackageVersionSealed => PACKAGE_VERSION_SEALED_REFUSAL,
            Self::PredecessorNotCurrent => PREDECESSOR_NOT_CURRENT_REFUSAL,
            Self::PredecessorPrefixMismatch => {
                wamn_schema_control::PackageMigrationErrorKind::PredecessorPrefixMismatch.as_str()
            }
            Self::BaseDefinitionMutation => BASE_DEFINITION_MUTATION_REFUSAL,
            Self::RelationNotClientExtensible => RELATION_NOT_CLIENT_EXTENSIBLE_REFUSAL,
            Self::DefinitionOwnerDeclarationMissing => DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL,
            Self::DefinitionOwnerConflict => DEFINITION_OWNER_CONFLICT_REFUSAL,
            Self::DefinitionNotFound => DEFINITION_NOT_FOUND_REFUSAL,
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
    schema: Option<String>,
    relation: Option<String>,
    definition_kind: Option<DefinitionKind>,
    definition: Option<String>,
    owner_package: Option<String>,
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

    pub fn schema(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    pub fn relation(&self) -> Option<&str> {
        self.relation.as_deref()
    }

    pub fn definition(&self) -> Option<&str> {
        self.definition.as_deref()
    }

    pub fn owner_package(&self) -> Option<&str> {
        self.owner_package.as_deref()
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
        if let Some(schema) = &self.schema {
            write!(formatter, "; schema={schema}")?;
        }
        if let Some(relation) = &self.relation {
            write!(formatter, "; relation={relation}")?;
        }
        if let Some(kind) = self.definition_kind {
            write!(formatter, "; definition-kind={}", kind.as_str())?;
        }
        if let Some(definition) = &self.definition {
            write!(formatter, "; definition={definition}")?;
        }
        if let Some(owner) = &self.owner_package {
            write!(formatter, "; owner-package={owner}")?;
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

#[derive(Debug)]
struct PlannedDefinitionMutation {
    relative_path: Box<str>,
    mutation: DefinitionMutation,
}

#[derive(Debug)]
struct DeferredMigrationPolicyError {
    relative_path: Box<str>,
    source: MigrationPolicyError,
}

#[derive(Debug)]
struct MigrationPolicyPlan {
    mutations: Vec<PlannedDefinitionMutation>,
    deferred: Option<DeferredMigrationPolicyError>,
}

#[derive(Debug)]
struct StoredDefinitionOwner {
    package_id: String,
    client_field_extensible: bool,
}

pub async fn run(args: ApplyPackageArgs) -> anyhow::Result<()> {
    ensure!(!args.tenant.is_empty(), "tenant must not be empty");
    let directory = read_package_directory(&args.package)?;
    let presented = plan_package_migrations(&directory, None)
        .context("validate package directory before database work")?;
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes)
        .context("parse strict package manifest for definition ownership")?;
    let migration_policy = validate_migration_policy(&args.package, &directory, &presented)?;
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
    let result = apply(
        &mut client,
        &args.tenant,
        &coordinate_text,
        &directory,
        &manifest,
        migration_policy,
    )
    .await;
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
) -> anyhow::Result<MigrationPolicyPlan> {
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
    let mut mutations = Vec::new();
    let mut deferred = None;
    for migration in &directory.migrations {
        let path = package_root.join(&migration.relative_path);
        let inspected =
            inspect_migration_definition_mutations(&path, &migration.bytes, &schemas)
                .with_context(|| format!("inspect {} before apply", migration.relative_path))?;
        let validation =
            wamn_schema_introspection::migration_policy::validate_migration_bytes_for_schemas(
                &path,
                &migration.bytes,
                &schemas,
            );
        match validation {
            Ok(()) => {}
            Err(source)
                if source.kind() == MigrationPolicyErrorKind::UnsupportedStatement
                    && inspected.iter().any(|mutation| {
                        matches!(
                            mutation.action(),
                            DefinitionAction::Alter | DefinitionAction::Drop
                        )
                    }) =>
            {
                if deferred.is_none() {
                    deferred = Some(DeferredMigrationPolicyError {
                        relative_path: migration.relative_path.clone().into_boxed_str(),
                        source,
                    });
                }
            }
            Err(source) => {
                return Err(source)
                    .with_context(|| format!("validate {} before apply", migration.relative_path));
            }
        }
        mutations.extend(
            inspected
                .into_iter()
                .map(|mutation| PlannedDefinitionMutation {
                    relative_path: migration.relative_path.clone().into_boxed_str(),
                    mutation,
                }),
        );
    }
    Ok(MigrationPolicyPlan {
        mutations,
        deferred,
    })
}

async fn apply(
    client: &mut tokio_postgres::Client,
    tenant: &str,
    coordinate_text: &str,
    directory: &PackageDirectory,
    manifest: &PackageManifest,
    migration_policy: MigrationPolicyPlan,
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
    let migration_changed = !plan.is_noop();
    let pending_paths = plan
        .pending
        .iter()
        .map(|migration| migration.relative_path.as_str())
        .collect::<BTreeSet<_>>();
    let MigrationPolicyPlan {
        mutations,
        deferred,
    } = migration_policy;
    let pending_mutations = mutations
        .iter()
        .filter(|planned| pending_paths.contains(planned.relative_path.as_ref()))
        .collect::<Vec<_>>();

    validate_definition_ownership_before_apply(
        &tx,
        tenant,
        coordinate_text,
        &package_id,
        manifest,
        &pending_mutations,
    )
    .await?;
    if let Some(deferred) = deferred {
        return Err(deferred.source)
            .with_context(|| format!("validate {} before apply", deferred.relative_path));
    }

    ensure_model_schemas(&tx, &plan).await?;
    for statement in &plan.statements {
        execute(&tx, statement, &coordinate_text).await?;
    }
    let ownership_changed = reconcile_definition_ownership(
        &tx,
        tenant,
        coordinate_text,
        &package_id,
        manifest,
        &pending_mutations,
    )
    .await?;
    reconcile_entity_maps(&tx, &plan, manifest).await?;
    let operation_grants =
        reconcile_package_operation_grants(&tx, &directory.manifest_bytes, tenant).await?;
    tx.commit().await.context("commit whole package suffix")?;
    Ok(ApplyOutcome {
        migrations_applied: applied_count,
        changed: migration_changed || ownership_changed || !operation_grants.is_noop(),
    })
}

async fn reconcile_package_operation_grants(
    tx: &Transaction<'_>,
    manifest_bytes: &[u8],
    tenant: &str,
) -> anyhow::Result<OperationGrantReconcileResult> {
    tx.query_one(OPERATION_GRANT_LOCK_SQL, &[&tenant])
        .await
        .context("lock the tenant operation-grant carrier")?;
    tx.batch_execute(OPERATION_GRANT_TRANSACTION_PRELUDE_SQL)
        .await
        .context("disable row filtering for package operation-grant reconciliation")?;
    tx.batch_execute(&operation_grant_floor_check_sql())
        .await
        .context("verify the application authorization floor")?;
    let statement = reconcile_operation_grants_sql(manifest_bytes, tenant)
        .context("derive exact package operation grants")?;
    let row = tx
        .query_one(&statement, &[])
        .await
        .context("reconcile exact package operation grants")?;
    Ok(OperationGrantReconcileResult::new(
        row.get("role_rows_changed"),
        row.get("grants_added"),
        row.get("grants_removed"),
    ))
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
        schema: None,
        relation: None,
        definition_kind: None,
        definition: None,
        owner_package: None,
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
        schema: None,
        relation: None,
        definition_kind: None,
        definition: None,
        owner_package: None,
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

async fn validate_definition_ownership_before_apply(
    tx: &Transaction<'_>,
    tenant: &str,
    coordinate: &str,
    package_id: &str,
    manifest: &PackageManifest,
    mutations: &[&PlannedDefinitionMutation],
) -> anyhow::Result<()> {
    validate_manifest_definition_owners(manifest)?;
    for planned in mutations {
        let mutation = &planned.mutation;
        match mutation.action() {
            DefinitionAction::Create => {
                preflight_create_relation(tx, tenant, coordinate, package_id, manifest, planned)
                    .await?;
            }
            DefinitionAction::Add => {
                preflight_add_definition(tx, tenant, coordinate, package_id, manifest, planned)
                    .await?;
            }
            DefinitionAction::Alter | DefinitionAction::Drop => {
                preflight_existing_definition_mutation(tx, tenant, coordinate, package_id, planned)
                    .await?;
            }
        }
    }
    Ok(())
}

fn validate_manifest_definition_owners(manifest: &PackageManifest) -> anyhow::Result<()> {
    let admitted = std::iter::once(manifest.package.id.as_str())
        .chain(
            manifest
                .base_dependencies
                .values()
                .map(|dependency| dependency.package.as_str()),
        )
        .collect::<BTreeSet<_>>();
    for (model_id, model) in &manifest.models {
        for (definition, owner) in std::iter::once(("relation", model.owner.as_str()))
            .chain(
                model
                    .field_owners
                    .iter()
                    .map(|(field, owner)| (field.as_str(), owner.as_str())),
            )
            .chain(
                model
                    .constraint_owners
                    .iter()
                    .map(|(constraint, owner)| (constraint.as_str(), owner.as_str())),
            )
        {
            ensure!(
                admitted.contains(owner),
                "definition-owner-undeclared: {model_id}.{definition} names {owner}, which is neither the package nor a declared base"
            );
        }
    }
    Ok(())
}

async fn preflight_create_relation(
    tx: &Transaction<'_>,
    tenant: &str,
    coordinate: &str,
    package_id: &str,
    manifest: &PackageManifest,
    planned: &PlannedDefinitionMutation,
) -> anyhow::Result<()> {
    let mutation = &planned.mutation;
    if let Some(owner) = load_definition_owner(
        tx,
        tenant,
        mutation.schema(),
        mutation.relation(),
        DefinitionKind::Relation,
        mutation.relation(),
    )
    .await?
    {
        let kind = if owner.package_id == package_id {
            ApplyPackageErrorKind::DefinitionOwnerConflict
        } else {
            ApplyPackageErrorKind::BaseDefinitionMutation
        };
        return Err(definition_error(
            kind,
            coordinate,
            planned,
            Some(owner.package_id.as_str()),
            "CREATE TABLE cannot replace an existing managed relation",
        )
        .into());
    }
    if definition_present(
        tx,
        mutation.schema(),
        mutation.relation(),
        DefinitionKind::Relation,
        mutation.relation(),
    )
    .await?
    {
        return Err(definition_error(
            ApplyPackageErrorKind::DefinitionOwnerConflict,
            coordinate,
            planned,
            None,
            "the live relation has no durable definition owner",
        )
        .into());
    }
    if let Some(model) = model_for_relation(manifest, mutation.schema(), mutation.relation())
        && model.owner != package_id
    {
        return Err(definition_error(
            ApplyPackageErrorKind::DefinitionOwnerDeclarationMissing,
            coordinate,
            planned,
            Some(model.owner.as_str()),
            "a package may create only a relation it declares as its own",
        )
        .into());
    }
    Ok(())
}

async fn preflight_add_definition(
    tx: &Transaction<'_>,
    tenant: &str,
    coordinate: &str,
    package_id: &str,
    manifest: &PackageManifest,
    planned: &PlannedDefinitionMutation,
) -> anyhow::Result<()> {
    let mutation = &planned.mutation;
    let Some(relation_owner) = load_definition_owner(
        tx,
        tenant,
        mutation.schema(),
        mutation.relation(),
        DefinitionKind::Relation,
        mutation.relation(),
    )
    .await?
    else {
        return Err(definition_error(
            ApplyPackageErrorKind::DefinitionOwnerConflict,
            coordinate,
            planned,
            None,
            "the target relation has no durable definition owner",
        )
        .into());
    };

    if relation_owner.package_id != package_id {
        if !relation_owner.client_field_extensible {
            return Err(definition_error(
                ApplyPackageErrorKind::RelationNotClientExtensible,
                coordinate,
                planned,
                Some(relation_owner.package_id.as_str()),
                "the base package must declare client_field_extensible before an overlay adds definitions",
            )
            .into());
        }
        let Some(model) = model_for_relation(manifest, mutation.schema(), mutation.relation())
        else {
            return Err(definition_error(
                ApplyPackageErrorKind::DefinitionOwnerDeclarationMissing,
                coordinate,
                planned,
                Some(relation_owner.package_id.as_str()),
                "the overlay manifest must name the shared relation and its base owner",
            )
            .into());
        };
        if model.owner != relation_owner.package_id
            || explicit_definition_owner(model, mutation.kind(), mutation.definition())
                != Some(package_id)
        {
            return Err(definition_error(
                ApplyPackageErrorKind::DefinitionOwnerDeclarationMissing,
                coordinate,
                planned,
                Some(relation_owner.package_id.as_str()),
                "an additive shared-relation definition must explicitly name the applying package as owner",
            )
            .into());
        }
    } else if let Some(model) = model_for_relation(manifest, mutation.schema(), mutation.relation())
        && explicit_definition_owner(model, mutation.kind(), mutation.definition())
            .is_some_and(|owner| owner != package_id)
    {
        return Err(definition_error(
            ApplyPackageErrorKind::DefinitionOwnerDeclarationMissing,
            coordinate,
            planned,
            explicit_definition_owner(model, mutation.kind(), mutation.definition()),
            "a package cannot add a definition declared as another package's property",
        )
        .into());
    }

    if let Some(owner) = load_definition_owner(
        tx,
        tenant,
        mutation.schema(),
        mutation.relation(),
        mutation.kind(),
        mutation.definition(),
    )
    .await?
    {
        let kind = if owner.package_id == package_id {
            ApplyPackageErrorKind::DefinitionOwnerConflict
        } else {
            ApplyPackageErrorKind::BaseDefinitionMutation
        };
        return Err(definition_error(
            kind,
            coordinate,
            planned,
            Some(owner.package_id.as_str()),
            "ADD cannot replace an existing managed definition",
        )
        .into());
    }
    if definition_present(
        tx,
        mutation.schema(),
        mutation.relation(),
        mutation.kind(),
        mutation.definition(),
    )
    .await?
    {
        return Err(definition_error(
            ApplyPackageErrorKind::DefinitionOwnerConflict,
            coordinate,
            planned,
            None,
            "the live definition has no durable definition owner",
        )
        .into());
    }
    Ok(())
}

async fn preflight_existing_definition_mutation(
    tx: &Transaction<'_>,
    tenant: &str,
    coordinate: &str,
    package_id: &str,
    planned: &PlannedDefinitionMutation,
) -> anyhow::Result<()> {
    let mutation = &planned.mutation;
    if let Some(owner) = load_definition_owner(
        tx,
        tenant,
        mutation.schema(),
        mutation.relation(),
        mutation.kind(),
        mutation.definition(),
    )
    .await?
    {
        if owner.package_id != package_id {
            return Err(definition_error(
                ApplyPackageErrorKind::BaseDefinitionMutation,
                coordinate,
                planned,
                Some(owner.package_id.as_str()),
                "an overlay may not alter or drop a definition owned by its base",
            )
            .into());
        }
    } else if definition_present(
        tx,
        mutation.schema(),
        mutation.relation(),
        mutation.kind(),
        mutation.definition(),
    )
    .await?
    {
        return Err(definition_error(
            ApplyPackageErrorKind::DefinitionOwnerConflict,
            coordinate,
            planned,
            None,
            "the live definition has no durable definition owner",
        )
        .into());
    }
    Ok(())
}

async fn reconcile_definition_ownership(
    tx: &Transaction<'_>,
    tenant: &str,
    coordinate: &str,
    package_id: &str,
    manifest: &PackageManifest,
    mutations: &[&PlannedDefinitionMutation],
) -> anyhow::Result<bool> {
    let mut changed = false;
    for planned in mutations {
        let mutation = &planned.mutation;
        match mutation.action() {
            DefinitionAction::Create => {
                ensure_definition_present(tx, coordinate, planned).await?;
                let extensible =
                    model_for_relation(manifest, mutation.schema(), mutation.relation())
                        .filter(|model| model.owner == package_id)
                        .is_some_and(|model| model.client_field_extensible);
                changed |= insert_definition_owner(
                    tx,
                    tenant,
                    coordinate,
                    planned,
                    DefinitionKind::Relation,
                    mutation.relation(),
                    package_id,
                    extensible,
                )
                .await?;
                for row in tx
                    .query(
                        SELECT_RELATION_DEFINITIONS_SQL,
                        &[&mutation.schema(), &mutation.relation()],
                    )
                    .await
                    .context("read server-derived relation definitions")?
                {
                    let kind = match row.get::<_, String>(0).as_str() {
                        "field" => DefinitionKind::Field,
                        "constraint" => DefinitionKind::Constraint,
                        value => unreachable!("closed server definition kind {value}"),
                    };
                    let definition = row.get::<_, String>(1);
                    changed |= insert_definition_owner(
                        tx,
                        tenant,
                        coordinate,
                        planned,
                        kind,
                        &definition,
                        package_id,
                        false,
                    )
                    .await?;
                }
            }
            DefinitionAction::Add => {
                ensure_definition_present(tx, coordinate, planned).await?;
                changed |= insert_definition_owner(
                    tx,
                    tenant,
                    coordinate,
                    planned,
                    mutation.kind(),
                    mutation.definition(),
                    package_id,
                    false,
                )
                .await?;
            }
            DefinitionAction::Alter | DefinitionAction::Drop => {
                unreachable!("migration policy refuses non-additive DDL before execution")
            }
        }
    }
    Ok(changed)
}

async fn ensure_definition_present(
    tx: &Transaction<'_>,
    coordinate: &str,
    planned: &PlannedDefinitionMutation,
) -> anyhow::Result<()> {
    let mutation = &planned.mutation;
    if definition_present(
        tx,
        mutation.schema(),
        mutation.relation(),
        mutation.kind(),
        mutation.definition(),
    )
    .await?
    {
        Ok(())
    } else {
        Err(definition_error(
            ApplyPackageErrorKind::DefinitionNotFound,
            coordinate,
            planned,
            None,
            "PostgreSQL did not expose the definition after its migration statement",
        )
        .into())
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the parameters are the exact durable definition-owner row"
)]
async fn insert_definition_owner(
    tx: &Transaction<'_>,
    tenant: &str,
    coordinate: &str,
    planned: &PlannedDefinitionMutation,
    kind: DefinitionKind,
    definition: &str,
    package_id: &str,
    client_field_extensible: bool,
) -> anyhow::Result<bool> {
    let mutation = &planned.mutation;
    let inserted = tx
        .execute(
            INSERT_DEFINITION_OWNER_SQL,
            &[
                &tenant,
                &mutation.schema(),
                &mutation.relation(),
                &kind.as_str(),
                &definition,
                &package_id,
                &client_field_extensible,
            ],
        )
        .await
        .context("record server-derived definition owner")?;
    if inserted == 1 {
        return Ok(true);
    }
    let existing = load_definition_owner(
        tx,
        tenant,
        mutation.schema(),
        mutation.relation(),
        kind,
        definition,
    )
    .await?
    .expect("the conflicting definition owner row exists");
    if existing.package_id == package_id
        && existing.client_field_extensible == client_field_extensible
    {
        Ok(false)
    } else {
        Err(definition_error_for_parts(
            ApplyPackageErrorKind::DefinitionOwnerConflict,
            coordinate,
            planned,
            kind,
            definition,
            Some(existing.package_id.as_str()),
            "the durable owner fact disagrees with the applying package",
        )
        .into())
    }
}

async fn load_definition_owner(
    tx: &Transaction<'_>,
    tenant: &str,
    schema: &str,
    relation: &str,
    kind: DefinitionKind,
    definition: &str,
) -> anyhow::Result<Option<StoredDefinitionOwner>> {
    tx.query_opt(
        SELECT_DEFINITION_OWNER_SQL,
        &[&tenant, &schema, &relation, &kind.as_str(), &definition],
    )
    .await
    .context("read durable definition owner")
    .map(|row| {
        row.map(|row| StoredDefinitionOwner {
            package_id: row.get(0),
            client_field_extensible: row.get(1),
        })
    })
}

async fn definition_present(
    tx: &Transaction<'_>,
    schema: &str,
    relation: &str,
    kind: DefinitionKind,
    definition: &str,
) -> anyhow::Result<bool> {
    let row = match kind {
        DefinitionKind::Relation => {
            tx.query_one(SELECT_RELATION_PRESENT_SQL, &[&schema, &relation])
                .await
        }
        DefinitionKind::Field => {
            tx.query_one(SELECT_FIELD_PRESENT_SQL, &[&schema, &relation, &definition])
                .await
        }
        DefinitionKind::Constraint => {
            tx.query_one(
                SELECT_CONSTRAINT_PRESENT_SQL,
                &[&schema, &relation, &definition],
            )
            .await
        }
    }
    .context("read server definition presence")?;
    Ok(row.get(0))
}

fn model_for_relation<'a>(
    manifest: &'a PackageManifest,
    schema: &str,
    relation: &str,
) -> Option<&'a ModelDeclaration> {
    manifest
        .models
        .values()
        .find(|model| model.schema == schema && model.table == relation)
}

fn explicit_definition_owner<'a>(
    model: &'a ModelDeclaration,
    kind: DefinitionKind,
    definition: &str,
) -> Option<&'a str> {
    match kind {
        DefinitionKind::Relation => Some(model.owner.as_str()),
        DefinitionKind::Field => model.field_owners.get(definition).map(String::as_str),
        DefinitionKind::Constraint => model.constraint_owners.get(definition).map(String::as_str),
    }
}

fn definition_error(
    kind: ApplyPackageErrorKind,
    coordinate: &str,
    planned: &PlannedDefinitionMutation,
    owner_package: Option<&str>,
    detail: impl Into<String>,
) -> ApplyPackageError {
    definition_error_for_parts(
        kind,
        coordinate,
        planned,
        planned.mutation.kind(),
        planned.mutation.definition(),
        owner_package,
        detail,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the arguments preserve exact refusal context at the effect boundary"
)]
fn definition_error_for_parts(
    kind: ApplyPackageErrorKind,
    coordinate: &str,
    planned: &PlannedDefinitionMutation,
    definition_kind: DefinitionKind,
    definition: &str,
    owner_package: Option<&str>,
    detail: impl Into<String>,
) -> ApplyPackageError {
    ApplyPackageError {
        kind,
        coordinate: coordinate.to_owned(),
        predecessor_version: None,
        current_version: None,
        path: Some(planned.relative_path.to_string()),
        schema: Some(planned.mutation.schema().to_owned()),
        relation: Some(planned.mutation.relation().to_owned()),
        definition_kind: Some(definition_kind),
        definition: Some(definition.to_owned()),
        owner_package: owner_package.map(str::to_owned),
        detail: detail.into(),
        source: None,
    }
}

async fn reconcile_entity_maps(
    tx: &Transaction<'_>,
    plan: &wamn_schema_control::PackageMigrationPlan,
    manifest: &PackageManifest,
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
        let relation_owner = &manifest
            .models
            .get(&model.model_id)
            .expect("the migration plan preserves every manifest model")
            .owner;
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
            if mapped_package != relation_owner || mapped_entity != model.model_id.as_str() {
                bail!(
                    "package-entity-oid-rebind-refused: {}.{} is already mapped to {mapped_package}/{mapped_entity}; cannot rebind it to {}/{}",
                    model.schema,
                    model.table,
                    relation_owner,
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
                &[relation_owner, &model.model_id, &model.table],
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
            schema: None,
            relation: None,
            definition_kind: None,
            definition: None,
            owner_package: None,
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

//! Apply one package-owned migration stream exactly once per immutable file.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use clap::Args;
use tokio_postgres::{NoTls, Transaction, error::SqlState};
use wamn_control_provision::audit_retention::{
    AUDIT_RETENTION_LOCK_SQL, reconcile_audit_retention_grants_sql,
};
use wamn_control_provision::operation_grants::{
    OPERATION_GRANT_LOCK_SQL, OPERATION_GRANT_TRANSACTION_PRELUDE_SQL,
    OperationGrantReconcileResult, operation_grant_floor_check_sql, reconcile_operation_grants_sql,
};
use wamn_control_provision::{
    AUDIT_RETENTION_ROLE, DB_OWNER_ROLE, PlatformComponent, bind_platform_principal_sql,
};
use wamn_event_reg::{
    DELETE_STALE_CATALOG_REGISTRATIONS_SQL, EventRegistration, RegistrationInput,
    UPSERT_CATALOG_REGISTRATION_SQL, project_catalog_registrations,
};
use wamn_pg_core::{Identifier, QualifiedName};
use wamn_schema_control::{
    AppliedPackage, MigrationSource, PackageDirectory, PackageMigrationError, RecordedMigration,
    SqlStatement, plan_package_migrations, plan_package_registration,
};
use wamn_schema_generator::{ModelDeclaration, PackageManifest, RecordHistoryColumn};
use wamn_schema_introspection::migration_policy::{
    DefinitionAction, DefinitionKind, DefinitionMutation, MigrationPolicyError,
    MigrationPolicyErrorKind, inspect_migration_definition_mutations,
};
use wamn_schema_introspection::postgres::{read_record_history_logs, read_record_history_stamps};
use wamn_schema_introspection::record_history::{
    HISTORY_TABLE_SUFFIX, RECORD_HISTORY_LOG_TRIGGER, history_table_name, is_history_table_name,
};

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";
const SELECT_ROLE_CONTEXT_SQL: &str = "SELECT current_user::text, session_user::text";
pub(crate) const LOCK_PACKAGE_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.package.lineage:' || $1 || ':' || $2, 0))";
const SELECT_PACKAGE_SQL: &str = "\
SELECT manifest_sha256, predecessor_version FROM catalog.packages \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
 FOR UPDATE";
const SELECT_MIGRATIONS_SQL: &str = "\
SELECT ordinal, relative_path, sha256 FROM catalog.package_migrations \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
 ORDER BY ordinal";
pub(crate) const SELECT_CURRENT_PACKAGE_VERSION_SQL: &str = "\
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

    /// Tenant stored with the package and migration records.
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
    ensure!(
        !plan.models.is_empty(),
        "package-migration-schema-missing: manifest must map at least one model to an application schema"
    );
    let mut schemas = plan
        .models
        .iter()
        .map(|model| model.schema.as_str())
        .chain(
            plan.cdc_excluded_relations
                .iter()
                .map(|relation| relation.schema.as_str()),
        )
        .collect::<Vec<_>>();
    schemas.sort_unstable();
    schemas.dedup();
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
    validate_relation_classifications(plan, &mutations)?;
    Ok(MigrationPolicyPlan {
        mutations,
        deferred,
    })
}

fn validate_relation_classifications(
    plan: &wamn_schema_control::PackageMigrationPlan,
    mutations: &[PlannedDefinitionMutation],
) -> anyhow::Result<()> {
    let created = mutations
        .iter()
        .filter(|planned| planned.mutation.action() == DefinitionAction::Create)
        .map(|planned| {
            (
                planned.mutation.schema(),
                planned.mutation.relation(),
                planned.relative_path.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    for (schema, relation, path) in &created {
        ensure!(
            !is_history_table_name(relation),
            "history-table-name-reserved: {path} creates {schema}.{relation}, but the {HISTORY_TABLE_SUFFIX} suffix is reserved for the history tables that apply-package creates"
        );
        let modeled = plan
            .models
            .iter()
            .any(|model| model.schema == *schema && model.table == *relation);
        let excluded = plan
            .cdc_excluded_relations
            .iter()
            .any(|item| item.schema == *schema && item.table == *relation);
        ensure!(
            modeled || excluded,
            "{DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL}: {path} creates {schema}.{relation}, but wamn.json declares it as neither a model nor an internal relation with cdc excluded"
        );
    }
    // apply-package creates each history table, so no migration creates one.
    for excluded in plan
        .cdc_excluded_relations
        .iter()
        .filter(|excluded| !is_history_table_name(&excluded.table))
    {
        ensure!(
            created.iter().any(|(schema, relation, _)| {
                *schema == excluded.schema && *relation == excluded.table
            }),
            "cdc-excluded-relation-missing: {} names {}.{}, but the package migration stream does not create it",
            excluded.relation_id,
            excluded.schema,
            excluded.table
        );
    }
    Ok(())
}

async fn apply(
    client: &mut tokio_postgres::Client,
    tenant: &str,
    coordinate_text: &str,
    directory: &PackageDirectory,
    manifest: &PackageManifest,
    migration_policy: MigrationPolicyPlan,
) -> anyhow::Result<ApplyOutcome> {
    wamn_schema_generator::validate_operation_vocabulary(manifest)
        .context("validate package manifest for registration projection")?;
    let registrations = derive_catalog_registrations(manifest);
    let presented =
        plan_package_migrations(directory, None).context("validate package directory")?;
    let package_id = presented.coordinate.package_id().to_owned();
    let package_version = presented.coordinate.package_version().to_owned();
    let tx = client.transaction().await.context("begin package apply")?;
    tx.query_one(CLAIM_TENANT_SQL, &[&tenant])
        .await
        .context("claim package tenant")?;
    bind_apply_package_principal(&tx).await?;
    tx.query_one(LOCK_PACKAGE_SQL, &[&tenant, &package_id])
        .await
        .context("lock package family")?;

    let applied = load_applied_package(&tx, tenant, &package_id, &package_version).await?;
    let plan = if let Some(applied) = applied.as_ref() {
        plan_package_migrations(directory, Some(applied))
            .context("compare package bytes with immutable records")?
    } else {
        match current_package_version(&tx, tenant, &package_id).await? {
            None => presented,
            Some(current_version) => {
                plan_package_registration(
                    &presented.coordinate,
                    &presented.manifest_sha256,
                    presented.predecessor_version.as_deref(),
                    None,
                    Some(&current_version),
                )
                .map_err(|_| {
                    predecessor_not_current_error(
                        coordinate_text,
                        presented.predecessor_version.as_deref(),
                        &current_version,
                    )
                })?;
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

    // wamn-yk9l. The platform extensions go in FIRST, while this connection is
    // still the administrator: `CREATE EXTENSION` is refused to a package by
    // the migration policy and is not a privilege the package-owner role holds.
    // Idempotent, so it converges a database provisioned before the list
    // existed.
    tx.batch_execute(&wamn_control_provision::sql::install_platform_extensions_sql())
        .await
        .context("install the platform extensions")?;

    // Package text cannot issue SET ROLE: the pre-apply policy rejects a
    // package escalating itself. This host-issued SET LOCAL ROLE moves in the
    // opposite direction, narrowing the administrator to the existing
    // package-owner role while package DDL runs.
    set_package_owner_role(&tx).await?;
    ensure_model_schemas(&tx, &plan).await?;
    reset_host_role(&tx).await?;
    let package_inserted = register_package(
        &tx,
        tenant,
        &plan.coordinate,
        &plan.manifest_sha256,
        plan.predecessor_version.as_deref(),
    )
    .await
    .context("register package before applying migrations")?;
    for statement in &plan.statements {
        // The planner carries exact package bytes as its parameter-free batch
        // statements; every host-authored record statement has binds.
        if statement.params.is_empty() {
            set_package_owner_role(&tx).await?;
            execute(&tx, statement, &coordinate_text).await?;
            reset_host_role(&tx).await?;
        } else {
            assert_host_role(&tx).await?;
            execute(&tx, statement, &coordinate_text).await?;
        }
    }
    assert_host_role(&tx).await?;
    let ownership_changed = reconcile_definition_ownership(
        &tx,
        tenant,
        coordinate_text,
        &package_id,
        manifest,
        &pending_mutations,
    )
    .await?;
    let history_changed = create_history_tables(&tx, manifest).await?;
    reconcile_entity_maps(&tx, &plan, manifest).await?;
    let triggers_changed = reconcile_record_history_triggers(&tx, manifest).await?;
    let operation_grants =
        reconcile_package_operation_grants(&tx, &directory.manifest_bytes, tenant).await?;
    let registrations_changed =
        reconcile_package_registrations(&tx, tenant, &package_id, &registrations).await?;
    tx.commit().await.context("commit whole package suffix")?;
    Ok(ApplyOutcome {
        migrations_applied: applied_count,
        changed: package_inserted
            || migration_changed
            || ownership_changed
            || history_changed
            || triggers_changed
            || !operation_grants.is_noop()
            || registrations_changed,
    })
}

/// Reconcile mutable local configuration only after confirming the installed schema.
pub(crate) async fn reconcile_local_package_configuration(
    tx: &Transaction<'_>,
    tenant: &str,
    directory: &PackageDirectory,
) -> anyhow::Result<()> {
    bind_apply_package_principal(tx).await?;
    let plan = plan_package_migrations(directory, None)?;
    let installed = load_applied_package(
        tx,
        tenant,
        plan.coordinate.package_id(),
        plan.coordinate.package_version(),
    )
    .await?
    .context("local configuration requires an applied package schema")?;
    let expected = plan
        .pending
        .iter()
        .map(|migration| RecordedMigration {
            ordinal: migration.ordinal,
            relative_path: migration.relative_path.clone(),
            sha256: migration.sha256.clone(),
        })
        .collect::<Vec<_>>();
    ensure!(
        installed.predecessor_version == plan.predecessor_version
            && installed.migrations == expected,
        "local schema inputs changed; recreate the owned disposable target"
    );
    // The relation classification check of apply refuses a manifest that drops
    // a model whose relation the installed migrations create.
    validate_migration_policy(Path::new(""), directory, &plan)?;
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes)?;
    create_history_tables(tx, &manifest).await?;
    reconcile_entity_maps(tx, &plan, &manifest).await?;
    reconcile_record_history_triggers(tx, &manifest).await?;
    reconcile_package_operation_grants(tx, &directory.manifest_bytes, tenant).await?;
    reconcile_package_registrations(
        tx,
        tenant,
        plan.coordinate.package_id(),
        &derive_catalog_registrations(&manifest),
    )
    .await?;
    Ok(())
}

/// Create the history table of each owned relation whose declaration keeps a log.
///
/// `wamn_history.create_history_table` is the one definition of the table
/// shape. apply-package never drops a history table, so a relation whose
/// retention becomes `none` keeps its table and its entries.
async fn create_history_tables(
    tx: &Transaction<'_>,
    manifest: &PackageManifest,
) -> anyhow::Result<bool> {
    let mut changed = false;
    for model in manifest
        .models
        .values()
        .filter(|model| model.owner == manifest.package.id && model.log_retention().is_some())
    {
        let history = history_table_name(&model.table);
        let present = tx
            .query_one(SELECT_RELATION_PRESENT_SQL, &[&model.schema, &history])
            .await
            .with_context(|| format!("read history table {}.{history}", model.schema))?
            .get::<_, bool>(0);
        if present {
            continue;
        }
        // The package-owner role owns the relation, so it owns its history table.
        set_package_owner_role(tx).await?;
        tx.execute(
            "SELECT wamn_history.create_history_table($1, $2, false)",
            &[&model.schema, &model.table],
        )
        .await
        .with_context(|| format!("create history table {}.{history}", model.schema))?;
        reset_host_role(tx).await?;
        changed = true;
    }
    Ok(changed)
}

/// Make each owned relation carry exactly the record-history triggers that its declaration names.
///
/// The triggers are derived state, like the operation grants. A declaration
/// that selects no column has no stamp trigger, and a retention of `none` has
/// no log trigger. A trigger that a declaration no longer needs is removed. The
/// log trigger carries the retention as its one argument. The installed
/// triggers are then read back through introspection and compared with the
/// declarations.
///
/// The step takes the audit retention lock first, so a retention run never
/// sees a retention change between its read and its delete. It ends with the
/// audit retention grants, which follow the installed log triggers.
async fn reconcile_record_history_triggers(
    tx: &Transaction<'_>,
    manifest: &PackageManifest,
) -> anyhow::Result<bool> {
    tx.query_one(AUDIT_RETENTION_LOCK_SQL, &[])
        .await
        .context("lock the audit retention source")?;
    let owned = manifest
        .models
        .iter()
        .filter(|(_, model)| model.owner == manifest.package.id)
        .map(|(model_id, model)| {
            let audit_log = model.audit_log.as_ref().with_context(|| {
                format!("{model_id} owns its relation and must declare audit_log")
            })?;
            Ok((
                (model.schema.clone(), model.table.clone()),
                model,
                audit_log,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let declared = owned
        .iter()
        .map(|(relation, _, audit_log)| {
            let columns = RecordHistoryColumn::ALL
                .into_iter()
                .filter(|column| audit_log.columns.contains(column))
                .map(|column| column.as_str().to_owned())
                .collect::<Vec<_>>();
            (relation.clone(), columns)
        })
        .collect::<BTreeMap<_, _>>();
    let declared_logs = owned
        .iter()
        .map(|(relation, model, _)| (relation.clone(), model.log_retention().map(str::to_owned)))
        .collect::<BTreeMap<_, _>>();
    let schemas = declared
        .keys()
        .map(|(schema, _)| schema.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let installed = read_record_history_stamps(tx, &schemas)
        .await
        .context("read installed record-history triggers")?;
    let installed_logs = read_record_history_logs(tx, &schemas)
        .await
        .context("read installed record-history log triggers")?;

    let mut statements = Vec::new();
    for (relation, columns) in &declared {
        if installed.get(relation).map_or(&[][..], Vec::as_slice) == columns.as_slice() {
            continue;
        }
        let quoted = quoted_relation(relation)?;
        statements.push(if columns.is_empty() {
            format!("DROP TRIGGER record_history_stamp ON {quoted}")
        } else {
            format!(
                "CREATE OR REPLACE TRIGGER record_history_stamp \
                 BEFORE INSERT OR UPDATE ON {quoted} \
                 FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row({})",
                columns
                    .iter()
                    .map(|column| format!("'{column}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
    }
    for (relation, retention) in &declared_logs {
        if installed_logs.get(relation) == retention.as_ref() {
            continue;
        }
        let quoted = quoted_relation(relation)?;
        statements.push(match retention {
            None => format!("DROP TRIGGER {RECORD_HISTORY_LOG_TRIGGER} ON {quoted}"),
            Some(retention) => format!(
                "CREATE OR REPLACE TRIGGER {RECORD_HISTORY_LOG_TRIGGER} \
                 AFTER INSERT OR UPDATE OR DELETE ON {quoted} \
                 FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('{}')",
                retention.replace('\'', "''")
            ),
        });
    }
    for statement in &statements {
        // The package-owner role owns the relation, so it creates the trigger.
        set_package_owner_role(tx).await?;
        tx.batch_execute(statement)
            .await
            .with_context(|| format!("reconcile a record-history trigger: {statement}"))?;
        reset_host_role(tx).await?;
    }

    let installed = read_record_history_stamps(tx, &schemas)
        .await
        .context("read reconciled record-history triggers")?;
    for (relation, columns) in &declared {
        let observed = installed.get(relation).map_or(&[][..], Vec::as_slice);
        ensure!(
            observed == columns.as_slice(),
            "record-history-trigger-mismatch: {}.{} declares {columns:?}, but its installed trigger selects {observed:?}",
            relation.0,
            relation.1
        );
    }
    let installed_logs = read_record_history_logs(tx, &schemas)
        .await
        .context("read reconciled record-history log triggers")?;
    for (relation, retention) in &declared_logs {
        let observed = installed_logs.get(relation);
        ensure!(
            observed == retention.as_ref(),
            "record-history-log-mismatch: {}.{} declares retention {retention:?}, but its installed log trigger carries {observed:?}",
            relation.0,
            relation.1
        );
    }
    let grants_changed = reconcile_audit_retention_grants(tx).await?;
    Ok(!statements.is_empty() || grants_changed)
}

/// Grant the audit retention role exactly its privileges on the history tables
/// whose log trigger carries `P<n>D`, and revoke every other privilege.
///
/// The package-owner role owns the history tables, so it issues the grants.
/// The result reports whether the grants of the role changed.
async fn reconcile_audit_retention_grants(tx: &Transaction<'_>) -> anyhow::Result<bool> {
    let read_grants = || async {
        tx.query(
            wamn_control_provision::sql::role_database_grants_sql(),
            &[&AUDIT_RETENTION_ROLE],
        )
        .await
        .context("read the audit retention grants")
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    (
                        row.get::<_, String>("object_kind"),
                        row.get::<_, String>("schema_name"),
                        row.get::<_, String>("object_name"),
                        row.get::<_, String>("privilege_type"),
                    )
                })
                .collect::<Vec<_>>()
        })
    };
    let before = read_grants().await?;
    set_package_owner_role(tx).await?;
    tx.batch_execute(&reconcile_audit_retention_grants_sql())
        .await
        .context("reconcile the audit retention grants")?;
    reset_host_role(tx).await?;
    Ok(read_grants().await? != before)
}

fn quoted_relation(relation: &(String, String)) -> anyhow::Result<String> {
    Ok(QualifiedName::new(
        Identifier::new(relation.0.as_str())?,
        Identifier::new(relation.1.as_str())?,
    )
    .quoted())
}

async fn set_package_owner_role(tx: &Transaction<'_>) -> anyhow::Result<()> {
    tx.batch_execute(&format!("SET LOCAL ROLE \"{DB_OWNER_ROLE}\""))
        .await
        .context("narrow package migration authority to wamn_db_owner")
}

async fn reset_host_role(tx: &Transaction<'_>) -> anyhow::Result<()> {
    tx.batch_execute("RESET ROLE")
        .await
        .context("reset package migration authority before trusted writes")?;
    assert_host_role(tx).await
}

async fn assert_host_role(tx: &Transaction<'_>) -> anyhow::Result<()> {
    let row = tx
        .query_one(SELECT_ROLE_CONTEXT_SQL, &[])
        .await
        .context("read server role context before trusted package writes")?;
    let current_role = row.get::<_, String>(0);
    let session_role = row.get::<_, String>(1);
    ensure!(
        current_role == session_role,
        "package-role-reset-refused: current role {current_role:?} differs from session role {session_role:?}"
    );
    Ok(())
}

fn derive_catalog_registrations(
    manifest: &wamn_schema_generator::PackageManifest,
) -> BTreeMap<String, EventRegistration> {
    let mut declarations = BTreeMap::new();
    for (operation_key, operation) in &manifest.custom_operations {
        let Some(registration) = operation.registration() else {
            continue;
        };
        let declaration = EventRegistration {
            schema_version: wamn_event_reg::SCHEMA_VERSION.to_owned(),
            registration_id: operation_key.clone(),
            package_id: manifest.package.id.clone(),
            source_package_id: registration.source_package.clone(),
            entity: registration.entity.clone(),
            ops: registration.ops.clone(),
            input: RegistrationInput::Event,
            condition: None,
        };
        declarations.insert(operation_key.clone(), declaration);
    }
    declarations
}

async fn reconcile_package_registrations(
    tx: &Transaction<'_>,
    tenant: &str,
    package_id: &str,
    declarations: &BTreeMap<String, EventRegistration>,
) -> anyhow::Result<bool> {
    let projection = project_catalog_registrations(package_id, declarations)
        .context("derive exact package registration rows")?;
    let mut changed = false;
    for row in &projection.rows {
        changed |= tx
            .execute(
                UPSERT_CATALOG_REGISTRATION_SQL,
                &[
                    &tenant,
                    &projection.package_id,
                    &row.registration_id,
                    &row.entity_id,
                    &row.registration_json,
                ],
            )
            .await
            .with_context(|| format!("reconcile registration {:?}", row.registration_id))?
            > 0;
    }
    changed |= tx
        .execute(
            DELETE_STALE_CATALOG_REGISTRATIONS_SQL,
            &[
                &tenant,
                &projection.package_id,
                &projection.retained_registration_ids,
            ],
        )
        .await
        .context("delete stale package registrations")?
        > 0;
    Ok(changed)
}

/// Bind `wamn:apply-package` as the actor and the operation of the
/// transaction, so its writes, including operation grants, record that
/// component.
async fn bind_apply_package_principal(tx: &Transaction<'_>) -> anyhow::Result<()> {
    tx.batch_execute(&bind_platform_principal_sql(
        PlatformComponent::ApplyPackage,
    ))
    .await
    .context("bind wamn:apply-package as the transaction actor and operation")
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

/// Register a package while retaining its lineage lock through the caller's transaction.
pub(crate) async fn register_package(
    tx: &Transaction<'_>,
    tenant: &str,
    coordinate: &wamn_catalog::PackageCoordinate,
    manifest_sha256: &str,
    predecessor_version: Option<&str>,
) -> anyhow::Result<bool> {
    let package_id = coordinate.package_id();
    tx.query_one(LOCK_PACKAGE_SQL, &[&tenant, &package_id])
        .await
        .context("lock package lineage before registration")?;
    let recorded = tx
        .query_opt(
            SELECT_PACKAGE_SQL,
            &[&tenant, &package_id, &coordinate.package_version()],
        )
        .await
        .context("read package coordinate before registration")?;
    let recorded = recorded.map(|row| (row.get::<_, String>(0), row.get::<_, Option<String>>(1)));
    let current = current_package_version(tx, tenant, package_id).await?;
    let insert = plan_package_registration(
        coordinate,
        manifest_sha256,
        predecessor_version,
        recorded
            .as_ref()
            .map(|(hash, predecessor)| (hash.as_str(), predecessor.as_deref())),
        current.as_deref(),
    )?;
    if insert {
        tx.execute(
            "INSERT INTO catalog.packages \
             (tenant_id, package_id, package_version, manifest_sha256, predecessor_version) \
             VALUES ($1, $2, $3, $4, $5)",
            &[
                &tenant,
                &package_id,
                &coordinate.package_version(),
                &manifest_sha256,
                &predecessor_version,
            ],
        )
        .await
        .context("insert immutable package root")?;
    }
    Ok(insert)
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
        .chain(
            plan.cdc_excluded_relations
                .iter()
                .map(|relation| relation.schema.as_str()),
        )
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
        .chain(
            plan.cdc_excluded_relations
                .iter()
                .map(|relation| relation.schema.as_str()),
        )
        .collect::<BTreeSet<_>>();
    for schema in schemas {
        tx.batch_execute(&wamn_control_provision::sql::ensure_entity_map_sql(schema))
            .await
            .with_context(|| format!("ensure {schema}.wamn_entities"))?;
        tx.batch_execute(&wamn_control_provision::sql::ensure_cdc_exclusion_map_sql(
            schema,
        ))
        .await
        .with_context(|| format!("ensure {schema}.wamn_cdc_exclusions"))?;
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
            "SELECT mapped.package_id, mapped.entity_id, mapped.table_name, \
                    excluded.relation_oid IS NOT NULL AS cdc_excluded \
               FROM pg_catalog.pg_class AS relation \
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
               LEFT JOIN {}.wamn_entities AS mapped ON mapped.relation_oid = relation.oid \
               LEFT JOIN {}.wamn_cdc_exclusions AS excluded ON excluded.relation_oid = relation.oid \
              WHERE namespace.nspname = $1 AND relation.relname = $2 \
                AND relation.relkind = 'r'",
            schema.quoted(),
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
        ensure!(
            !mapping.get::<_, bool>(3),
            "package-relation-classification-conflict: {}.{} is already CDC-excluded and cannot also map to model {}",
            model.schema,
            model.table,
            model.model_id
        );
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

    for excluded in &plan.cdc_excluded_relations {
        let schema = wamn_schema_control::BareSchemaName::new(&excluded.schema)
            .context("validate internal relation schema for CDC-exclusion query")?;
        let mapping_sql = format!(
            "SELECT mapped.package_id, mapped.relation_id, mapped.table_name, \
                    entity.relation_oid IS NOT NULL AS entity_mapped \
               FROM pg_catalog.pg_class AS relation \
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
               LEFT JOIN {}.wamn_cdc_exclusions AS mapped ON mapped.relation_oid = relation.oid \
               LEFT JOIN {}.wamn_entities AS entity ON entity.relation_oid = relation.oid \
              WHERE namespace.nspname = $1 AND relation.relname = $2 \
                AND relation.relkind = 'r'",
            schema.quoted(),
            schema.quoted()
        );
        let Some(mapping) = tx
            .query_opt(&mapping_sql, &[&excluded.schema, &excluded.table])
            .await
            .with_context(|| {
                format!(
                    "read CDC exclusion map {}.{} ({})",
                    excluded.schema, excluded.table, excluded.relation_id
                )
            })?
        else {
            bail!(
                "cdc-excluded-relation-missing: {} maps {}.{}, but the migration stream did not create that table",
                excluded.relation_id,
                excluded.schema,
                excluded.table
            );
        };
        ensure!(
            !mapping.get::<_, bool>(3),
            "package-relation-classification-conflict: {}.{} is already entity-mapped and cannot also be CDC-excluded",
            excluded.schema,
            excluded.table
        );
        let mapped_package = mapping.get::<_, Option<String>>(0);
        let mapped_relation = mapping.get::<_, Option<String>>(1);
        let mapped_table = mapping.get::<_, Option<String>>(2);
        if let (Some(mapped_package), Some(mapped_relation)) =
            (mapped_package.as_deref(), mapped_relation.as_deref())
        {
            if mapped_package != manifest.package.id
                || mapped_relation != excluded.relation_id.as_str()
            {
                bail!(
                    "package-cdc-exclusion-oid-rebind-refused: {}.{} is already mapped to {mapped_package}/{mapped_relation}; cannot rebind it to {}/{}",
                    excluded.schema,
                    excluded.table,
                    manifest.package.id,
                    excluded.relation_id
                );
            }
            if mapped_table.as_deref() == Some(excluded.table.as_str()) {
                continue;
            }
        }
        let mapped = tx
            .execute(
                &wamn_control_provision::sql::upsert_cdc_exclusion_map_sql(&excluded.schema),
                &[&manifest.package.id, &excluded.relation_id, &excluded.table],
            )
            .await
            .with_context(|| {
                format!(
                    "upsert CDC exclusion {}.{} ({})",
                    excluded.schema, excluded.table, excluded.relation_id
                )
            })?;
        ensure!(mapped == 1, "package-cdc-exclusion-map-write-refused");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_registration_projection_does_not_require_redeclaring_the_base_entity() {
        let manifest: wamn_schema_generator::PackageManifest = serde_json::from_str(include_str!(
            "../../../apps/client_acme_receiving/wamn.json"
        ))
        .expect("the repository overlay manifest parses");
        wamn_schema_generator::validate_operation_vocabulary(&manifest)
            .expect("the overlay operation vocabulary is valid without base model restatement");

        let declarations = derive_catalog_registrations(&manifest);
        let registration = &declarations["quality.create_inspection"];
        assert_eq!(registration.registration_id, "quality.create_inspection");
        assert_eq!(registration.package_id, "client_acme_receiving");
        assert_eq!(registration.source_package_id, "wamn_receiving");
        assert_eq!(registration.entity, "receipt");
        assert_eq!(registration.ops, [wamn_event_reg::Op::Insert]);
    }

    /// Ruling 69: the local configuration path runs the relation classification
    /// check of apply-package. A manifest that drops a model whose relation the
    /// installed migrations create gets the refusal of apply.
    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL 18 in WAMN_CTL_PG_URL"]
    async fn local_configuration_refuses_a_removed_model_with_the_apply_refusal() {
        const TENANT: &str = "local-configuration";
        let url = std::env::var("WAMN_CTL_PG_URL").expect("owned PostgreSQL database URL");
        let (mut client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
        tokio::spawn(connection);
        client
            .batch_execute(
                "DROP SCHEMA IF EXISTS receiving CASCADE; \
                 DROP SCHEMA IF EXISTS app_system CASCADE; \
                 DROP SCHEMA IF EXISTS catalog CASCADE; \
                 DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
                 DROP SCHEMA IF EXISTS wamn_history CASCADE; \
                 DO $roles$ BEGIN \
                   IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                     CREATE ROLE wamn_app NOLOGIN; \
                   END IF; \
                   IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                     CREATE ROLE wamn_scenario_author NOLOGIN; \
                   END IF; \
                 END $roles$;",
            )
            .await
            .unwrap();
        client
            .batch_execute(wamn_control_provision::sql::ensure_db_owner_role_sql())
            .await
            .unwrap();
        client
            .batch_execute(
                "DO $grant$ BEGIN \
                   EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); \
                 END $grant$;",
            )
            .await
            .unwrap();
        client
            .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
            .await
            .unwrap();
        client
            .batch_execute(include_str!("../../../deploy/sql/app-schema.sql"))
            .await
            .unwrap();
        let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/wamn_receiving");
        run(ApplyPackageArgs {
            package: shipped.clone(),
            database_url: url.clone(),
            tenant: TENANT.to_owned(),
        })
        .await
        .expect("apply the shipped package");

        let mut directory = read_package_directory(&shipped).unwrap();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&directory.manifest_bytes).unwrap();
        manifest["models"]
            .as_object_mut()
            .unwrap()
            .remove("receipt_line")
            .expect("the shipped package models receipt_line");
        directory.manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let root =
            std::env::temp_dir().join(format!("wamn-local-configuration-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("migrations")).unwrap();
        std::fs::write(root.join("wamn.json"), &directory.manifest_bytes).unwrap();
        for migration in &directory.migrations {
            std::fs::write(root.join(&migration.relative_path), &migration.bytes).unwrap();
        }
        let applied = run(ApplyPackageArgs {
            package: root.clone(),
            database_url: url.clone(),
            tenant: TENANT.to_owned(),
        })
        .await
        .expect_err("apply-package admitted a removed model");
        std::fs::remove_dir_all(&root).unwrap();
        assert!(
            applied
                .to_string()
                .starts_with(DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL),
            "apply-package refused the removed model for another reason: {applied:#}"
        );

        let tx = client.transaction().await.unwrap();
        tx.query_one(CLAIM_TENANT_SQL, &[&TENANT]).await.unwrap();
        let local = reconcile_local_package_configuration(&tx, TENANT, &directory)
            .await
            .expect_err("the local configuration path admitted a removed model");
        assert_eq!(format!("{local:#}"), format!("{applied:#}"));
    }
}

#[cfg(test)]
mod registration_tests;

//! Plan or apply image-owned SQL in one explicitly selected database.

use anyhow::{Context as _, ensure};
use tokio_postgres::{Client, GenericClient, Transaction};
use wamn_control_provision::schema_migrations::{self, MigrationScope};
use wamn_package_control::migrations::{self, MigrationHooks, MigrationResolution, MigrationTable};
use wamn_schema_control::migrations::MigrationPlan;
use wamn_schema_control::{
    MigrationOutcome, MigrationSource, RecordedMigration, SqlStatement, Value,
};

/// Project execution requires the existing environment lifecycle exclusion.
#[derive(Debug, Clone, Copy)]
pub enum MigrationTarget<'a> {
    System,
    Project(&'a crate::environment::lock::LifecycleLock),
}

/// Read-only adoption and file plan for the selected image.
#[derive(Debug)]
pub struct PlatformMigrationPlan {
    pub adoption: bool,
    pub migrations: MigrationPlan,
    pub recorded: Vec<RecordedMigration>,
}

fn scope_names(scope: MigrationScope) -> (&'static str, &'static str, &'static str) {
    match scope {
        MigrationScope::System => ("system", "registry", "migrations/system/"),
        MigrationScope::Project => ("project", "platform", "migrations/project/"),
    }
}

/// Read the embedded directory listing without a second source of file names.
pub fn embedded_sources(scope: MigrationScope) -> Vec<MigrationSource> {
    let files = match scope {
        MigrationScope::System => schema_migrations::SYSTEM_MIGRATIONS,
        MigrationScope::Project => schema_migrations::PROJECT_MIGRATIONS,
    };
    files
        .iter()
        .map(|(id, bytes)| MigrationSource {
            relative_path: (*id).into(),
            bytes: bytes.to_vec(),
        })
        .collect()
}

/// Compare initialization and history without creating a table or recording a file.
pub async fn plan(
    client: &impl GenericClient,
    scope: MigrationScope,
    sources: &[MigrationSource],
) -> anyhow::Result<PlatformMigrationPlan> {
    let (_, schema, prefix) = scope_names(scope);
    let candidate = match scope {
        MigrationScope::System => wamn_control_provision::schema_baseline::system_sha256(),
        MigrationScope::Project => wamn_control_provision::schema_baseline::project_sha256(),
    };
    let installed: String = client
        .query_one(&format!("SELECT sha256 FROM {schema}.schema_baseline"), &[])
        .await
        .context("read initialization identity before migration")?
        .get(0);
    ensure!(
        installed == candidate,
        "BaselineMismatch: installed={installed} candidate={candidate}"
    );
    let table = format!("{schema}.schema_migration");
    let exists: bool = client
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&table])
        .await?
        .get(0);
    let mut recorded = Vec::new();
    if exists {
        for row in client
            .query(
                &format!(
                    "SELECT ordinal, relative_path, sha256, outcome FROM {table} ORDER BY ordinal"
                ),
                &[],
            )
            .await?
        {
            let outcome: String = row.get(3);
            recorded.push(RecordedMigration {
                ordinal: u32::try_from(row.get::<_, i32>(0))?,
                relative_path: row.get(1),
                sha256: row.get(2),
                outcome: match outcome.as_str() {
                    "started" => MigrationOutcome::Started,
                    "applied" => MigrationOutcome::Applied,
                    _ => anyhow::bail!("unsupported schema migration outcome: {outcome}"),
                },
            });
        }
    }
    let migrations =
        wamn_schema_control::migrations::plan_migrations(sources, &recorded, prefix, None)?;
    for (file, sql) in migrations.pending.iter().zip(&migrations.statements) {
        wamn_schema_control::migrations::validate_execution_sql(file, sql)?;
    }
    Ok(PlatformMigrationPlan {
        adoption: !exists,
        migrations,
        recorded,
    })
}

/// Apply exact files without initialization, package authority, or release changes.
///
/// Use a dedicated connection. Close it on cancellation to release session locks.
/// Project scope shares the application/reset schema exclusion.
pub async fn apply(
    client: &mut Client,
    target: MigrationTarget<'_>,
    sources: &[MigrationSource],
    resolution: Option<&MigrationResolution>,
) -> anyhow::Result<usize> {
    let scope = match target {
        MigrationTarget::System => MigrationScope::System,
        MigrationTarget::Project(_) => MigrationScope::Project,
    };
    let lock_sql = match scope {
        MigrationScope::System => {
            "SELECT pg_advisory_xact_lock(hashtextextended('wamn.platform.system-schema:' || current_database(), 0))"
        }
        MigrationScope::Project => wamn_package_control::apply_package::LOCK_SQL,
    };
    let key = migrations::lock_session(client, lock_sql, &[]).await?;
    let result = apply_locked(client, scope, sources, resolution).await;
    let unlock = client
        .query_one("SELECT pg_advisory_unlock($1)", &[&key])
        .await;
    let count = result?;
    ensure!(
        unlock
            .context("release schema session lock")?
            .get::<_, bool>(0),
        "schema session lock disappeared"
    );
    Ok(count)
}

async fn apply_locked(
    client: &mut Client,
    scope: MigrationScope,
    sources: &[MigrationSource],
    resolution: Option<&MigrationResolution>,
) -> anyhow::Result<usize> {
    let planned = plan(client, scope, sources).await?;
    let (label, schema, _) = scope_names(scope);
    let table_name = format!("{schema}.schema_migration");
    let table = MigrationTable {
        name: &table_name,
        keys: &[],
        recorded: &planned.recorded,
        scope: migrations::ResolutionScope::Platform(label),
    };
    let mut statements = Vec::with_capacity(planned.migrations.pending.len() * 2);
    for (file, sql) in planned
        .migrations
        .pending
        .iter()
        .zip(planned.migrations.statements)
    {
        statements.push(sql);
        statements.push(SqlStatement {
            summary: format!("record {}", file.relative_path),
            sql: format!("INSERT INTO {table_name} (ordinal, relative_path, sha256, outcome, applied_at) VALUES ($1, $2, $3, $4, CASE WHEN $4 = 'applied' THEN now() END)"),
            params: vec![Value::Int(i32::try_from(file.ordinal)?), Value::Text(file.relative_path.clone()),
                Value::Text(file.sha256.clone()), Value::Text(if file.transactional { "applied" } else { "started" }.into())],
        });
    }
    let mut hooks = PlatformHooks {
        scope,
        adoption: planned.adoption,
    };
    let count = migrations::execute_migrations(
        client,
        &planned.migrations.pending,
        &statements,
        &table,
        resolution,
        &mut hooks,
    )
    .await?;
    Ok(count)
}

struct PlatformHooks {
    scope: MigrationScope,
    adoption: bool,
}

impl MigrationHooks for PlatformHooks {
    async fn begin<'a>(&mut self, client: &'a mut Client) -> anyhow::Result<Transaction<'a>> {
        crate::provision_project_env::provisioning_transaction(client).await
    }

    async fn initialize(&mut self, tx: &Transaction<'_>) -> anyhow::Result<()> {
        if self.adoption {
            tx.batch_execute(&schema_migrations::history_sql(self.scope))
                .await
                .context("adopt initialization with empty migration history")?;
        }
        Ok(())
    }

    async fn transactional(
        &mut self,
        tx: &Transaction<'_>,
        sql: &SqlStatement,
    ) -> anyhow::Result<()> {
        tx.batch_execute(&sql.sql)
            .await
            .with_context(|| sql.summary.clone())
    }

    async fn nontransactional(
        &mut self,
        client: &Client,
        file: &wamn_schema_control::PendingMigration,
        sql: &SqlStatement,
    ) -> anyhow::Result<()> {
        client.batch_execute(&sql.sql).await.with_context(|| format!(
            "migration {} has an uncertain outcome; inspect its effects and use --resolve {}=applied|retry", file.relative_path, file.relative_path))
    }
}

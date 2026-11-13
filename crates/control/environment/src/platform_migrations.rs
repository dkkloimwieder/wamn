//! Plan or apply image-owned SQL in one explicitly selected database.

use anyhow::{Context as _, ensure};
use sqlx::PgConnection as Client;
use sqlx::Row as _;

use wamn_control_provision::schema_migrations::{self, MigrationDatabase};
use wamn_package_control::migrations::{self, MigrationHooks, MigrationResolution, MigrationTable};
use wamn_schema_control::{MigrationSource, SqlStatement, Value};

/// Project execution requires the existing environment lifecycle exclusion.
#[derive(Debug, Clone, Copy)]
pub enum MigrationTarget<'a> {
    System,
    Project(&'a crate::environment::lock::LifecycleLock),
}

impl MigrationTarget<'_> {
    fn database(self) -> MigrationDatabase {
        match self {
            Self::System => MigrationDatabase::System,
            Self::Project(_) => MigrationDatabase::Project,
        }
    }
}

pub use wamn_package_control::schema_baseline::{
    SchemaPlan as PlatformMigrationPlan, embedded_sources, plan,
};

fn report(
    database: MigrationDatabase,
    planned: &PlatformMigrationPlan,
    sources: &[MigrationSource],
) -> String {
    format!(
        "migration history adoption: {}\n{}",
        if planned.adoption { "required" } else { "none" },
        wamn_package_control::schema_baseline::report(database, planned, sources)
    )
}

/// Describe known SQL consumers without imposing a compatibility condition.
async fn warnings(
    client: &mut sqlx::PgConnection,
    planned: &PlatformMigrationPlan,
    database: MigrationDatabase,
) -> anyhow::Result<Vec<String>> {
    use wamn_schema_introspection::migration_policy::{
        DefinitionAction, DefinitionType, inspect_migration_definition_mutations,
    };
    let consumers = match database {
        MigrationDatabase::System => "control and identity",
        MigrationDatabase::Project => "platform and application",
    };
    let namespaces = sqlx::query("SELECT nspname::text FROM pg_namespace")
        .fetch_all(&mut *client)
        .await?;
    let schemas = namespaces
        .iter()
        .map(|row| row.get::<&str, _>(0))
        .collect::<Vec<_>>();
    let mut warnings = Vec::new();
    for (file, sql) in planned
        .migrations
        .pending
        .iter()
        .zip(&planned.migrations.statements)
    {
        let Ok(mutations) = inspect_migration_definition_mutations(
            &file.relative_path,
            sql.sql.as_bytes(),
            &schemas,
        ) else {
            warnings.push(format!(
                "SQL reference analysis is unavailable for {}. Inspect its SQL.",
                file.relative_path
            ));
            continue;
        };
        for mutation in mutations {
            if mutation.action() != DefinitionAction::Drop {
                continue;
            }
            let relation = format!(
                "{}.{}",
                postgres_protocol::escape::escape_identifier(mutation.schema()),
                postgres_protocol::escape::escape_identifier(mutation.relation())
            );
            let column = (mutation.definition_type() == DefinitionType::Field)
                .then_some(mutation.definition());
            let (type_, member) = match mutation.definition_type() {
                DefinitionType::Relation => ("table", String::new()),
                DefinitionType::Field => (
                    "column",
                    format!(
                        ".{}",
                        postgres_protocol::escape::escape_identifier(mutation.definition())
                    ),
                ),
                DefinitionType::Constraint => (
                    "constraint",
                    format!(
                        ".{}",
                        postgres_protocol::escape::escape_identifier(mutation.definition())
                    ),
                ),
            };
            warnings.push(format!(
                "{} removes {type_} {relation}{member}. Inspect {consumers} consumers.",
                file.relative_path
            ));
            for row in sqlx::query(
                "SELECT DISTINCT pg_describe_object(classid, objid, objsubid) FROM pg_depend \
                 WHERE refclassid = 'pg_class'::regclass AND refobjid = to_regclass($1) \
                   AND ($2::text IS NULL OR refobjsubid = (SELECT attnum FROM pg_attribute \
                     WHERE attrelid = to_regclass($1) AND attname = $2)) ORDER BY 1",
            )
            .bind(&relation)
            .bind(column)
            .fetch_all(&mut *client)
            .await?
            {
                warnings.push(format!(
                    "known SQL reference to {relation}: {}",
                    row.get::<String, _>(0)
                ));
            }
        }
    }
    Ok(warnings)
}

/// Apply files and report committed progress if execution stops.
pub async fn apply_reported(
    client: &mut Client,
    target: MigrationTarget<'_>,
    sources: &[MigrationSource],
    resolution: Option<&MigrationResolution>,
) -> anyhow::Result<usize> {
    let result = apply(client, target, sources, resolution).await;
    report_result(client, target.database(), sources, result).await
}

async fn report_result(
    client: &mut Client,
    database: MigrationDatabase,
    sources: &[MigrationSource],
    result: anyhow::Result<usize>,
) -> anyhow::Result<usize> {
    match result {
        Ok(count) => Ok(count),
        Err(error) => {
            let report = match plan(client, database, sources).await {
                Ok(planned) => report(database, &planned, sources),
                Err(error) => format!("cannot read completed and remaining files: {error:#}\n"),
            };
            let selection = match database {
                MigrationDatabase::System => "system --chart <prepared-chart>",
                MigrationDatabase::Project => "project --env FILE --chart DIR",
            };
            Err(error.context(format!(
                "{report}next: wamn platform schema apply --target {selection}"
            )))
        }
    }
}

/// Finish only a newly initialized project database under provisioning's exclusion.
pub(crate) async fn initialize_project(
    client: &mut Client,
    sources: &[MigrationSource],
) -> anyhow::Result<usize> {
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut *client)
        .await
        .map(drop)?;
    let result = restore_project(client, sources).await;
    let reset = sqlx::raw_sql("RESET ROLE")
        .execute(&mut *client)
        .await
        .map(drop);
    let count = result?;
    reset?;
    Ok(count)
}

/// Restore validated project files while the caller holds reset or provisioning exclusion.
pub async fn restore_project(
    client: &mut Client,
    sources: &[MigrationSource],
) -> anyhow::Result<usize> {
    let result = apply_target(client, MigrationDatabase::Project, sources, None).await;
    report_result(client, MigrationDatabase::Project, sources, result).await
}

/// Inspect or execute one image's SQL using an already selected database and exclusion.
pub async fn run(
    client: &mut Client,
    target: MigrationTarget<'_>,
    sources: &[MigrationSource],
    apply: bool,
    resolution: Option<&MigrationResolution>,
    mut output: impl std::io::Write,
) -> anyhow::Result<()> {
    let database = target.database();
    let (label, _, _) = target_names(database);
    let consumers = match database {
        MigrationDatabase::System => "Control and identity",
        MigrationDatabase::Project => "Platform and application",
    };
    if !apply {
        sqlx::raw_sql("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *client)
            .await
            .map(drop)?;
    }
    let planned = plan(client, database, sources).await?;
    write!(output, "{}", report(database, &planned, sources))?;
    if !planned.migrations.pending.is_empty() {
        writeln!(
            output,
            "warning: selected image has pending {label} SQL. {consumers} code can require different SQL objects."
        )?;
        match warnings(client, &planned, database).await {
            Ok(warnings) => {
                for warning in warnings {
                    writeln!(output, "warning: {warning}")?;
                }
            }
            Err(error) => writeln!(
                output,
                "warning: SQL reference analysis is unavailable: {error:#}"
            )?,
        }
    }
    for (file, sql) in planned
        .migrations
        .pending
        .iter()
        .zip(&planned.migrations.statements)
    {
        writeln!(
            output,
            "SQL {} ({}):\n{}",
            file.relative_path,
            if file.transactional {
                "transactional"
            } else {
                "nontransactional"
            },
            sql.sql
        )?;
    }
    output.flush()?;
    if apply {
        apply_reported(client, target, sources, resolution).await?;
        let completed = plan(client, database, sources).await?;
        write!(output, "{}", report(database, &completed, sources))?;
    } else {
        sqlx::raw_sql("ROLLBACK")
            .execute(&mut *client)
            .await
            .map(drop)?;
    }
    output.flush()?;
    Ok(())
}

fn target_names(database: MigrationDatabase) -> (&'static str, &'static str, &'static str) {
    match database {
        MigrationDatabase::System => ("system", "registry", "migrations/system/"),
        MigrationDatabase::Project => ("project", "platform", "migrations/project/"),
    }
}

/// Apply exact files without initialization, package authority, or release changes.
///
/// Use a dedicated connection. Close it on cancellation to release session locks.
/// Project database shares the application/reset schema exclusion.
pub async fn apply(
    client: &mut Client,
    target: MigrationTarget<'_>,
    sources: &[MigrationSource],
    resolution: Option<&MigrationResolution>,
) -> anyhow::Result<usize> {
    apply_target(client, target.database(), sources, resolution).await
}

async fn apply_target(
    client: &mut Client,
    database: MigrationDatabase,
    sources: &[MigrationSource],
    resolution: Option<&MigrationResolution>,
) -> anyhow::Result<usize> {
    let lock_sql = match database {
        MigrationDatabase::System => {
            "SELECT pg_advisory_xact_lock(hashtextextended('wamn.platform.system-schema:' || current_database(), 0))"
        }
        MigrationDatabase::Project => wamn_package_control::apply_package::LOCK_SQL,
    };
    let key = migrations::lock_session(client, lock_sql, &[]).await?;
    let result = apply_locked(client, database, sources, resolution).await;
    let unlock = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .fetch_one(&mut *client)
        .await;
    let count = result?;
    ensure!(
        unlock
            .context("release schema session lock")?
            .get::<bool, _>(0),
        "schema session lock disappeared"
    );
    Ok(count)
}

async fn apply_locked(
    client: &mut Client,
    database: MigrationDatabase,
    sources: &[MigrationSource],
    resolution: Option<&MigrationResolution>,
) -> anyhow::Result<usize> {
    let planned = plan(client, database, sources).await?;
    if let Some(error) = planned.missing {
        return Err(error.into());
    }
    let (label, schema, _) = target_names(database);
    let table_name = format!("{schema}.schema_migration");
    let table = MigrationTable {
        name: &table_name,
        keys: &[],
        recorded: &planned.recorded,
        target: migrations::ResolutionTarget::Platform(label),
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
        database,
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
    database: MigrationDatabase,
    adoption: bool,
}

impl MigrationHooks for PlatformHooks {
    async fn begin<'a>(
        &mut self,
        client: &'a mut Client,
    ) -> anyhow::Result<sqlx::Transaction<'a, sqlx::Postgres>> {
        crate::provision_project_env::provisioning_transaction(client).await
    }

    async fn initialize(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> anyhow::Result<()> {
        if self.adoption {
            sqlx::raw_sql(sqlx::AssertSqlSafe(schema_migrations::history_sql(
                self.database,
            )))
            .execute(&mut **tx)
            .await
            .map(drop)
            .context("adopt initialization with empty migration history")?;
        }
        Ok(())
    }

    async fn transactional(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        sql: &SqlStatement,
    ) -> anyhow::Result<()> {
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql.sql.clone()))
            .execute(&mut **tx)
            .await
            .map(drop)
            .with_context(|| sql.summary.clone())
    }

    async fn nontransactional(
        &mut self,
        client: &mut Client,
        file: &wamn_schema_control::PendingMigration,
        sql: &SqlStatement,
    ) -> anyhow::Result<()> {
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql.sql.clone())).execute(&mut *client).await.map(drop).with_context(|| format!(
            "migration {} has an uncertain outcome; inspect its effects and use --resolve {}=applied|retry", file.relative_path, file.relative_path))
    }
}

/// Recorded substrate required by a disposable reset, independent of image additions.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ProjectReset {
    pub initialization_sha256: String,
    pub applied: Vec<wamn_package_control::schema_baseline::MigrationFile>,
}

impl ProjectReset {
    /// Refuse uncertain history before pinning or changing the serving database.
    pub async fn read(
        client: &mut sqlx::PgConnection,
        sources: &[MigrationSource],
    ) -> anyhow::Result<Self> {
        let planned = plan(client, MigrationDatabase::Project, sources).await?;
        if let Some(error) = planned.missing {
            return Err(error.into());
        }
        if let Some(file) = planned
            .recorded
            .iter()
            .find(|file| file.outcome == wamn_schema_control::MigrationOutcome::Started)
        {
            anyhow::bail!(
                "project migration {} is uncertain (started); inspect its effects and run wamn platform schema apply --target project --env FILE --chart DIR --resolve {}=applied|retry before reset",
                file.relative_path,
                file.relative_path
            );
        }
        Ok(Self {
            initialization_sha256: planned.initialization.sha256,
            applied: planned
                .recorded
                .into_iter()
                .map(
                    |file| wamn_package_control::schema_baseline::MigrationFile {
                        ordinal: file.ordinal,
                        relative_path: file.relative_path,
                        sha256: file.sha256,
                    },
                )
                .collect(),
        })
    }

    /// Validate the complete image against the pinned initialization and history.
    pub fn validate(&self, sources: &[MigrationSource]) -> anyhow::Result<()> {
        let candidate = wamn_control_provision::schema_baseline::project_sha256();
        ensure!(
            self.initialization_sha256 == candidate,
            "BaselineMismatch: installed={} candidate={candidate}",
            self.initialization_sha256
        );
        let recorded = self
            .applied
            .iter()
            .map(|file| wamn_schema_control::RecordedMigration {
                ordinal: file.ordinal,
                relative_path: file.relative_path.clone(),
                sha256: file.sha256.clone(),
                outcome: wamn_schema_control::MigrationOutcome::Applied,
            })
            .collect::<Vec<_>>();
        wamn_schema_control::migrations::plan_migrations(
            sources,
            &recorded,
            "migrations/project/",
            None,
        )?;
        Ok(())
    }

    /// Retain only applied SQL after validating the pinned identities.
    pub fn sources(
        &self,
        mut sources: Vec<MigrationSource>,
    ) -> anyhow::Result<Vec<MigrationSource>> {
        self.validate(&sources)?;
        sources.retain(|source| {
            self.applied
                .iter()
                .any(|file| file.relative_path == source.relative_path)
        });
        let replay = wamn_schema_control::migrations::plan_migrations(
            &sources,
            &[],
            "migrations/project/",
            None,
        )?;
        for (file, sql) in replay.pending.iter().zip(&replay.statements) {
            wamn_schema_control::migrations::validate_execution_sql(file, sql)?;
        }
        Ok(sources)
    }
}

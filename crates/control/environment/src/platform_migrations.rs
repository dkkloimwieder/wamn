//! Plan or apply image-owned SQL in one explicitly selected database.

use anyhow::{Context as _, ensure};
use tokio_postgres::{Client, GenericClient, Transaction};
use wamn_control_provision::schema_migrations::{self, MigrationScope};
use wamn_package_control::migrations::{self, MigrationHooks, MigrationResolution, MigrationTable};
use wamn_schema_control::{MigrationSource, SqlStatement, Value};

/// Project execution requires the existing environment lifecycle exclusion.
#[derive(Debug, Clone, Copy)]
pub enum MigrationTarget<'a> {
    System,
    Project(&'a crate::environment::lock::LifecycleLock),
}

impl MigrationTarget<'_> {
    fn scope(self) -> MigrationScope {
        match self {
            Self::System => MigrationScope::System,
            Self::Project(_) => MigrationScope::Project,
        }
    }
}

pub use wamn_package_control::schema_baseline::{
    SchemaPlan as PlatformMigrationPlan, embedded_sources, plan,
};

fn report(
    scope: MigrationScope,
    planned: &PlatformMigrationPlan,
    sources: &[MigrationSource],
) -> String {
    format!(
        "migration history adoption: {}\n{}",
        if planned.adoption { "required" } else { "none" },
        wamn_package_control::schema_baseline::report(scope, planned, sources)
    )
}

/// Describe known SQL consumers without imposing a compatibility gate.
async fn warnings(
    client: &impl GenericClient,
    planned: &PlatformMigrationPlan,
    scope: MigrationScope,
) -> anyhow::Result<Vec<String>> {
    use wamn_schema_introspection::migration_policy::{
        DefinitionAction, DefinitionType, inspect_migration_definition_mutations,
    };
    let consumers = match scope {
        MigrationScope::System => "control and identity",
        MigrationScope::Project => "platform and application",
    };
    let namespaces = client
        .query("SELECT nspname::text FROM pg_namespace", &[])
        .await?;
    let schemas = namespaces
        .iter()
        .map(|row| row.get::<_, &str>(0))
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
            let (kind, member) = match mutation.definition_type() {
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
                "{} removes {kind} {relation}{member}. Inspect {consumers} consumers.",
                file.relative_path
            ));
            for row in client
                .query(
                    "SELECT DISTINCT pg_describe_object(classid, objid, objsubid) FROM pg_depend \
                 WHERE refclassid = 'pg_class'::regclass AND refobjid = to_regclass($1) \
                   AND ($2::text IS NULL OR refobjsubid = (SELECT attnum FROM pg_attribute \
                     WHERE attrelid = to_regclass($1) AND attname = $2)) ORDER BY 1",
                    &[&relation, &column],
                )
                .await?
            {
                warnings.push(format!(
                    "known SQL reference to {relation}: {}",
                    row.get::<_, String>(0)
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
    report_result(client, target.scope(), sources, result).await
}

async fn report_result(
    client: &Client,
    scope: MigrationScope,
    sources: &[MigrationSource],
    result: anyhow::Result<usize>,
) -> anyhow::Result<usize> {
    match result {
        Ok(count) => Ok(count),
        Err(error) => {
            let report = match plan(client, scope, sources).await {
                Ok(planned) => report(scope, &planned, sources),
                Err(error) => format!("cannot read completed and remaining files: {error:#}\n"),
            };
            let selection = match scope {
                MigrationScope::System => "system --chart <prepared-chart>",
                MigrationScope::Project => "project --env FILE --chart DIR",
            };
            Err(error.context(format!(
                "{report}next: wamn platform schema apply --scope {selection}"
            )))
        }
    }
}

/// Finish only a newly initialized project database under provisioning's exclusion.
pub(crate) async fn initialize_project(
    client: &mut Client,
    sources: &[MigrationSource],
) -> anyhow::Result<usize> {
    client.batch_execute("SET ROLE wamn_db_owner").await?;
    let result = restore_project(client, sources).await;
    let reset = client.batch_execute("RESET ROLE").await;
    let count = result?;
    reset?;
    Ok(count)
}

/// Restore validated project files while the caller holds reset or provisioning exclusion.
pub async fn restore_project(
    client: &mut Client,
    sources: &[MigrationSource],
) -> anyhow::Result<usize> {
    let result = apply_scope(client, MigrationScope::Project, sources, None).await;
    report_result(client, MigrationScope::Project, sources, result).await
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
    let scope = target.scope();
    let (label, _, _) = scope_names(scope);
    let consumers = match scope {
        MigrationScope::System => "Control and identity",
        MigrationScope::Project => "Platform and application",
    };
    if !apply {
        client
            .batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .await?;
    }
    let planned = plan(client, scope, sources).await?;
    write!(output, "{}", report(scope, &planned, sources))?;
    if !planned.migrations.pending.is_empty() {
        writeln!(
            output,
            "warning: selected image has pending {label} SQL. {consumers} code can require different SQL objects."
        )?;
        match warnings(client, &planned, scope).await {
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
        let completed = plan(client, scope, sources).await?;
        write!(output, "{}", report(scope, &completed, sources))?;
    } else {
        client.batch_execute("ROLLBACK").await?;
    }
    output.flush()?;
    Ok(())
}

fn scope_names(scope: MigrationScope) -> (&'static str, &'static str, &'static str) {
    match scope {
        MigrationScope::System => ("system", "registry", "migrations/system/"),
        MigrationScope::Project => ("project", "platform", "migrations/project/"),
    }
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
    apply_scope(client, target.scope(), sources, resolution).await
}

async fn apply_scope(
    client: &mut Client,
    scope: MigrationScope,
    sources: &[MigrationSource],
    resolution: Option<&MigrationResolution>,
) -> anyhow::Result<usize> {
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
    if let Some(error) = planned.missing {
        return Err(error.into());
    }
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

/// Recorded substrate required by a disposable reset, independent of image additions.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ProjectReset {
    pub initialization_sha256: String,
    pub applied: Vec<wamn_package_control::schema_baseline::MigrationFile>,
}

impl ProjectReset {
    /// Refuse uncertain history before pinning or changing the serving database.
    pub async fn read(
        client: &impl GenericClient,
        sources: &[MigrationSource],
    ) -> anyhow::Result<Self> {
        let planned = plan(client, MigrationScope::Project, sources).await?;
        if let Some(error) = planned.missing {
            return Err(error.into());
        }
        if let Some(file) = planned
            .recorded
            .iter()
            .find(|file| file.outcome == wamn_schema_control::MigrationOutcome::Started)
        {
            anyhow::bail!(
                "project migration {} is uncertain (started); inspect its effects and run wamn platform schema apply --scope project --env FILE --chart DIR --resolve {}=applied|retry before reset",
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

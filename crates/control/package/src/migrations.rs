//! Shared SQL file execution, resolution, and session exclusion.

use anyhow::{Context as _, ensure};
use sqlx::PgConnection as Client;
use sqlx::Row as _;

use wamn_schema_control::{PendingMigration, RecordedMigration, SqlStatement};

/// The developer's explicit decision about one uncertain file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationResolution {
    /// Package ID, or the platform target name for a platform operation.
    pub package: String,
    pub path: String,
    pub action: Resolution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Applied,
    Retry,
}

impl std::fmt::Display for MigrationResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}={}",
            self.package,
            self.path,
            match self.action {
                Resolution::Applied => "applied",
                Resolution::Retry => "retry",
            }
        )
    }
}

impl std::str::FromStr for MigrationResolution {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (path, action) = value
            .split_once('=')
            .context("resolution must be <package>:<relative_path>=applied|retry")?;
        let (package, path) = path
            .split_once(':')
            .context("resolution requires a package and migration path")?;
        ensure!(
            !package.is_empty() && !path.is_empty(),
            "resolution requires a package and migration path"
        );
        let action = match action {
            "applied" => Resolution::Applied,
            "retry" => Resolution::Retry,
            _ => anyhow::bail!("resolution must be <package>:<relative_path>=applied|retry"),
        };
        Ok(Self {
            package: package.into(),
            path: path.into(),
            action,
        })
    }
}

/// Route a path to exactly one selected package before applying any SQL.
/// A repeated resolution for an applied file is harmless after a lost result.
pub fn resolution_package<'a>(
    resolution: Option<&MigrationResolution>,
    packages: impl IntoIterator<Item = (&'a str, &'a [RecordedMigration])>,
) -> anyhow::Result<Option<&'a str>> {
    let mut matched = None;
    let mut recorded = false;
    for (id, files) in packages {
        for file in files {
            if file.outcome == wamn_schema_control::MigrationOutcome::Started
                && resolution.is_none()
            {
                return Err(uncertain_migration(
                    ResolutionTarget::Package(id),
                    &file.relative_path,
                ));
            }
            if resolution
                .is_some_and(|choice| choice.package == id && choice.path == file.relative_path)
            {
                recorded = true;
                matched = Some(id);
            }
        }
    }
    if let Some(resolution) = resolution {
        ensure!(
            recorded,
            "resolution names no recorded migration: {}",
            resolution.path
        );
    }
    Ok(matched)
}

/// Record table, immutable keys, and observed prefix supplied by its owning target.
#[derive(Debug)]
pub struct MigrationTable<'a> {
    pub name: &'a str,
    pub keys: &'a [(&'a str, &'a str)],
    pub recorded: &'a [RecordedMigration],
    pub target: ResolutionTarget<'a>,
}

impl MigrationTable<'_> {
    async fn complete(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        path: &str,
    ) -> anyhow::Result<()> {
        let predicates = self
            .keys
            .iter()
            .enumerate()
            .map(|(index, (column, _))| {
                format!(
                    "{} = ${}",
                    postgres_protocol::escape::escape_identifier(column),
                    index + 1
                )
            })
            .collect::<Vec<_>>();
        let conjunction = if predicates.is_empty() {
            String::new()
        } else {
            format!("{} AND ", predicates.join(" AND "))
        };
        let sql = format!(
            "UPDATE {} SET outcome = 'applied', applied_at = now() WHERE {conjunction}relative_path = ${} AND outcome = 'started'",
            self.name,
            self.keys.len() + 1
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
        for (_, value) in self.keys {
            query = query.bind(value);
        }
        let updated = query
            .bind(path)
            .execute(&mut **tx)
            .await
            .context("record nontransactional completion")?
            .rows_affected();
        ensure!(
            updated == 1,
            "nontransactional start record disappeared: {path}"
        );
        Ok(())
    }
}

/// Target-specific authority and bookkeeping around shared file execution.
pub trait MigrationHooks: Send {
    fn begin<'a>(
        &mut self,
        client: &'a mut Client,
    ) -> impl std::future::Future<Output = anyhow::Result<sqlx::Transaction<'a, sqlx::Postgres>>> + Send;
    fn initialize(
        &mut self,
        _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send {
        async { Ok(()) }
    }
    fn before(
        &mut self,
        _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        _file: &PendingMigration,
        _accept: bool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send {
        async { Ok(()) }
    }
    fn transactional(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        sql: &SqlStatement,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
    fn nontransactional(
        &mut self,
        client: &mut Client,
        file: &PendingMigration,
        sql: &SqlStatement,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
    fn after(
        &mut self,
        _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        _file: &PendingMigration,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send {
        async { Ok(()) }
    }
    fn finish(
        &mut self,
        _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send {
        async { Ok(()) }
    }
}

/// Diagnostic identity without package authority in platform targets.
#[derive(Debug, Clone, Copy)]
pub enum ResolutionTarget<'a> {
    Package(&'a str),
    Platform(&'a str),
}

fn uncertain_migration(target: ResolutionTarget<'_>, path: &str) -> anyhow::Error {
    match target {
        ResolutionTarget::Package(id) => anyhow::anyhow!(
            "package {id}: migration {path} has an uncertain outcome; inspect its effects and use --resolve {id}:{path}=applied|retry"
        ),
        ResolutionTarget::Platform(name) => anyhow::anyhow!(
            "{name} schema: migration {path} has an uncertain outcome; inspect its effects and use --resolve {path}=applied|retry"
        ),
    }
}

/// Resolve uncertain files before any mutation, including history-table adoption.
pub fn validate_resolution(
    target: ResolutionTarget<'_>,
    pending: &[PendingMigration],
    recorded: &[RecordedMigration],
    resolution: Option<&MigrationResolution>,
) -> anyhow::Result<()> {
    let label = match target {
        ResolutionTarget::Package(id) | ResolutionTarget::Platform(id) => id,
    };
    if let Some(choice) = resolution {
        ensure!(
            choice.package == label
                && recorded
                    .iter()
                    .any(|file| file.relative_path == choice.path),
            "resolution names no recorded migration: {}",
            choice.path
        );
    }
    for file in pending.iter().filter(|file| file.started) {
        if !resolution
            .is_some_and(|choice| choice.package == label && choice.path == file.relative_path)
        {
            return Err(uncertain_migration(target, &file.relative_path));
        }
    }
    Ok(())
}

/// Execute a validated plan while the caller holds its session exclusion.
///
/// Run final bookkeeping in the last transaction. Each pair
/// contains the exact file SQL and its target-specific insertion statement.
/// The caller must use a dedicated session and close it on cancellation.
pub async fn execute_migrations(
    client: &mut Client,
    pending: &[PendingMigration],
    statements: &[SqlStatement],
    table: &MigrationTable<'_>,
    resolution: Option<&MigrationResolution>,
    hooks: &mut impl MigrationHooks,
) -> anyhow::Result<usize> {
    ensure!(
        statements.len() == pending.len() * 2,
        "migration plan has unmatched statements"
    );
    validate_resolution(table.target, pending, table.recorded, resolution)?;
    for (file, [sql, _]) in pending.iter().zip(statements.as_chunks::<2>().0) {
        wamn_schema_control::migrations::validate_execution_sql(file, sql)?;
    }
    let mut tx = hooks.begin(client).await?;
    hooks.initialize(&mut tx).await?;
    let mut executed = 0;
    for (file, [sql, record]) in pending.iter().zip(statements.as_chunks::<2>().0) {
        let accept = file.started
            && resolution.is_some_and(|choice| {
                choice.path == file.relative_path && choice.action == Resolution::Applied
            });
        hooks.before(&mut tx, file, accept).await?;
        if file.transactional {
            hooks.transactional(&mut tx, sql).await?;
            execute(&mut tx, record).await?;
            executed += 1;
        } else {
            tx.commit().await.context("commit migration batch")?;
            tx = hooks.begin(client).await?;
            if !file.started {
                execute(&mut tx, record).await?;
            }
            tx.commit().await.context("commit nontransactional start")?;
            if !accept {
                hooks.nontransactional(client, file, sql).await?;
                executed += 1;
            }
            tx = hooks.begin(client).await?;
            table.complete(&mut tx, &file.relative_path).await?;
        }
        hooks.after(&mut tx, file).await?;
        tx.commit().await.context("commit migration completion")?;
        tx = hooks.begin(client).await?;
    }
    hooks.finish(&mut tx).await?;
    tx.commit()
        .await
        .context("commit completed schema operation")?;
    Ok(executed)
}

async fn execute(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    statement: &SqlStatement,
) -> anyhow::Result<()> {
    let result = if statement.params.is_empty() {
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement.sql.clone()))
            .execute(&mut **tx)
            .await
            .map(drop)
    } else {
        crate::sql_params::bind(
            sqlx::query(sqlx::AssertSqlSafe(statement.sql.clone())),
            &statement.params,
        )
        .execute(&mut **tx)
        .await
        .map(drop)
    };
    result.with_context(|| format!("apply {}", statement.summary))
}

/// Acquire the existing lock key without retaining a snapshot while waiting.
/// A blocking advisory-lock query can deadlock with CREATE INDEX CONCURRENTLY,
/// which waits for older snapshots. Each attempt finishes before the client sleeps.
pub async fn lock_session(
    client: &mut Client,
    transaction_lock_sql: &str,
    parameters: &[&str],
) -> anyhow::Result<i64> {
    let mut query = sqlx::query(sqlx::AssertSqlSafe(
        transaction_lock_sql.replace("pg_advisory_xact_lock", ""),
    ));
    for value in parameters {
        query = query.bind(value);
    }
    let key: i64 = query
        .fetch_one(&mut *client)
        .await
        .context("read package schema lock key")?
        .get(0);
    let mut delay = std::time::Duration::from_millis(25);
    let mut reported_holders = None;
    loop {
        if sqlx::query("SELECT pg_try_advisory_lock($1)")
            .bind(key)
            .fetch_one(&mut *client)
            .await
            .context("try package schema session lock")?
            .get::<bool, _>(0)
        {
            return Ok(key);
        }
        let holders = sqlx::query(
            "SELECT pid FROM pg_locks WHERE locktype='advisory' AND granted \
             AND database=(SELECT oid FROM pg_database WHERE datname=current_database()) \
             AND classid::bigint=(($1::bigint >> 32) & 4294967295) \
             AND objid::bigint=($1::bigint & 4294967295) AND objsubid=1 ORDER BY pid",
        )
        .bind(key)
        .fetch_all(&mut *client)
        .await
        .context("read package schema lock holders")?
        .iter()
        .map(|row| row.get::<Option<i32>, _>(0))
        .collect::<Vec<_>>();
        if reported_holders.as_ref() != Some(&holders) {
            eprintln!("waiting for package schema lock {key}; PostgreSQL holders: {holders:?}");
            reported_holders = Some(holders);
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(std::time::Duration::from_millis(250));
    }
}

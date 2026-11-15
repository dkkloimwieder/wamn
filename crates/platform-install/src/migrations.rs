//! Session exclusion and SQLx migration execution.

use anyhow::Context as _;
use sqlx::PgConnection as Client;
use sqlx::Row as _;

/// Apply one SQLx sequence to its own history table.
/// The caller owns a dedicated session and holds schema exclusion.
pub async fn run(
    client: &mut Client,
    mut migrator: sqlx::migrate::Migrator,
    table: String,
) -> anyhow::Result<usize> {
    migrator.dangerous_set_table_name(table.clone());
    migrator.set_locking(false);
    let query = format!("SELECT count(*) FROM {table}");
    let before: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(query.clone()))
        .fetch_one(&mut *client)
        .await?;
    migrator.run_direct(None, client, false).await?;
    let after: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(client)
        .await?;
    Ok(usize::try_from(after - before)?)
}

/// Wait without holding relation locks, including across nontransactional SQL.
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

/// A migration version and its SQLx checksum.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MigrationFile {
    pub version: i64,
    pub checksum: String,
}

/// SQLx history compared with the selected immutable migration files.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SchemaReport {
    pub installed: Vec<MigrationFile>,
    pub pending: Vec<MigrationFile>,
    pub missing: Vec<MigrationFile>,
    pub warnings: Vec<String>,
}

/// Read applied migrations without creating a history table.
pub async fn observe(
    client: &mut Client,
    table: &str,
    sources: &[sqlx::migrate::Migration],
) -> anyhow::Result<SchemaReport> {
    use sqlx::migrate::Migrate as _;
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(table)
        .fetch_one(&mut *client)
        .await?;
    let applied = if exists {
        client.list_applied_migrations(table).await?
    } else {
        Vec::new()
    };
    let mut report = SchemaReport::default();
    for row in &applied {
        let file = MigrationFile {
            version: row.version,
            checksum: hex::encode(&row.checksum),
        };
        if !sources
            .iter()
            .any(|source| source.migration_type.is_up_migration() && source.version == row.version)
        {
            report.missing.push(file.clone());
        }
        report.installed.push(file);
    }
    for source in sources
        .iter()
        .filter(|source| source.migration_type.is_up_migration())
    {
        match applied.iter().find(|row| row.version == source.version) {
            None => report.pending.push(MigrationFile {
                version: source.version,
                checksum: hex::encode(&source.checksum),
            }),
            Some(row) if row.checksum != source.checksum => report.warnings.push(format!(
                "SQLx migration {} checksum differs",
                source.version
            )),
            Some(_) => {}
        }
    }
    if exists && let Some(version) = client.dirty_version(table).await? {
        report
            .warnings
            .push(format!("SQLx migration {version} is incomplete"));
    }
    Ok(report)
}

impl std::fmt::Display for SchemaReport {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (label, files) in [
            ("installed", &self.installed),
            ("pending", &self.pending),
            ("missing", &self.missing),
        ] {
            writeln!(output, "{label} migrations:")?;
            for file in files {
                writeln!(output, "  {} {}", file.version, file.checksum)?;
            }
        }
        for warning in &self.warnings {
            writeln!(output, "warning: {warning}")?;
        }
        Ok(())
    }
}

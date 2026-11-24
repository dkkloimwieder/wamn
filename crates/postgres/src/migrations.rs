//! Session exclusion and SQLx migration execution.

use crate::migration_scope::MigrationDatabase;
use anyhow::{Context as _, ensure};
use sqlx::PgConnection as Client;
use sqlx::Row as _;
use sqlx::migrate::{Migration, Migrator};
use wamn_principal::PlatformComponent;

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

/// Return the embedded migration files for the selected database.
pub fn embedded_sources(target: MigrationDatabase) -> Vec<Migration> {
    crate::migration_scope::embedded(target)
        .iter()
        .cloned()
        .collect()
}

pub async fn apply_target(
    client: &mut sqlx::PgConnection,
    database: MigrationDatabase,
    sources: &[Migration],
) -> anyhow::Result<usize> {
    let (schema, lock) = match database {
        MigrationDatabase::System => (
            "registry",
            "SELECT pg_advisory_xact_lock(hashtextextended('wamn.platform.system-schema:' || current_database(), 0))",
        ),
        MigrationDatabase::Project => (
            "platform",
            "SELECT pg_advisory_xact_lock(hashtextextended('wamn.package.data-access:' || current_database(), 0))",
        ),
    };
    let login = database.require_login(client).await?;
    let owner = postgres_protocol::escape::escape_identifier(&database.owner_role());
    sqlx::query("INSERT INTO platform.login_principal(login,principal_id) VALUES($1,$2) ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id")
        .bind(login.as_str())
        .bind(sqlx::types::Uuid::from(PlatformComponent::ApplyPackage.principal_id()))
        .execute(&mut *client).await?;
    let key = lock_session(client, lock, &[]).await?;
    let result = async {
        let table = format!("{schema}._sqlx_migrations");
        prepare_history(client, &table, &login).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE {table} OWNER TO {owner}"
        )))
        .execute(&mut *client)
        .await?;
        let migrator = Migrator::with_migrations(sources.to_vec());
        run(client, migrator, table).await
    }
    .await;
    // Connections are dedicated. A cancellation closes them; a completed call releases exclusion.
    let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .fetch_one(client)
        .await?;
    ensure!(unlocked, "platform schema session lock disappeared");
    result
}

/// Create SQLx's history with platform-owned audit and immutable migration identity.
/// Only SQLx's elapsed-time update remains writable after insertion.
pub async fn prepare_history(
    connection: &mut sqlx::PgConnection,
    table: &str,
    login: &wamn_role::LoginName,
) -> anyhow::Result<()> {
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(table)
        .fetch_one(&mut *connection)
        .await?;
    if !exists {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TABLE {table} (
            version BIGINT PRIMARY KEY,
            description TEXT NOT NULL,
            installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),
            success BOOLEAN NOT NULL,
            checksum BYTEA NOT NULL,
            execution_time BIGINT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL,
            created_by UUID NOT NULL,
            updated_at TIMESTAMPTZ NOT NULL,
            updated_by UUID NOT NULL
        );
        CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON {table}
            FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row('created_at','created_by','updated_at','updated_by');
        CREATE TRIGGER migration_immutable BEFORE UPDATE OR DELETE ON {table}
            FOR EACH ROW EXECUTE FUNCTION wamn_history.guard_sqlx_migration();")))
            .execute(&mut *connection).await?;
    }
    let role = postgres_protocol::escape::escape_identifier(login.as_str());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT, INSERT, UPDATE (execution_time) ON {table} TO {role}"
    )))
    .execute(connection)
    .await?;
    Ok(())
}

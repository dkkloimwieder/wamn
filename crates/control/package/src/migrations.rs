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
    migrator.run(&mut *client).await?;
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

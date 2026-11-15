//! Caller identity for native reads of protected platform records.

/// Read a protected platform record under its caller on a pooled connection.
pub async fn read_pool(
    pool: &sqlx::PgPool,
    principal: &str,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, sqlx::Error> {
    read(
        pool.begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ")
            .await?,
        principal,
    )
    .await
}

/// Read a protected platform record under its caller on a dedicated connection.
pub async fn read_connection<'connection>(
    connection: &'connection mut sqlx::PgConnection,
    principal: &str,
) -> Result<sqlx::Transaction<'connection, sqlx::Postgres>, sqlx::Error> {
    use sqlx::Connection as _;
    read(
        connection
            .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ")
            .await?,
        principal,
    )
    .await
}

async fn read<'connection>(
    mut transaction: sqlx::Transaction<'connection, sqlx::Postgres>,
    principal: &str,
) -> Result<sqlx::Transaction<'connection, sqlx::Postgres>, sqlx::Error> {
    sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1::text::uuid,'','','admin:authority-read')")
        .bind(principal).execute(&mut *transaction).await?;
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *transaction)
        .await?;
    Ok(transaction)
}

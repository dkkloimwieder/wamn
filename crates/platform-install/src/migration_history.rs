//! Registry migration history during system installation.

pub(crate) async fn history_exists(
    connection: &mut sqlx::PgConnection,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT to_regclass('registry._sqlx_migrations') IS NOT NULL")
        .fetch_one(connection)
        .await
}

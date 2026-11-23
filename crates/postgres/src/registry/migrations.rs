use wamn_registry::migrations::MigrationHistory;

/// Migration history table in the registry schema.
pub const REGISTRY_HISTORY_TABLE: &str = "registry._sqlx_migrations";

impl MigrationHistory for super::Registry<'_> {
    type Error = sqlx::Error;

    async fn history_exists(&mut self) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar("SELECT to_regclass('registry._sqlx_migrations') IS NOT NULL")
            .fetch_one(&mut *self.connection)
            .await
    }
}

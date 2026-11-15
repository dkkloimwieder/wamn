//! SELECT-only file reads and a session lock for one native collector.

use wamn_registry::identifiers::{valid_project, valid_project_env_key};
use wamn_sql::WamnPostgres;

/// A dedicated reader releases its advisory lock when its connection closes.
#[derive(Debug)]
pub struct FileCollection {
    connection: sqlx::PgConnection,
}

impl FileCollection {
    /// Acquire the environment's collector lease with its application login.
    pub async fn acquire(
        postgres: &WamnPostgres,
        project: &str,
        project_env_key: &str,
    ) -> anyhow::Result<Option<FileCollection>> {
        anyhow::ensure!(
            valid_project(project) && valid_project_env_key(project_env_key),
            "invalid file collector scope"
        );
        let connection = postgres
            .application_connection(project, project_env_key)
            .await?;
        let mut reader = FileCollection {
            connection: connection.detach(),
        };
        let locked: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_lock(hashtextextended('wamn:file:collector', 0))",
        )
        .fetch_one(&mut reader.connection)
        .await?;
        Ok(locked.then_some(reader))
    }
}

impl FileCollection {
    /// Read at most 100 exact keys and their connection identities.
    pub async fn rows(&mut self, keys: &[String]) -> anyhow::Result<Vec<(String, String)>> {
        anyhow::ensure!(keys.len() <= 100, "file collector batch exceeds 100 keys");
        Ok(sqlx::query_as(
            "SELECT connection_id::text, blob_key FROM platform.file WHERE blob_key = ANY($1)",
        )
        .bind(keys)
        .fetch_all(&mut self.connection)
        .await?)
    }
}

//! SELECT-only file reads and a session lock for one native collector.

use super::WamnPostgres;
use wamn_control_registry::identifiers::{valid_env_key, valid_project};

/// A dedicated reader that releases its advisory lock on completion or cancellation.
/// Dropping the detached connection closes it instead of returning a lock to the pool.
#[derive(Debug)]
pub struct FileCollection {
    connection: deadpool_postgres::ClientWrapper,
}

impl WamnPostgres {
    /// Read an active store connection from the environment projection.
    pub async fn file_connection_id(
        &self,
        project: &str,
        env_key: &str,
        name: &str,
    ) -> anyhow::Result<Option<String>> {
        let (client, _) = self
            .checkout_guest(project, env_key)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(client.query_opt("SELECT c.id::text FROM platform.connection c JOIN platform.integration i ON i.id=c.integration_id WHERE i.name=$1 AND i.status='active' AND c.status='active' AND c.integration_type IN ('gcs','s3','disk')", &[&name]).await?.map(|row| row.get(0)))
    }

    /// Acquire the environment's collector lease with the existing guest SELECT login.
    /// A second host skips the pass while another collector holds the session lock.
    pub async fn file_collection(
        &self,
        project: &str,
        env_key: &str,
    ) -> anyhow::Result<Option<FileCollection>> {
        anyhow::ensure!(
            valid_project(project) && valid_env_key(env_key),
            "invalid file collector scope"
        );
        let (connection, _) = self
            .checkout_guest(project, env_key)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let reader = FileCollection {
            connection: deadpool_postgres::Object::take(connection),
        };
        let locked: bool = reader
            .connection
            .query_one(
                "SELECT pg_try_advisory_lock(hashtextextended('wamn:file:collector', 0))",
                &[],
            )
            .await?
            .try_get(0)?;
        Ok(locked.then_some(reader))
    }
}

impl FileCollection {
    /// Read at most 100 exact keys and their connection identities, without a transaction.
    pub async fn rows(&self, keys: &[String]) -> anyhow::Result<Vec<(String, String)>> {
        anyhow::ensure!(keys.len() <= 100, "file collector batch exceeds 100 keys");
        self.connection
            .query(
                "SELECT connection_id::text, blob_key FROM platform.file WHERE blob_key = ANY($1)",
                &[&keys],
            )
            .await?
            .into_iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?)))
            .collect()
    }
}

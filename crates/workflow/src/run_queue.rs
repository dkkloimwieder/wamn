//! Restart pins and release bytes for the workflow queue.

use wamn_postgres_capability::WamnPostgres;
use wamn_run_state::persistence;

/// Read restart pins through this project's executor credential.
pub async fn unfinished_releases(
    postgres: &WamnPostgres,
    project: &str,
    project_env_key: &str,
    environment: &str,
) -> anyhow::Result<Vec<(String, wamn_digest::Digest, String)>> {
    postgres
        .executor_read(project, project_env_key, async |connection| {
            Ok(persistence::unfinished_releases(connection, project_env_key, environment).await?)
        })
        .await
}

/// Load immutable catalog bytes through the owning project's executor credential.
pub async fn release_bytes(
    postgres: &WamnPostgres,
    project: &str,
    project_env_key: &str,
    digest: &wamn_digest::Digest,
) -> anyhow::Result<Vec<u8>> {
    postgres.executor_read(project, project_env_key, async |connection| {
            Ok(sqlx::query_scalar("SELECT canonical_bytes FROM catalog.releases WHERE project_env_key=$1 AND manifest_digest=$2")
                .bind(project_env_key).bind(digest).fetch_one(connection).await?)
        }).await
}

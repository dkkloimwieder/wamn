//! Restart pins and release bytes for the workflow queue.

use wamn_sql::WamnPostgres;

/// Load immutable catalog bytes through the owning project's executor credential.
pub async fn release_bytes(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
    project_env_key: &wamn_registry::ProjectEnvKey,
    digest: &wamn_digest::Digest,
) -> anyhow::Result<Vec<u8>> {
    postgres.executor_read(project, async |connection| {
            Ok(sqlx::query_scalar("SELECT canonical_bytes FROM catalog.releases WHERE project_env_key=$1 AND manifest_digest=$2")
                .bind(project_env_key).bind(digest).fetch_one(connection).await?)
        }).await
}

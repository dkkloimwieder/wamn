//! Authorized reads of release connection facts.

use sqlx::Row as _;

use anyhow::Context as _;

use super::WamnPostgres;

/// Make sure that every component-grain requirement in the synchronous release
/// closure has one exact usable environment binding.
pub(crate) const RELEASE_COMPONENT_BINDINGS_READY_SQL: &str = "\
SELECT NOT EXISTS ( \
    SELECT FROM catalog.connection_requirement requirement \
     WHERE requirement.project_env_key=$1 AND requirement.component_digest=ANY($3::text[]) \
       AND NOT EXISTS ( \
        SELECT FROM catalog.connection_binding binding \
        JOIN catalog.release_connection selected USING(project_env_key,manifest_digest,connection_id) \
        JOIN platform.connection connection ON connection.id=selected.connection_id \
        JOIN platform.integration integration ON integration.id=connection.integration_id \
        WHERE binding.project_env_key=requirement.project_env_key AND binding.manifest_digest=$2 \
          AND binding.component_digest=requirement.component_digest AND binding.package_id=requirement.package_id \
          AND binding.name=requirement.name AND connection.status='active' AND integration.status='active' \
       ) \
)";

impl WamnPostgres {
    /// Read the interfaces declared by the exact package and component.
    pub async fn declared_connection_interfaces(
        &self,
        project: &str,
        project_env_key: &str,
        package: &str,
        digest: &str,
    ) -> anyhow::Result<Vec<(String, String)>> {
        self.executor_read(project, project_env_key, async |connection| {
        Ok(sqlx::query("SELECT name,interface FROM catalog.connection_requirement WHERE project_env_key=$1 AND package_id=$2 AND component_digest=$3").bind(project_env_key).bind(package).bind(digest).fetch_all(&mut *connection).await?.into_iter().map(|row| (row.get(0),row.get(1))).collect())
        }).await
    }

    /// Read the connection map of release `digest` for the connection
    /// memory, whatever the status of each connection.
    pub async fn release_fact(
        &self,
        project: &str,
        project_env_key: &str,
        digest: &str,
    ) -> anyhow::Result<crate::connection_memory::ReleaseFact> {
        self.executor_read(project, project_env_key, async |connection| {
        let rows = sqlx::query("SELECT b.component_digest,b.package_id,b.name,b.connection_id,r.version FROM catalog.connection_binding b JOIN catalog.release_connection r USING(project_env_key,manifest_digest,connection_id) WHERE b.project_env_key=$1 AND b.manifest_digest=$2 ORDER BY 1,2,3").bind(project_env_key).bind(digest).fetch_all(&mut *connection).await.context("read the release connection map")?;
        let versions = sqlx::query("SELECT connection_id,version FROM catalog.release_connection WHERE project_env_key=$1 AND manifest_digest=$2").bind(project_env_key).bind(digest).fetch_all(&mut *connection).await.context("read the release connection versions")?;
        Ok(crate::connection_memory::ReleaseFact {
            versions: versions
                .into_iter()
                .map(|row| (row.get(0), row.get(1)))
                .collect(),
            bindings: rows
                .into_iter()
                .map(|row| crate::connection_memory::ReleaseBinding {
                    component_digest: row.get(0),
                    package_id: row.get(1),
                    name: row.get(2),
                    connection_id: row.get(3),
                    version: row.get(4),
                })
                .collect(),
        })
        }).await
    }

    /// Make sure of the exact component requirements selected by request readiness.
    ///
    /// An empty digest set is a background-only release and performs no store
    /// call. Otherwise the test shares the driver's existing platform pool and
    /// project identity; missing rows, unavailable storage and malformed results are
    /// errors, while an ordinary unbound requirement is `Ok(false)`.
    pub async fn release_component_bindings_ready(
        &self,
        project: &str,
        project_env_key: &str,
        manifest_digest: &str,
        environment: &str,
        component_digests: &[String],
    ) -> anyhow::Result<bool> {
        if component_digests.is_empty() {
            return Ok(true);
        }
        self.executor_read(project, project_env_key, async |connection| {
            if let Some(local) = &self.local_application {
                anyhow::ensure!(
                    local.target.project_env_key == project_env_key
                        && local.target.environment == environment
                        && local.facts.manifest_digest.as_str() == manifest_digest,
                    "local readiness release target mismatch"
                );
                return local.bindings_ready(connection, component_digests).await;
            }
            sqlx::query(RELEASE_COMPONENT_BINDINGS_READY_SQL)
                .bind(project_env_key)
                .bind(manifest_digest)
                .bind(component_digests)
                .fetch_one(connection)
                .await
                .context("query release connection bindings")?
                .try_get(0)
                .context("decode release binding readiness")
        })
        .await
    }
}

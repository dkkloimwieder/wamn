//! Remove retired values after the replacement generation ages five minutes.

use sqlx::Row as _;
use wamn_kubernetes::Store as _;
/// Collect retired values without changing the immutable credential metadata.
pub async fn collect(client: &mut sqlx::PgConnection, org: &str) -> anyhow::Result<()> {
    let rows = sqlx::query(wamn_registry::sql::EXECUTION_HOST_SRC_CREDENTIAL_CLEANUP_COLLECT)
        .bind(org)
        .fetch_all(&mut *client)
        .await?;
    for row in rows {
        let reference: String = row.get(1);
        let version: String = row.get(2);
        let result = async {
            wamn_kubernetes::kubernetes::KubernetesStore::in_cluster()?
                .destroy(&reference, &version)
                .await
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, "retired credential collection failed");
        }
    }
    Ok(())
}

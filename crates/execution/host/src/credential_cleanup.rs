//! Remove retired values after the replacement generation ages five minutes.

use wamn_kubernetes::Store as _;
/// Collect retired values without changing the immutable credential metadata.
pub async fn collect(client: &tokio_postgres::Client, org: &str) -> anyhow::Result<()> {
    let rows = client
        .query(
            "SELECT store,ref,version FROM registry.retired_connection_credential($1)",
            &[&org],
        )
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

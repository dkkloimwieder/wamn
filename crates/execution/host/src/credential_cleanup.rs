//! Remove retired values after the replacement generation ages five minutes.

/// Collect retired values without changing the immutable credential metadata.
pub async fn collect(client: &tokio_postgres::Client, org: &str) -> anyhow::Result<()> {
    let rows = client
        .query(
            "SELECT store,ref,version FROM registry.retired_connection_credential($1)",
            &[&org],
        )
        .await?;
    for row in rows {
        let store: String = row.get(0);
        let reference: String = row.get(1);
        let version: String = row.get(2);
        let result = async {
            wamn_credential_store::open(&store)?
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

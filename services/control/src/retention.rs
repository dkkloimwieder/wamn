//! Private worker entry points for the environment chart's retention tasks.

use sqlx::Connection as _;
use std::path::Path;
use wamn_postgres::ConnectionInput;

async fn prune_identities(input: ConnectionInput<'_>) -> anyhow::Result<()> {
    // Bound connection establishment and cleanup.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut connection = wamn_postgres::connect(input).await?;
        let mut transaction = connection.begin().await?;
        sqlx::raw_sql("SET LOCAL statement_timeout = '5s'")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("SELECT platform.prune_tx_identity()")
            .execute(&mut *transaction)
            .await?;
        wamn_environment::credential_cleanup::collect(&mut transaction).await?;
        transaction.commit().await?;
        Ok(())
    })
    .await?
}

/// Remove identities that no running transaction can still use.
pub(crate) async fn identities(directory: &Path) -> anyhow::Result<()> {
    let mut entries = tokio::fs::read_dir(directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.')
            || !tokio::fs::metadata(&path)
                .await
                .is_ok_and(|metadata| metadata.is_file())
        {
            continue;
        }
        let result = async {
            let config = wamn_postgres::ConnectionConfig::read(&path).await?;
            let login = wamn_provision::workload_role::platform_connection(config)?
                .mounted_options(directory)
                .await?;
            prune_identities(login.into()).await
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, database = ?entry.file_name(), "transaction identity retention failed");
        }
    }
    Ok(())
}

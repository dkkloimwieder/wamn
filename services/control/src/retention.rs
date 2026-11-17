//! Private worker entry points for the environment chart's retention tasks.

use sqlx::Connection as _;

async fn prune_identities(
    input: &str,
    scope: wamn_provision::migrations::MigrationDatabase,
) -> anyhow::Result<()> {
    // Bound credential lookup, connection establishment, role selection and delete.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let configuration = if scope == wamn_provision::migrations::MigrationDatabase::Project {
            wamn_provision::workload_role::platform_configuration(input)?
        } else {
            scope.configuration(input)?
        };
        let mut connection = wamn_postgres::connect(&configuration).await?;
        if scope == wamn_provision::migrations::MigrationDatabase::System {
            scope.require_login(&mut connection).await?;
        }
        let mut transaction = connection.begin().await?;
        sqlx::raw_sql("SET LOCAL statement_timeout = '5s'")
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "DELETE FROM platform.tx_identity WHERE xid < pg_snapshot_xmin(pg_current_snapshot())",
        )
        .execute(&mut *transaction)
        .await?;
        if scope == wamn_provision::migrations::MigrationDatabase::Project {
            wamn_connection::credential_cleanup::collect(&mut transaction).await?;
        }
        transaction.commit().await?;
        Ok(())
    })
    .await?
}

/// Remove identities that no running transaction can still use.
pub(crate) async fn identities(directory: &std::path::Path, system: &str) -> anyhow::Result<()> {
    use wamn_provision::migrations::MigrationDatabase;
    if let Err(error) = prune_identities(system, MigrationDatabase::System).await {
        tracing::warn!(%error, "system transaction identity retention failed");
    }
    let mut entries = tokio::fs::read_dir(directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_name().to_string_lossy().starts_with('.')
            || !entry.file_type().await?.is_file()
        {
            continue;
        }
        let result = async {
            let input = tokio::fs::read_to_string(entry.path()).await?;
            prune_identities(&input, MigrationDatabase::Project).await
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, database = ?entry.file_name(), "transaction identity retention failed");
        }
    }
    Ok(())
}

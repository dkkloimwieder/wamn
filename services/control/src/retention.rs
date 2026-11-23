//! Private worker entry points for the environment chart's retention tasks.

use sqlx::Connection as _;
use wamn_postgres::ConnectionInput;

async fn prune_identities(input: ConnectionInput<'_>) -> anyhow::Result<()> {
    // Bound connection establishment and cleanup.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut connection = wamn_postgres::connect(input).await?;
        let mut transaction = connection.begin().await?;
        wamn_postgres::control::prune_transaction_identities(&mut transaction).await?;
        wamn_environment::credential_cleanup::collect(&mut transaction).await?;
        transaction.commit().await?;
        Ok(())
    })
    .await?
}

/// Remove identities that no running transaction can still use.
pub(crate) async fn identities(logins: &crate::project_login::ProjectLogins) -> anyhow::Result<()> {
    for (triple, instance) in logins.recorded().await? {
        let result = async {
            let login = logins.login(&triple, &instance).await?;
            prune_identities(login.into()).await
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, %triple, "transaction identity retention failed");
        }
    }
    Ok(())
}

//! Create one human principal in the system database.
//!
//! This command uses the provisioning administrator for the system database.
//! It creates the principal only. Access comes from
//! `grant-project-env-membership`, and the tenant `app_system.users` person row
//! comes from `reconcile-run-plane`.

use anyhow::Context as _;
use tokio_postgres::NoTls;
use wamn_platform_identity::{Principal, create_human};

use crate::provision_project_env::provisioning_transaction;

/// Inputs that name one new human.
#[derive(Debug)]
pub struct CreateHumanRequest {
    /// Subject that the identity provider asserts for this human.
    pub subject: String,

    /// Deliverable address of this human. One address names one principal.
    pub email: String,

    /// Name shown for this human.
    pub display_name: String,

    /// Provisioning administrator URL for the system database.
    pub system_database_url: String,
}

/// Create the human principal and return it as the system database stored it.
pub async fn create_human_principal(args: CreateHumanRequest) -> anyhow::Result<Principal> {
    let (mut client, connection) = tokio_postgres::connect(&args.system_database_url, NoTls)
        .await
        .context("connect to the system database to create a human principal")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system to create a human principal")?;
        let transaction = provisioning_transaction(&mut client).await?;
        let principal = create_human(&transaction, &args.subject, &args.email, &args.display_name)
            .await
            .context("create the human principal")?;
        transaction
            .commit()
            .await
            .context("commit the human principal")?;
        Ok::<_, anyhow::Error>(principal)
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

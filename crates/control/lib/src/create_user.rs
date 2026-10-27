//! Create one user principal in the system database.
//!
//! This command uses the provisioning administrator for the system database.
//! It creates the principal only. Access comes from
//! `grant-project-env-membership`, and the environment's `platform."user"` row
//! comes from `reconcile-run-plane`.

use anyhow::Context as _;
use tokio_postgres::NoTls;
use wamn_platform_identity::auth_changes::{AuthChanges, AuthDrops};
use wamn_platform_identity::{Principal, create_user};

use crate::provision_project_env::provisioning_transaction;

/// Inputs that name one new user.
#[derive(Debug)]
pub struct CreateUserRequest {
    /// Subject that the identity provider asserts for this user.
    pub subject: String,

    /// Deliverable address of this user. One address names one principal.
    pub email: String,

    /// Name shown for this user.
    pub display_name: String,

    /// Provisioning administrator URL for the system database.
    pub system_database_url: String,
}

/// Create the user principal and return it as the system database stored it.
pub async fn create_user_principal(
    args: CreateUserRequest,
    changes: &AuthChanges,
) -> anyhow::Result<Principal> {
    let (mut client, connection) = tokio_postgres::connect(&args.system_database_url, NoTls)
        .await
        .context("connect to the system database to create a user principal")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system to create a user principal")?;
        let transaction = provisioning_transaction(&mut client).await?;
        let principal = create_user(&transaction, &args.subject, &args.email, &args.display_name)
            .await
            .context("create the user principal")?;
        let mut drops = AuthDrops::default();
        drops.identity(principal.id().as_str());
        transaction
            .commit()
            .await
            .context("commit the user principal")?;
        changes.publish(drops).await;
        Ok::<_, anyhow::Error>(principal)
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

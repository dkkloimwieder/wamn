//! Create one user principal in the system database.
//!
//! This command uses the provisioning administrator for the system database.
//! It creates the principal only. Access comes from
//! `grant-project-env-membership`, and the environment's `platform."user"` row
//! comes from `reconcile-run-plane`.

use anyhow::Context as _;
use wamn_broker::Publisher;
use wamn_broker::auth_changes::AuthDrops;
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
    changes: &Publisher,
) -> anyhow::Result<Principal> {
    let mut client = wamn_postgres::connect(&args.system_database_url)
        .await
        .context("connect to the system database to create a user principal")?;

    let result = async {
        sqlx::raw_sql("SET ROLE wamn_system")
            .execute(&mut client)
            .await
            .map(drop)
            .context("SET ROLE wamn_system to create a user principal")?;
        let mut transaction = provisioning_transaction(&mut client).await?;
        let principal = create_user(
            &mut transaction,
            &args.subject,
            &args.email,
            &args.display_name,
        )
        .await
        .context("create the user principal")?;
        let mut drops = AuthDrops::default();
        drops.identity(principal.id().to_string().as_str());
        transaction
            .commit()
            .await
            .context("commit the user principal")?;
        changes.publish(drops).await;
        Ok::<_, anyhow::Error>(principal)
    }
    .await;
    drop(client);
    result
}

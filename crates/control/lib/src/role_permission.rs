//! Authored application roles and their permission rows
//! (docs/plan/platform-ui.md §2.2 to §2.4 and §4.1).
//!
//! A permission row is a stable operation reference that an administrator
//! granted to a role. A grant stores the row, served by a release or not, and
//! a revoke removes it. No deployment verb touches the rows. `admin` has no
//! rows.

use anyhow::Context as _;
use tokio_postgres::{Client, Transaction};
use wamn_platform_identity::application;
pub use wamn_platform_identity::application::PermissionGrantOutcome;
use wamn_platform_identity::auth_changes::{AuthChanges, AuthDrops};

use crate::user_role::{EnvironmentTarget, connect_target};
use wamn_control_provision::operation_grants::{
    OPERATION_GRANT_LOCK_SQL, OPERATION_GRANT_TRANSACTION_PRELUDE_SQL,
    operation_grant_floor_check_sql,
};
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql};

/// Lock the environment's roles and permissions, refuse a filtered view, and bind
/// `wamn:provisioning` as the actor of the writes.
async fn prepare(tx: &Transaction<'_>, environment_key: &str) -> anyhow::Result<()> {
    tx.query_one(OPERATION_GRANT_LOCK_SQL, &[&environment_key])
        .await
        .context("lock the environment roles and permissions")?;
    tx.batch_execute(OPERATION_GRANT_TRANSACTION_PRELUDE_SQL)
        .await
        .context("disable row filtering for the permission write")?;
    tx.batch_execute(&operation_grant_floor_check_sql())
        .await
        .context("verify the application authorization floor")?;
    tx.batch_execute(&bind_platform_principal_sql(
        PlatformComponent::Provisioning,
    ))
    .await
    .context("bind wamn:provisioning for the permission write")
}

/// Grant `reference` to the authored `role`.
pub async fn grant_permission(
    tx: &Transaction<'_>,
    environment_key: &str,
    role: &str,
    reference: &str,
    drops: &mut AuthDrops,
) -> anyhow::Result<PermissionGrantOutcome> {
    prepare(tx, environment_key).await?;
    Ok(application::grant_permission(tx, environment_key, role, reference, drops).await?)
}

/// Remove the permission `reference` from `role`. A reference the role does
/// not hold refuses.
pub async fn revoke_permission(
    tx: &Transaction<'_>,
    environment_key: &str,
    role: &str,
    reference: &str,
    drops: &mut AuthDrops,
) -> anyhow::Result<()> {
    prepare(tx, environment_key).await?;
    Ok(application::revoke_permission(tx, environment_key, role, reference, drops).await?)
}

/// Create the empty authored role `role`. A second create changes nothing.
pub async fn create_role(
    tx: &Transaction<'_>,
    environment_key: &str,
    role: &str,
    drops: &mut AuthDrops,
) -> anyhow::Result<bool> {
    prepare(tx, environment_key).await?;
    Ok(application::create_role(tx, environment_key, role, drops).await?)
}

/// Delete the authored role `role` with its assignments and permissions. A
/// delete of an absent role changes nothing.
pub async fn delete_role(
    tx: &Transaction<'_>,
    environment_key: &str,
    role: &str,
    drops: &mut AuthDrops,
) -> anyhow::Result<bool> {
    prepare(tx, environment_key).await?;
    Ok(application::delete_role(tx, environment_key, role, drops).await?)
}

/// Run `change` in one transaction on the registry-checked database of
/// `target`, and publish its drops after the commit.
async fn in_environment<T>(
    target: &EnvironmentTarget,
    changes: &AuthChanges,
    change: impl AsyncFnOnce(&Transaction<'_>, &mut AuthDrops) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let (mut client, connection): (Client, _) = connect_target(target).await?;
    let result = async {
        let tx = client
            .transaction()
            .await
            .context("begin the role change")?;
        let mut drops = AuthDrops::default();
        let value = change(&tx, &mut drops).await?;
        tx.commit().await.context("commit the role change")?;
        changes.publish(drops).await;
        Ok(value)
    }
    .await;
    drop(client);
    let _ = connection.await;
    result
}

/// `wamn-ctl create-role`: create an empty authored role.
pub async fn create_role_in(
    target: &EnvironmentTarget,
    role: &str,
    changes: &AuthChanges,
) -> anyhow::Result<bool> {
    in_environment(target, changes, async |tx, drops| {
        create_role(tx, &target.tenant, role, drops).await
    })
    .await
}

/// `wamn-ctl delete-role`: delete an authored role with its assignments and
/// permissions.
pub async fn delete_role_in(
    target: &EnvironmentTarget,
    role: &str,
    changes: &AuthChanges,
) -> anyhow::Result<bool> {
    in_environment(target, changes, async |tx, drops| {
        delete_role(tx, &target.tenant, role, drops).await
    })
    .await
}

/// `wamn-ctl grant-permission`: grant `reference` to `role`.
pub async fn grant_permission_in(
    target: &EnvironmentTarget,
    role: &str,
    reference: &str,
    changes: &AuthChanges,
) -> anyhow::Result<PermissionGrantOutcome> {
    in_environment(target, changes, async |tx, drops| {
        grant_permission(tx, &target.tenant, role, reference, drops).await
    })
    .await
}

/// `wamn-ctl revoke-permission`: remove `reference` from `role`.
pub async fn revoke_permission_in(
    target: &EnvironmentTarget,
    role: &str,
    reference: &str,
    changes: &AuthChanges,
) -> anyhow::Result<()> {
    in_environment(target, changes, async |tx, drops| {
        revoke_permission(tx, &target.tenant, role, reference, drops).await
    })
    .await
}

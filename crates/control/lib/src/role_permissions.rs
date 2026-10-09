//! Authored application roles and their permission rows
//! (docs/plan/platform-ui.md §2.2 to §2.4 and §4.1).
//!
//! A permission row is a root: a stable operation reference that an
//! administrator selected for a role (`permission = required_by`). A grant
//! stores the root, served by a release or not, and a revoke removes it. No
//! deployment verb touches the rows: each host expands a caller's roots through
//! the closures of the release it loaded (platform-deploy.md R18). `admin` has
//! no rows.

use anyhow::Context as _;
use tokio_postgres::{Client, Transaction};
pub use wamn_catalog::ReleaseClosures;
use wamn_platform_identity::application;
pub use wamn_platform_identity::application::{PermissionGrantOutcome, PermissionRevokeOutcome};

use crate::user_roles::{EnvironmentTarget, connect_target};
use wamn_control_provision::operation_grants::{
    OPERATION_GRANT_LOCK_SQL, OPERATION_GRANT_TRANSACTION_PRELUDE_SQL,
    operation_grant_floor_check_sql,
};
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql};

/// Lock the tenant's roles and permissions, refuse a filtered view, and bind
/// `wamn:provisioning` as the actor of the writes.
async fn prepare(tx: &Transaction<'_>, tenant: &str) -> anyhow::Result<()> {
    tx.query_one(OPERATION_GRANT_LOCK_SQL, &[&tenant])
        .await
        .context("lock the tenant roles and permissions")?;
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

/// Select `reference` for the authored `role` as a root. `current` holds the
/// closures the outcome reports; nothing stores them.
pub async fn grant_permission(
    tx: &Transaction<'_>,
    tenant: &str,
    role: &str,
    reference: &str,
    current: &ReleaseClosures,
) -> anyhow::Result<PermissionGrantOutcome> {
    prepare(tx, tenant).await?;
    Ok(
        application::grant_permission(tx, tenant, role, reference, current.closure(reference))
            .await?,
    )
}

/// Remove the selection of `reference` from `role` and every permission that
/// the selection required. A permission another selected root still requires
/// stays, and the outcome names those roots. A reference that the role holds
/// only because another root requires it refuses and names those roots.
pub async fn revoke_permission(
    tx: &Transaction<'_>,
    tenant: &str,
    role: &str,
    reference: &str,
) -> anyhow::Result<PermissionRevokeOutcome> {
    prepare(tx, tenant).await?;
    Ok(application::revoke_permission(tx, tenant, role, reference).await?)
}

/// Create the empty authored role `role`. A second create changes nothing.
pub async fn create_role(tx: &Transaction<'_>, tenant: &str, role: &str) -> anyhow::Result<bool> {
    prepare(tx, tenant).await?;
    Ok(application::create_role(tx, tenant, role).await?)
}

/// Delete the authored role `role` with its assignments and permissions. A
/// delete of an absent role changes nothing.
pub async fn delete_role(tx: &Transaction<'_>, tenant: &str, role: &str) -> anyhow::Result<bool> {
    prepare(tx, tenant).await?;
    Ok(application::delete_role(tx, tenant, role).await?)
}

/// Run `change` in one transaction on the registry-checked database of
/// `target`.
async fn in_environment<T>(
    target: &EnvironmentTarget,
    change: impl AsyncFnOnce(&Transaction<'_>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let (mut client, connection): (Client, _) = connect_target(target).await?;
    let result = async {
        let tx = client
            .transaction()
            .await
            .context("begin the role change")?;
        let value = change(&tx).await?;
        tx.commit().await.context("commit the role change")?;
        Ok(value)
    }
    .await;
    drop(client);
    let _ = connection.await;
    result
}

/// `wamn-ctl create-role`: create an empty authored role.
pub async fn create_role_in(target: &EnvironmentTarget, role: &str) -> anyhow::Result<bool> {
    in_environment(target, async |tx| {
        create_role(tx, &target.tenant, role).await
    })
    .await
}

/// `wamn-ctl delete-role`: delete an authored role with its assignments and
/// permissions.
pub async fn delete_role_in(target: &EnvironmentTarget, role: &str) -> anyhow::Result<bool> {
    in_environment(target, async |tx| {
        delete_role(tx, &target.tenant, role).await
    })
    .await
}

/// `wamn-ctl grant-permission`: select `reference` for `role`. The verb loads
/// no release, so the outcome reports no closure.
pub async fn grant_permission_in(
    target: &EnvironmentTarget,
    role: &str,
    reference: &str,
) -> anyhow::Result<PermissionGrantOutcome> {
    in_environment(target, async |tx| {
        grant_permission(
            tx,
            &target.tenant,
            role,
            reference,
            &ReleaseClosures::default(),
        )
        .await
    })
    .await
}

/// `wamn-ctl revoke-permission`: remove the selection of `reference` from
/// `role`.
pub async fn revoke_permission_in(
    target: &EnvironmentTarget,
    role: &str,
    reference: &str,
) -> anyhow::Result<PermissionRevokeOutcome> {
    in_environment(target, async |tx| {
        revoke_permission(tx, &target.tenant, role, reference).await
    })
    .await
}

//! Authored application roles and their permission rows
//! (docs/plan/platform-ui.md §2.2 to §2.4 and §4.1).
//!
//! A permission row holds a stable operation reference and the directly
//! selected root that requires it. A grant writes the root and its released
//! call-graph closure. A revoke removes the root, and the self-referencing key
//! of `app_system.permissions` removes the closure rows with it. Before a
//! candidate serving release becomes current, [`reconcile_release_permissions`]
//! removes each root the candidate does not serve and rewrites the closure of
//! each surviving root from the candidate. `admin` has no rows.

use anyhow::Context as _;
use tokio_postgres::{Client, Transaction};
pub use wamn_catalog::ReleaseClosures;
use wamn_catalog::ServingManifest;
use wamn_platform_identity::application;
pub use wamn_platform_identity::application::{PermissionGrantOutcome, PermissionRevokeOutcome};

use crate::user_roles::{EnvironmentTarget, connect_target};
use wamn_control_provision::operation_grants::{
    OPERATION_GRANT_LOCK_SQL, OPERATION_GRANT_TRANSACTION_PRELUDE_SQL,
    operation_grant_floor_check_sql,
};
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql};

/// What [`reconcile_release_permissions`] changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReleasePermissionOutcome {
    /// Selected roots removed, because the candidate does not serve them.
    pub roots_removed: u64,
    /// Closure rows inserted.
    pub required_added: u64,
    /// Closure rows removed, because their root no longer requires them.
    pub required_removed: u64,
}

const REMOVE_UNSERVED_ROOTS_SQL: &str = "DELETE FROM app_system.permissions \
     WHERE tenant_id = $1 AND permission = required_by AND NOT (permission = ANY($2::text[]))";

const REMOVE_UNREQUIRED_SQL: &str = "DELETE FROM app_system.permissions AS stored \
     WHERE stored.tenant_id = $1 AND stored.permission <> stored.required_by \
       AND NOT EXISTS (SELECT FROM unnest($2::text[], $3::text[]) AS required(root, permission) \
                        WHERE required.root = stored.required_by \
                          AND required.permission = stored.permission)";

const ADD_REQUIRED_SQL: &str = "INSERT INTO app_system.permissions \
       (tenant_id, role_name, permission, required_by) \
     SELECT root.tenant_id, root.role_name, required.permission, required.root \
       FROM app_system.permissions AS root \
       JOIN unnest($2::text[], $3::text[]) AS required(root, permission) \
         ON required.root = root.permission \
      WHERE root.tenant_id = $1 AND root.permission = root.required_by \
     ON CONFLICT DO NOTHING";

/// Reconcile the authored roles of `tenant` in the database at `url`, in a
/// transaction of its own. The development loop uses it before it starts the
/// host on a new local release.
pub async fn reconcile_release_permissions_at(
    url: &str,
    tenant: &str,
    closures: &ReleaseClosures,
) -> anyhow::Result<()> {
    let (mut client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .context("connect to reconcile the authored role permissions")?;
    let connection = tokio::spawn(connection);
    let result = async {
        let tx = client
            .transaction()
            .await
            .context("begin the permission reconciliation")?;
        reconcile_release_permissions(&tx, tenant, closures).await?;
        tx.commit()
            .await
            .context("commit the permission reconciliation")
    }
    .await;
    drop(client);
    let _ = connection.await;
    result
}

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

/// Reconcile every authored role of `tenant` against the closures of the
/// candidate release, inside the transaction that makes the candidate current.
/// If it fails, the caller's transaction fails, so the candidate does not
/// activate.
pub async fn reconcile_release_permissions(
    tx: &Transaction<'_>,
    tenant: &str,
    closures: &ReleaseClosures,
) -> anyhow::Result<ReleasePermissionOutcome> {
    prepare(tx, tenant).await?;
    let served: Vec<&str> = closures.roots().collect();
    let (roots, permissions) = closures.required_pairs();
    let roots_removed = tx
        .execute(REMOVE_UNSERVED_ROOTS_SQL, &[&tenant, &served])
        .await
        .context("remove the selected roots the candidate release does not serve")?;
    let required_removed = tx
        .execute(REMOVE_UNREQUIRED_SQL, &[&tenant, &roots, &permissions])
        .await
        .context("remove the permissions no selected root requires")?;
    let required_added = tx
        .execute(ADD_REQUIRED_SQL, &[&tenant, &roots, &permissions])
        .await
        .context("add the permissions the selected roots require")?;
    Ok(ReleasePermissionOutcome {
        roots_removed,
        required_added,
        required_removed,
    })
}

/// Select `reference` for the authored `role` and write its closure in the
/// current serving release, whose closures are `current`.
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

/// The closures of the release that the environment head of `environment`
/// names. `select-release` and `promote` write the head.
async fn current_closures(
    tx: &Transaction<'_>,
    tenant: &str,
    environment: &str,
) -> anyhow::Result<ReleaseClosures> {
    let row = tx
        .query_opt(
            "SELECT s.canonical_bytes FROM catalog.effective_release_heads AS h \
             JOIN catalog.release_manifest_snapshots AS s USING (tenant_id, effective_release_id) \
             WHERE h.tenant_id = $1 AND h.environment = $2",
            &[&tenant, &environment],
        )
        .await
        .context("read the current serving release")?
        .with_context(|| {
            format!("environment {environment} of tenant {tenant} has no current serving release")
        })?;
    let (manifest, _) = ServingManifest::from_canonical_bytes(&row.get::<_, Vec<u8>>(0))
        .context("the current release snapshot is not a canonical manifest")?;
    Ok(ReleaseClosures::from_manifest(&manifest))
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

/// `wamn-ctl grant-permission`: select `reference` for `role` against the
/// current serving release of the environment.
pub async fn grant_permission_in(
    target: &EnvironmentTarget,
    role: &str,
    reference: &str,
) -> anyhow::Result<PermissionGrantOutcome> {
    in_environment(target, async |tx| {
        // The lock orders the read of the head after a reconciliation that
        // moves it.
        prepare(tx, &target.tenant).await?;
        let current = current_closures(tx, &target.tenant, &target.env).await?;
        grant_permission(tx, &target.tenant, role, reference, &current).await
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

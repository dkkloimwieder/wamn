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

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context as _;
use tokio_postgres::Transaction;
use wamn_catalog::{ServingComponent, ServingManifest};
use wamn_control_provision::operation_grants::{
    OPERATION_GRANT_LOCK_SQL, OPERATION_GRANT_TRANSACTION_PRELUDE_SQL,
    operation_grant_floor_check_sql,
};
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql};
use wamn_engine::flow_http_routing::operation_reference;

/// The permission closure of every operation a serving release registers,
/// keyed by stable reference. Each closure holds the operation itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleaseClosures {
    closures: BTreeMap<String, BTreeSet<String>>,
}

impl ReleaseClosures {
    /// Read the closures that publish folded into the release components.
    pub fn from_manifest(manifest: &ServingManifest) -> Self {
        Self::from_components(&manifest.components)
    }

    /// Read the closures of `components`. A palette export registers no
    /// operation and contributes none.
    pub fn from_components<'a>(components: impl IntoIterator<Item = &'a ServingComponent>) -> Self {
        let mut closures: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for operation in components
            .into_iter()
            .flat_map(|component| component.operations.values())
        {
            let Some(sealed) = &operation.registered_operation else {
                continue;
            };
            let root = operation_reference(sealed).to_owned();
            let closure = closures.entry(root.clone()).or_default();
            closure.insert(root);
            closure.extend(
                operation
                    .permissions
                    .iter()
                    .map(|permission| operation_reference(permission).to_owned()),
            );
        }
        Self { closures }
    }

    /// The closure of `reference`, or `None` when the release does not serve it.
    pub fn closure(&self, reference: &str) -> Option<&BTreeSet<String>> {
        self.closures.get(reference)
    }

    /// Every `(root, permission)` pair with `permission != root`, as two
    /// parallel arrays.
    fn required_pairs(&self) -> (Vec<&str>, Vec<&str>) {
        self.closures
            .iter()
            .flat_map(|(root, closure)| {
                closure
                    .iter()
                    .filter(move |permission| *permission != root)
                    .map(move |permission| (root.as_str(), permission.as_str()))
            })
            .unzip()
    }
}

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
    let served: Vec<&str> = closures.closures.keys().map(String::as_str).collect();
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

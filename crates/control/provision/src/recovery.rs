//! Rendering the CNPG `Cluster` that **recovers** an org cluster from its WAL/PITR
//! object store (wamn-fibe).
//!
//! The other half of [`crate::backup`]. Backup ships every WAL segment and a
//! periodic base backup to the per-cluster prefix `s3://wamn-backups/wal/<cluster>`
//! through the Barman Cloud plugin; recovery reads that same prefix back into a
//! **new** cluster. CloudNativePG recovery is always **whole-cluster**: it
//! restores a base backup and replays WAL, so the unit is the cluster, which is
//! why [`crate::org`] renders one cluster per distinct recovery domain — the
//! blast radius of a restore is a designed boundary, not an accident.
//!
//! Three properties this renderer holds, each guarding a cost:
//!
//! * **The recovered cluster is never the source cluster.** A recovery CR applied
//!   under the source's own name would target the live cluster, so a target equal
//!   to the source is refused ([`ProvisionError::RecoveryIntoSourceCluster`]).
//! * **The recovered cluster archives to its OWN store.** Its `ObjectStore` is
//!   `<target>-store` over the prefix `…/wal/<target>`, so a restored cluster can
//!   never write WAL into the stream it was restored from. It also means a
//!   promoted restore keeps its backups instead of silently running unbacked.
//! * **The source store is read-only here.** The `externalClusters` entry carries
//!   the plugin WITHOUT `isWALArchiver`, so it is a read path only.
//!
//! **Pure** (SR3 / house rule 1): manifest renderers over the env policy. No K8s
//! client, no clock. `docs/operations/backup-and-recovery.md` is the runbook.

use serde_json::{Value, json};
use wamn_control_registry::{Env, EnvPolicy, Org};

use crate::backup::{BACKUP_PLUGIN_NAME, object_store_name};
use crate::error::ProvisionError;

/// The `Cluster` CR that recovers one recovery domain, with the backup resources
/// the recovered cluster needs for its OWN future backups.
#[derive(Debug, Clone)]
pub struct RecoveredCluster {
    /// The new `Cluster` CR: sized by the source's env policy, bootstrapped from
    /// the source's object store.
    pub cluster: Value,
    /// The recovered cluster's own `ObjectStore` (`<target>-store`) — a separate
    /// WAL prefix from the source's. Absent when the policy has no backup.
    pub object_store: Option<Value>,
    /// The recovered cluster's own `ScheduledBackup`. Absent when the policy has
    /// no backup.
    pub scheduled_backup: Option<Value>,
}

/// The `.spec.externalClusters` entry a recovery `Cluster` reads its base backup
/// and WAL from: the Barman Cloud plugin pointed at the **source** cluster's
/// `ObjectStore` and server name.
///
/// `serverName` names the folder under the store's `destinationPath`, and the
/// `ObjectStore` CRD forbids `serverName` in its own configuration
/// (`use the 'serverName' plugin parameter in the Cluster resource`), so it is
/// carried here. No `isWALArchiver`: this store is read-only to the recovered
/// cluster.
pub fn recovery_external_cluster(source: &str) -> Value {
    json!({
        "name": source,
        "plugin": {
            "name": BACKUP_PLUGIN_NAME,
            "parameters": {
                "barmanObjectName": object_store_name(source),
                "serverName": source,
            },
        },
    })
}

/// The `.spec.bootstrap` block of a recovery cluster: `recovery.source` names the
/// [`externalClusters`](recovery_external_cluster) entry to restore from.
///
/// `target_time` is the point-in-time stop, an RFC3339 timestamp taken straight
/// into `recoveryTarget.targetTime` — the operator picks the instant just before
/// the loss. `None` replays every archived WAL segment (full recovery), which is
/// what a lost-cluster restore wants.
pub fn recovery_bootstrap(source: &str, target_time: Option<&str>) -> Value {
    let mut recovery = json!({ "source": source });
    if let Some(at) = target_time {
        recovery["recoveryTarget"] = json!({ "targetTime": at });
    }
    json!({ "recovery": recovery })
}

/// Render the recovery of one org recovery domain into a new cluster named
/// `target`.
///
/// `owner` is the recovery-domain owner env, so the source cluster is
/// `<org>-<owner>` — the name [`crate::org::render_org_cluster_set`] gave it — and
/// the new cluster is sized by that same owner env's policy, because a restore
/// needs at least the storage the source had.
///
/// Errors with [`ProvisionError::OrgIsPooled`] (a pooled org owns no clusters, so
/// it has no WAL stream of its own to restore), [`ProvisionError::UnknownEnvPolicy`]
/// (the owner env names no policy, so the cluster cannot be sized), or
/// [`ProvisionError::RecoveryIntoSourceCluster`] (`target` is the source).
pub fn render_recovery_cluster(
    org: &Org,
    owner: &Env,
    policies: &[EnvPolicy],
    target: &str,
    target_time: Option<&str>,
) -> Result<RecoveredCluster, ProvisionError> {
    if let wamn_control_registry::Placement::Pooled { pool } = &org.placement {
        return Err(ProvisionError::OrgIsPooled { pool: pool.clone() });
    }
    let policy = policies.iter().find(|p| &p.name == owner).ok_or_else(|| {
        ProvisionError::UnknownEnvPolicy {
            name: owner.to_string(),
        }
    })?;
    let source = format!("{}-{}", org.id, owner);
    if target == source {
        return Err(ProvisionError::RecoveryIntoSourceCluster { cluster: source });
    }

    // The recovered cluster archives to ITS OWN store, so it can never write WAL
    // into the stream it was restored from, and a promoted restore keeps backups.
    let (object_store, scheduled_backup, own_store) = if policy.has_scheduled_backup() {
        (
            Some(crate::backup::render_object_store(target, policy)),
            Some(crate::backup::render_scheduled_backup(target, policy)),
            Some(object_store_name(target)),
        )
    } else {
        (None, None, None)
    };

    // Sized exactly as the source is sized, then the initdb bootstrap is REPLACED
    // by the recovery bootstrap (CNPG accepts one bootstrap method, not both).
    let mut cluster =
        crate::org::render_cluster(&org.id, owner, target, policy, own_store.as_deref());
    cluster["spec"]["bootstrap"] = recovery_bootstrap(&source, target_time);
    cluster["spec"]["externalClusters"] = json!([recovery_external_cluster(&source)]);
    cluster["metadata"]["labels"]["wamn.recovered-from"] = json!(source);

    Ok(RecoveredCluster {
        cluster,
        object_store,
        scheduled_backup,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_control_registry::Template;

    fn dedicated_org() -> Org {
        Template::dedicated().stamp("acme", "wamn-pg").0
    }

    #[test]
    fn the_recovery_cluster_reads_the_source_store_and_writes_its_own() {
        let org = dedicated_org();
        let policies = EnvPolicy::defaults();
        let r = render_recovery_cluster(
            &org,
            &Env::new("prod"),
            &policies,
            "acme-prod-restore",
            None,
        )
        .expect("dedicated org with a prod policy");

        // A NEW cluster, bootstrapped from the source rather than initdb.
        assert_eq!(r.cluster["metadata"]["name"], "acme-prod-restore");
        assert_eq!(
            r.cluster["spec"]["bootstrap"]["recovery"]["source"],
            "acme-prod"
        );
        assert!(r.cluster["spec"]["bootstrap"].get("initdb").is_none());

        // The source store is the READ path: the plugin entry names the source's
        // ObjectStore and server name, and is not a WAL archiver.
        let ext = &r.cluster["spec"]["externalClusters"][0];
        assert_eq!(ext["name"], "acme-prod");
        assert_eq!(ext["plugin"]["name"], "barman-cloud.cloudnative-pg.io");
        assert_eq!(
            ext["plugin"]["parameters"]["barmanObjectName"],
            "acme-prod-store"
        );
        assert_eq!(ext["plugin"]["parameters"]["serverName"], "acme-prod");
        assert!(ext["plugin"].get("isWALArchiver").is_none());

        // The WRITE path is the recovered cluster's OWN store, over its own WAL
        // prefix — it can never archive into the stream it restored from.
        assert_eq!(
            r.cluster["spec"]["plugins"][0]["parameters"]["barmanObjectName"],
            "acme-prod-restore-store"
        );
        assert_eq!(r.cluster["spec"]["plugins"][0]["isWALArchiver"], true);
        let store = r.object_store.expect("prod policy has a scheduled backup");
        assert_eq!(store["metadata"]["name"], "acme-prod-restore-store");
        assert_eq!(
            store["spec"]["configuration"]["destinationPath"],
            "s3://wamn-backups/wal/acme-prod-restore"
        );
        assert_eq!(
            r.scheduled_backup
                .expect("prod policy has a scheduled backup")["spec"]["cluster"]["name"],
            "acme-prod-restore"
        );
    }

    #[test]
    fn the_recovered_cluster_is_sized_by_the_source_env_policy() {
        let org = dedicated_org();
        let policies = EnvPolicy::defaults();
        let prod = policies
            .iter()
            .find(|p| p.name == Env::new("prod"))
            .unwrap();
        let r = render_recovery_cluster(
            &org,
            &Env::new("prod"),
            &policies,
            "acme-prod-restore",
            None,
        )
        .unwrap();
        // A restore needs at least the storage and instances the source had.
        assert_eq!(r.cluster["spec"]["storage"]["size"], prod.storage.as_str());
        assert_eq!(r.cluster["spec"]["instances"], prod.instances);
        assert_eq!(
            r.cluster["metadata"]["labels"]["wamn.recovered-from"],
            "acme-prod"
        );
    }

    #[test]
    fn a_point_in_time_target_carries_the_timestamp() {
        let org = dedicated_org();
        let r = render_recovery_cluster(
            &org,
            &Env::new("prod"),
            &EnvPolicy::defaults(),
            "acme-prod-restore",
            Some("2026-09-16T14:31:00Z"),
        )
        .unwrap();
        assert_eq!(
            r.cluster["spec"]["bootstrap"]["recovery"]["recoveryTarget"]["targetTime"],
            "2026-09-16T14:31:00Z"
        );
    }

    #[test]
    fn recovery_into_the_source_name_is_refused() {
        let org = dedicated_org();
        let error = render_recovery_cluster(
            &org,
            &Env::new("prod"),
            &EnvPolicy::defaults(),
            "acme-prod",
            None,
        )
        .unwrap_err();
        assert_eq!(
            error,
            ProvisionError::RecoveryIntoSourceCluster {
                cluster: "acme-prod".to_string()
            }
        );
    }

    #[test]
    fn a_backupless_policy_recovers_without_rendering_its_own_backup() {
        // `dev` has no backup cadence, so it has no WAL stream to restore FROM
        // either; the renderer still refuses to invent a store for the restore.
        let org = dedicated_org();
        let r = render_recovery_cluster(
            &org,
            &Env::new("dev"),
            &EnvPolicy::defaults(),
            "acme-dev-restore",
            None,
        )
        .unwrap();
        assert!(r.object_store.is_none());
        assert!(r.scheduled_backup.is_none());
        assert!(r.cluster["spec"].get("plugins").is_none());
    }

    #[test]
    fn a_pooled_org_has_no_cluster_to_recover() {
        let pooled = Template::trials().stamp("acme", "wamn-pg").0;
        let error = render_recovery_cluster(
            &pooled,
            &Env::new("prod"),
            &EnvPolicy::defaults(),
            "acme-prod-restore",
            None,
        )
        .unwrap_err();
        assert!(matches!(error, ProvisionError::OrgIsPooled { .. }));
    }
}

//! Rendering an org cluster's WAL/PITR **backup** config (wamn-e1g).
//!
//! The first backup mechanism in the four-tier topology
//! (docs/postgres-topology.md §Backup architecture): continuous **WAL archiving +
//! base backups** to object storage via the CloudNativePG **Barman Cloud plugin**
//! (`barman-cloud.cloudnative-pg.io`). The in-tree `barmanObjectStore` provider is
//! deprecated in CNPG 1.26 with removal slated for 1.31 — so this builds on the
//! plugin (a CNPG-I sidecar the operator drives; it needs its own install +
//! cert-manager for plugin↔operator mTLS — see deploy/barman-cloud-plugin.yaml).
//!
//! This gives **whole-cluster point-in-time recovery**: the **retention window**
//! ([`wal_retention`]) is the PITR-SLA knob, a per-tier lever this module exposes
//! (topology §Backup architecture: "WAL retention window is the PITR-SLA lever …
//! a per-tier, per-org knob e1g must expose"). The *other* backup mechanism —
//! per-project-env logical dumps (tenant-scoped restore, the 10.3 export) — is
//! [`crate::dump`] (wamn-q3n.10/.11); the two share the object store (MinIO,
//! deploy/minio.yaml).
//!
//! **Pure** (SR3 / house rule 1): K8s manifest renderers (`serde_json::Value` —
//! `kubectl apply -f` accepts JSON, the [`crate::org`] / [`crate::dump`]
//! precedent) + the tier knobs. No K8s client, no clock. `provision-org` emits the
//! CRs and the runbook applies them, in order: the **ObjectStore** before the
//! **Cluster** that references it (via `.spec.plugins`), and the **ScheduledBackup**
//! after the cluster exists.

use serde_json::{Value, json};
use wamn_registry::Tier;

/// The Barman Cloud plugin name a Cluster references (`.spec.plugins[].name`) and
/// a `Backup`/`ScheduledBackup` targets (`spec.pluginConfiguration.name`).
pub const BACKUP_PLUGIN_NAME: &str = "barman-cloud.cloudnative-pg.io";
/// The object-store bucket WAL + base backups are written under — distinct from
/// the logical-dump bucket ([`crate::dump::DEFAULT_BUCKET`]), so WAL streams and
/// dumps never collide in one prefix tree.
pub const WAL_BUCKET: &str = "wamn-backups";
/// The shared object-store credentials `Secret` (keys `ACCESS_KEY_ID` /
/// `ACCESS_SECRET_KEY`), created by deploy/minio.yaml. The ObjectStore's
/// `s3Credentials` reference it; the dump upload ([`crate::dump`]) reads the same.
pub const OBJECT_STORE_SECRET: &str = "wamn-object-store";
/// The in-cluster MinIO S3 endpoint (the deploy/minio.yaml `Service`). The shared
/// object store both WAL/PITR and the logical dumps write to.
pub const MINIO_ENDPOINT: &str = "http://minio.wamn-system.svc:9000";
/// The namespace backup CRs live in (alongside the clusters + object store).
const NAMESPACE: &str = "wamn-system";

/// The `ObjectStore` CR name for a cluster: `<cluster>-store`.
pub fn object_store_name(cluster: &str) -> String {
    format!("{cluster}-store")
}

/// The `ScheduledBackup` CR name for a cluster: `<cluster>-backup`.
pub fn scheduled_backup_name(cluster: &str) -> String {
    format!("{cluster}-backup")
}

/// The WAL/base-backup **retention window** (the PITR-SLA knob) by tier, as a
/// Barman duration. This is the recovery-window lever e1g exposes: a restore can
/// reach any instant between the point of recoverability (window edge) and the
/// latest archived WAL. The regulated `dedicated` tier keeps the **longest**
/// window; `trials` the shortest (pre-contract).
pub fn wal_retention(tier: Tier) -> &'static str {
    match tier {
        Tier::Trials => "7d",
        Tier::Standard => "14d",
        Tier::Dedicated => "30d",
    }
}

/// The base-backup cadence by tier, as a **6-field** cron (the CNPG
/// `ScheduledBackup` schedule format includes seconds). WAL archiving is
/// continuous; a base backup anchors the recovery window and is taken more often
/// on the regulated tier (tighter guaranteed point of recoverability).
pub fn base_backup_schedule(tier: Tier) -> &'static str {
    match tier {
        Tier::Trials => "0 0 2 * * *",      // daily 02:00
        Tier::Standard => "0 0 */12 * * *", // every 12h
        Tier::Dedicated => "0 0 */6 * * *", // every 6h
    }
}

/// Whether a cluster **role** gets continuous WAL/PITR: `prod` and `canary`
/// always (each is its own recovery domain, always backed up), `dev` not — its
/// restore path is the per-project-env logical dump (topology §Backup
/// architecture: "T2-dev optional", off by default). The predicate
/// [`render_org_cluster_set`](crate::org::render_org_cluster_set) uses to decide
/// which clusters carry the backup plugin + get an ObjectStore/ScheduledBackup.
pub fn backup_enabled_for_role(role: &str) -> bool {
    matches!(role, "prod" | "canary")
}

/// Labels stamped on a backup CR — platform ownership + the cluster it backs (so
/// tooling never parses the name), the [`crate::dump`] precedent.
fn backup_labels(cluster: &str) -> Value {
    json!({
        "app.kubernetes.io/managed-by": "wamn",
        "app.kubernetes.io/component": "org-backup",
        "wamn.cluster": cluster,
    })
}

/// Render the `ObjectStore` CR that backs a cluster's WAL/PITR — the Barman Cloud
/// plugin's store. `destinationPath` = `s3://<bucket>/wal/<cluster>` (a
/// **per-cluster prefix** under the shared bucket, so each recovery domain has its
/// own isolated WAL stream); `endpointURL` = the in-cluster MinIO; `s3Credentials`
/// → the shared object-store [`Secret`](OBJECT_STORE_SECRET); `spec.retentionPolicy`
/// = the tier recovery window ([`wal_retention`] — the PITR-SLA knob). WAL is
/// gzip-compressed.
pub fn render_object_store(cluster: &str, tier: Tier) -> Value {
    json!({
        "apiVersion": "barmancloud.cnpg.io/v1",
        "kind": "ObjectStore",
        "metadata": {
            "name": object_store_name(cluster),
            "namespace": NAMESPACE,
            "labels": backup_labels(cluster),
        },
        "spec": {
            // The PITR-SLA knob: how far back a restore can reach (the recovery
            // window). A per-tier, per-org lever (topology §Backup architecture).
            "retentionPolicy": wal_retention(tier),
            "configuration": {
                // Per-cluster prefix: each recovery domain's WAL is isolated.
                "destinationPath": format!("s3://{WAL_BUCKET}/wal/{cluster}"),
                "endpointURL": MINIO_ENDPOINT,
                "s3Credentials": {
                    "accessKeyId": { "name": OBJECT_STORE_SECRET, "key": "ACCESS_KEY_ID" },
                    "secretAccessKey": { "name": OBJECT_STORE_SECRET, "key": "ACCESS_SECRET_KEY" },
                },
                "wal": { "compression": "gzip" },
            },
        },
    })
}

/// The `.spec.plugins` entry a backup-enabled `Cluster` carries — references the
/// [`ObjectStore`](render_object_store) by name and declares this plugin the WAL
/// archiver (`isWALArchiver: true`), so every WAL segment is continuously shipped
/// to the store. Attached by [`crate::org`] to the WAL/PITR-enabled clusters.
pub fn cluster_backup_plugin(object_store: &str) -> Value {
    json!({
        "name": BACKUP_PLUGIN_NAME,
        "isWALArchiver": true,
        "parameters": { "barmanObjectName": object_store },
    })
}

/// Render the `ScheduledBackup` CR that anchors a cluster's recovery window — a
/// periodic **base backup** taken through the plugin (`spec.method: plugin`,
/// `pluginConfiguration.name` = [`BACKUP_PLUGIN_NAME`]) at the tier cadence
/// ([`base_backup_schedule`]). `immediate: true` takes one at creation so the
/// window opens without waiting for the first scheduled tick.
pub fn render_scheduled_backup(cluster: &str, tier: Tier) -> Value {
    json!({
        "apiVersion": "postgresql.cnpg.io/v1",
        "kind": "ScheduledBackup",
        "metadata": {
            "name": scheduled_backup_name(cluster),
            "namespace": NAMESPACE,
            "labels": backup_labels(cluster),
        },
        "spec": {
            "schedule": base_backup_schedule(tier),
            "immediate": true,
            "backupOwnerReference": "self",
            "cluster": { "name": cluster },
            "method": "plugin",
            "pluginConfiguration": { "name": BACKUP_PLUGIN_NAME },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_retention_is_a_tier_knob() {
        // The recovery window rises with the tier — the regulated tier keeps the
        // longest PITR window, trials the shortest.
        assert_eq!(wal_retention(Tier::Trials), "7d");
        assert_eq!(wal_retention(Tier::Standard), "14d");
        assert_eq!(wal_retention(Tier::Dedicated), "30d");
        // Three distinct windows.
        let all: Vec<_> = Tier::ALL.iter().map(|&t| wal_retention(t)).collect();
        let uniq: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(uniq.len(), 3, "each tier has its own recovery window");
    }

    #[test]
    fn base_backup_schedule_is_a_tier_knob() {
        // 6-field cron (CNPG ScheduledBackup includes seconds); cadence tightens
        // with the tier.
        assert_eq!(base_backup_schedule(Tier::Trials), "0 0 2 * * *");
        assert_eq!(base_backup_schedule(Tier::Standard), "0 0 */12 * * *");
        assert_eq!(base_backup_schedule(Tier::Dedicated), "0 0 */6 * * *");
        for &t in Tier::ALL.iter() {
            assert_eq!(
                base_backup_schedule(t).split_whitespace().count(),
                6,
                "6-field cron"
            );
        }
        let all: Vec<_> = Tier::ALL.iter().map(|&t| base_backup_schedule(t)).collect();
        let uniq: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(uniq.len(), 3, "each tier has its own base-backup cadence");
    }

    #[test]
    fn backup_enabled_for_prod_and_canary_not_dev() {
        // prod + canary are their own recovery domains — always WAL/PITR-backed.
        assert!(backup_enabled_for_role("prod"));
        assert!(backup_enabled_for_role("canary"));
        // dev's restore path is the logical dump ("T2-dev optional"), off by default.
        assert!(!backup_enabled_for_role("dev"));
    }

    #[test]
    fn object_store_targets_minio_with_a_per_cluster_prefix_and_retention() {
        let os = render_object_store("acme-prod", Tier::Standard);
        assert_eq!(os["apiVersion"], "barmancloud.cnpg.io/v1");
        assert_eq!(os["kind"], "ObjectStore");
        assert_eq!(os["metadata"]["name"], "acme-prod-store");
        assert_eq!(os["metadata"]["namespace"], "wamn-system");
        // The PITR-SLA knob = the tier's recovery window.
        assert_eq!(os["spec"]["retentionPolicy"], "14d");
        // Per-cluster WAL prefix under the shared bucket (each recovery domain
        // isolated). Two clusters never share a WAL prefix.
        assert_eq!(
            os["spec"]["configuration"]["destinationPath"],
            "s3://wamn-backups/wal/acme-prod"
        );
        assert_ne!(
            render_object_store("acme-canary", Tier::Standard)["spec"]["configuration"]["destinationPath"],
            os["spec"]["configuration"]["destinationPath"]
        );
        // Points at the in-cluster MinIO with the shared object-store credentials.
        assert_eq!(
            os["spec"]["configuration"]["endpointURL"],
            "http://minio.wamn-system.svc:9000"
        );
        assert_eq!(
            os["spec"]["configuration"]["s3Credentials"]["accessKeyId"]["name"],
            "wamn-object-store"
        );
        assert_eq!(
            os["spec"]["configuration"]["s3Credentials"]["secretAccessKey"]["key"],
            "ACCESS_SECRET_KEY"
        );
        // The dedicated tier's store carries the longer window.
        assert_eq!(
            render_object_store("acme-prod", Tier::Dedicated)["spec"]["retentionPolicy"],
            "30d"
        );
    }

    #[test]
    fn cluster_plugin_references_the_object_store_as_wal_archiver() {
        let p = cluster_backup_plugin("acme-prod-store");
        assert_eq!(p["name"], "barman-cloud.cloudnative-pg.io");
        assert_eq!(p["isWALArchiver"], true);
        assert_eq!(p["parameters"]["barmanObjectName"], "acme-prod-store");
    }

    #[test]
    fn scheduled_backup_uses_the_plugin_method_at_the_tier_cadence() {
        let sb = render_scheduled_backup("acme-prod", Tier::Dedicated);
        assert_eq!(sb["apiVersion"], "postgresql.cnpg.io/v1");
        assert_eq!(sb["kind"], "ScheduledBackup");
        assert_eq!(sb["metadata"]["name"], "acme-prod-backup");
        // Taken through the Barman plugin (not the in-tree/tablespace methods).
        assert_eq!(sb["spec"]["method"], "plugin");
        assert_eq!(
            sb["spec"]["pluginConfiguration"]["name"],
            "barman-cloud.cloudnative-pg.io"
        );
        assert_eq!(sb["spec"]["cluster"]["name"], "acme-prod");
        assert_eq!(sb["spec"]["schedule"], "0 0 */6 * * *");
        assert_eq!(sb["spec"]["immediate"], true);
    }
}

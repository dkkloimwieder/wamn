//! Rendering an org cluster's WAL/PITR **backup** config (wamn-e1g).
//!
//! The first backup mechanism in the four-tier topology
//! (docs/postgres-topology.md §Backup architecture): continuous **WAL archiving +
//! base backups** to object storage via the CloudNativePG **Barman Cloud plugin**
//! (`barman-cloud.cloudnative-pg.io`). The in-tree `barmanObjectStore` provider is
//! deprecated in CNPG 1.26 with removal slated for 1.31 — so this builds on the
//! plugin (a CNPG-I sidecar the operator drives; it needs its own install +
//! cert-manager for plugin↔operator mTLS — see deploy/infra/barman-cloud-plugin.yaml).
//!
//! This gives **whole-cluster point-in-time recovery**: the **retention window**
//! (`EnvPolicy::wal_retention`) is the PITR-SLA knob — a **per-env policy** lever
//! under D18 (cjv.21: sizing is policy-driven, not a closed tier), and the
//! **base-backup cadence** is `EnvPolicy::backup_cadence` (empty = no scheduled
//! backup). The *other* backup mechanism — per-project-env logical dumps
//! (tenant-scoped restore, the 10.3 export) — is [`crate::dump`] (wamn-q3n.10/.11);
//! the two share the object store (MinIO, deploy/infra/minio.yaml).
//!
//! **Pure** (SR3 / house rule 1): K8s manifest renderers (`serde_json::Value` —
//! `kubectl apply -f` accepts JSON, the [`crate::org`] / [`crate::dump`]
//! precedent) reading the env-policy knobs. No K8s client, no clock. `provision-org` emits the
//! CRs and the runbook applies them, in order: the **ObjectStore** before the
//! **Cluster** that references it (via `.spec.plugins`), and the **ScheduledBackup**
//! after the cluster exists.

use serde_json::{Value, json};
use wamn_registry::EnvPolicy;

/// The Barman Cloud plugin name a Cluster references (`.spec.plugins[].name`) and
/// a `Backup`/`ScheduledBackup` targets (`spec.pluginConfiguration.name`).
pub const BACKUP_PLUGIN_NAME: &str = "barman-cloud.cloudnative-pg.io";
/// The object-store bucket WAL + base backups are written under — distinct from
/// the logical-dump bucket ([`crate::dump::DEFAULT_BUCKET`]), so WAL streams and
/// dumps never collide in one prefix tree.
pub const WAL_BUCKET: &str = "wamn-backups";
/// The shared object-store credentials `Secret` (keys `ACCESS_KEY_ID` /
/// `ACCESS_SECRET_KEY`), created by deploy/infra/minio.yaml. The ObjectStore's
/// `s3Credentials` reference it; the dump upload ([`crate::dump`]) reads the same.
pub const OBJECT_STORE_SECRET: &str = "wamn-object-store";
/// The in-cluster MinIO S3 endpoint (the deploy/infra/minio.yaml `Service`). The shared
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
/// = the env policy's `wal_retention` (the PITR-SLA knob, D18 — sized by the
/// owner env's policy, not a closed tier). WAL is gzip-compressed.
pub fn render_object_store(cluster: &str, policy: &EnvPolicy) -> Value {
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
            // window). A per-env policy lever (D18 — cjv.21).
            "retentionPolicy": policy.wal_retention,
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
/// `pluginConfiguration.name` = [`BACKUP_PLUGIN_NAME`]) at the env policy's
/// `backup_cadence` (a 6-field CNPG cron; D18 — sized by the owner env's policy).
/// `immediate: true` takes one at creation so the window opens without waiting for
/// the first scheduled tick.
pub fn render_scheduled_backup(cluster: &str, policy: &EnvPolicy) -> Value {
    json!({
        "apiVersion": "postgresql.cnpg.io/v1",
        "kind": "ScheduledBackup",
        "metadata": {
            "name": scheduled_backup_name(cluster),
            "namespace": NAMESPACE,
            "labels": backup_labels(cluster),
        },
        "spec": {
            "schedule": policy.backup_cadence,
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
    fn prod_policy_has_a_backup_dev_does_not() {
        // The base-backup + PITR knobs are per-env policy fields (D18): prod has a
        // cadence + retention window, dev has neither (its restore path is the dump).
        assert!(EnvPolicy::prod().has_scheduled_backup());
        assert!(!EnvPolicy::dev().has_scheduled_backup());
        assert_eq!(EnvPolicy::prod().wal_retention, "14d");
        assert_eq!(EnvPolicy::prod().backup_cadence, "0 0 */6 * * *");
    }

    #[test]
    fn object_store_targets_minio_with_a_per_cluster_prefix_and_policy_retention() {
        let os = render_object_store("acme-prod", &EnvPolicy::prod());
        assert_eq!(os["apiVersion"], "barmancloud.cnpg.io/v1");
        assert_eq!(os["kind"], "ObjectStore");
        assert_eq!(os["metadata"]["name"], "acme-prod-store");
        assert_eq!(os["metadata"]["namespace"], "wamn-system");
        // The PITR-SLA knob = the env policy's wal_retention.
        assert_eq!(os["spec"]["retentionPolicy"], "14d");
        // Per-cluster WAL prefix under the shared bucket (each recovery domain
        // isolated). Two clusters never share a WAL prefix.
        assert_eq!(
            os["spec"]["configuration"]["destinationPath"],
            "s3://wamn-backups/wal/acme-prod"
        );
        assert_ne!(
            render_object_store("acme-canary", &EnvPolicy::prod())["spec"]["configuration"]["destinationPath"],
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
    }

    #[test]
    fn cluster_plugin_references_the_object_store_as_wal_archiver() {
        let p = cluster_backup_plugin("acme-prod-store");
        assert_eq!(p["name"], "barman-cloud.cloudnative-pg.io");
        assert_eq!(p["isWALArchiver"], true);
        assert_eq!(p["parameters"]["barmanObjectName"], "acme-prod-store");
    }

    #[test]
    fn scheduled_backup_uses_the_plugin_method_at_the_policy_cadence() {
        let sb = render_scheduled_backup("acme-prod", &EnvPolicy::prod());
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
        // The schedule comes from the env policy's backup_cadence.
        assert_eq!(sb["spec"]["schedule"], "0 0 */6 * * *");
        assert_eq!(sb["spec"]["immediate"], true);
    }
}

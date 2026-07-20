//! Rendering a **dedicated** org's CNPG `Cluster` set (wamn-q3n.6, generalized to
//! the D18 policy model by wamn-8df.3).
//!
//! `provision-org` renders one Postgres cluster per **distinct recovery-domain
//! owner** across the env-policy set — the D18 generalization of the old
//! `<org>-prod` / `<org>-canary` / `<org>-dev` fixed shape:
//!
//! * with the default `{dev, prod}` policies → `<org>-dev` and `<org>-prod`;
//! * add a `canary` policy sharing prod's recovery domain → still two clusters
//!   (canary co-resides on `<org>-prod`, the T2 collapse);
//! * make `canary`'s recovery domain its own → a third cluster `<org>-canary`
//!   (the T4 maximal-separation property).
//!
//! Each cluster is **sized by the policy of its owner env** (`instances` /
//! `storage` / `cpu` / `memory` / `image` — fixing cjv.21: sizes are policy-driven,
//! not hard-coded), and a policy with a non-empty `backup_cadence` carries the
//! Barman Cloud WAL/PITR plugin + its `ObjectStore` / `ScheduledBackup` CRs (the
//! knobs — retention window, cadence — are in [`crate::backup`]).
//!
//! Rendered as `serde_json::Value` CNPG `Cluster` custom resources (`kubectl apply
//! -f` accepts JSON). This crate is pure — no K8s client. A **pooled** org owns no
//! clusters (it shares the pool) → [`render_org_cluster_set`] errors with
//! [`ProvisionError::OrgIsPooled`].

use serde_json::{Value, json};
use wamn_registry::{Env, EnvPolicy, Org, Placement};

use crate::error::ProvisionError;

/// The namespace org clusters live in (alongside the guardrailed clusters).
const NAMESPACE: &str = "wamn-system";

/// The `max_slot_wal_keep_size` WAL-retention bound every rendered cluster carries.
///
/// The §11 sharp edge (docs/event-plane-jetstream.md): a forgotten CDC logical
/// slot pins WAL *forever* without a bound, and the reader never GCs a slot it
/// did not create. `max_slot_wal_keep_size` is the backstop — once a slot falls
/// this far behind, PG invalidates it (a first-class, alerted incident) instead
/// of letting WAL fill the volume and take the primary down. Always-on:
/// single-instance pools host CDC slots too (the reader MVP runs on `wamn-pg`),
/// so no cluster is ever renderable without a bound. `1GB` is the S-CDC-1-proven
/// value (poc/cdc1/cdc1-cluster.yaml) for the 2Gi cluster sizing rendered here.
const WAL_KEEP_BOUND: &str = "1GB";

/// `logical_decoding_work_mem` on multi-instance CDC clusters — the per-walsender
/// reorder-buffer bound.
///
/// Kept well under the 256Mi pod memory request so concurrent logical-decoding
/// sessions cannot exhaust the pod; a transaction exceeding it *streams*
/// (protocol-v2, PG14+) to the reader early rather than buffering — the
/// bounded-memory / early-delivery property a CDC pipeline wants. `16MB` is 4× the
/// S-CDC-1 spike's deliberately-minimal 4MB (tuned to force streaming of a 1M-row
/// txn), leaving headroom for ordinary small transactions to decode without
/// streaming overhead. Emitted only with the failover-sync block below (a
/// single-instance pool needs only the WAL bound; there its PG default applies).
const LOGICAL_DECODING_WORK_MEM: &str = "16MB";

/// The rendered CNPG `Cluster` set for a dedicated org: one cluster per distinct
/// recovery-domain owner env (`<org>-<owner>`), each sized by that owner env's
/// [`EnvPolicy`], plus the WAL/PITR `ObjectStore` / `ScheduledBackup` CRs for the
/// backup-enabled ones (a policy with a non-empty `backup_cadence`).
#[derive(Debug, Clone)]
pub struct OrgClusters {
    /// One CNPG `Cluster` CR per distinct recovery-domain owner (e.g. `<org>-dev`,
    /// `<org>-prod`), sized by the owner env's policy.
    pub clusters: Vec<Value>,
    /// The `ObjectStore` CRs for the backup-enabled clusters — applied **before**
    /// their clusters (the plugin references them).
    pub object_stores: Vec<Value>,
    /// The `ScheduledBackup` CRs for the backup-enabled clusters — applied
    /// **after** their clusters exist.
    pub scheduled_backups: Vec<Value>,
}

/// Render a dedicated org's CNPG `Cluster` set (D18). One cluster per **distinct
/// recovery-domain owner** across `policies` — `<org>-<owner>` — sized by the
/// owner env's policy; backup-enabled owners (non-empty `backup_cadence`) also get
/// their WAL/PITR `ObjectStore` + `ScheduledBackup` CRs and a Barman plugin ref.
/// `policies` should come ordered by `promotion_rank` (the DB read is), so the
/// rendered set is stable.
///
/// Errors with [`ProvisionError::OrgIsPooled`] for a pooled org (it shares the
/// pool — `provision-org` records only its registry row, emitting no CRs), or
/// [`ProvisionError::UnknownEnvPolicy`] if a recovery-domain owner env names no
/// policy in the set (a malformed set — `validate()` flags it).
pub fn render_org_cluster_set(
    org: &Org,
    policies: &[EnvPolicy],
) -> Result<OrgClusters, ProvisionError> {
    if let Placement::Pooled { pool } = &org.placement {
        return Err(ProvisionError::OrgIsPooled { pool: pool.clone() });
    }

    // Distinct recovery-domain owners, in policy (promotion-rank) order. `canary`
    // sharing prod's domain collapses onto `prod`; `canary` with its own domain
    // adds `<org>-canary`.
    let mut owners: Vec<&Env> = Vec::new();
    for p in policies {
        let owner = p.owner();
        if !owners.contains(&owner) {
            owners.push(owner);
        }
    }

    let mut clusters = Vec::new();
    let mut object_stores = Vec::new();
    let mut scheduled_backups = Vec::new();
    for owner in owners {
        // The cluster is SIZED by its owner env's own policy.
        let policy = policies.iter().find(|p| &p.name == owner).ok_or_else(|| {
            ProvisionError::UnknownEnvPolicy {
                name: owner.to_string(),
            }
        })?;
        let name = format!("{}-{}", org.id, owner);
        if policy.has_scheduled_backup() {
            object_stores.push(crate::backup::render_object_store(&name, policy));
            scheduled_backups.push(crate::backup::render_scheduled_backup(&name, policy));
            let store = crate::backup::object_store_name(&name);
            clusters.push(render_cluster(&org.id, owner, &name, policy, Some(&store)));
        } else {
            clusters.push(render_cluster(&org.id, owner, &name, policy, None));
        }
    }

    Ok(OrgClusters {
        clusters,
        object_stores,
        scheduled_backups,
    })
}

/// Common labels stamped on every org cluster — platform ownership + identity (the
/// org and the recovery-domain owner env), so tooling never parses the name.
fn cluster_labels(org: &str, owner: &str) -> Value {
    json!({
        "app.kubernetes.io/managed-by": "wamn",
        "app.kubernetes.io/component": "org-cluster",
        "wamn.org": org,
        "wamn.recovery-domain": owner,
    })
}

/// Render one org CNPG `Cluster` for recovery-domain `owner`, sized entirely by
/// its [`EnvPolicy`] (D18 — cjv.21):
///
/// * **HA** (`policy.instances >= 2`): pod anti-affinity spreads instances across
///   nodes so a node loss drops at most one, PLUS the D19 v3 §4 failover-slot
///   CONTINUITY config — `replicationSlots.highAvailability`
///   (`synchronizeLogicalDecoding`) + the `sync_replication_slots` /
///   `hot_standby_feedback` / `logical_decoding_work_mem` GUCs — since a CDC
///   logical slot survives switchover only when CNPG syncs it to a standby, which
///   only a multi-instance cluster has.
/// * **hibernation-eligible** (`policy.hibernation == "eligible"`): carries the
///   `cnpg.io/hibernation` annotation set `off` (opted into the lifecycle but
///   running, so it comes up ready at provision; the platform off-hours scheduler
///   may flip it `on`, roughly halving idle cost). Independent of instance count.
///
/// All: `enableSuperuserAccess` (the wamn-q3n.7 per-project-env path connects as
/// superuser to CREATE the databases/roles), a non-TLS `pg_hba` (the repo connects
/// `NoTls`), **NO cpu limit** — requests only (the S2 CFS lesson) — and the
/// always-on `max_slot_wal_keep_size` WAL bound (the §11 sharp edge, see
/// [`WAL_KEEP_BOUND`]).
///
/// `backup_object_store` (wamn-e1g): when `Some`, the cluster carries a Barman
/// Cloud plugin ref in `.spec.plugins` naming that ObjectStore — continuous
/// WAL/PITR. `None` for a policy with no scheduled backup (its restore path is the
/// logical dump). We use the plugin's `.spec.plugins`, not the deprecated in-tree
/// `.spec.backup.barmanObjectStore`.
fn render_cluster(
    org: &str,
    owner: &Env,
    name: &str,
    policy: &EnvPolicy,
    backup_object_store: Option<&str>,
) -> Value {
    let mut metadata = json!({
        "name": name,
        "namespace": NAMESPACE,
        "labels": cluster_labels(org, owner),
    });
    let mut spec = json!({
        "instances": policy.instances,
        "imageName": policy.image,
        "primaryUpdateStrategy": "unsupervised",
        "enableSuperuserAccess": true,
        "resources": { "requests": { "cpu": policy.cpu, "memory": policy.memory } },
        "storage": { "size": policy.storage },
        "postgresql": {
            "pg_hba": ["host all all all scram-sha-256"],
            // The §11 WAL-retention bound is ALWAYS set (see WAL_KEEP_BOUND) —
            // the one CDC-capture knob every cluster carries, single- or
            // multi-instance. The failover-sync parameters are added below only
            // when there is a standby to sync a slot to.
            "parameters": { "max_slot_wal_keep_size": WAL_KEEP_BOUND },
        },
        // A neutral placeholder DB; the real per-project-env databases are created
        // declaratively by wamn-q3n.7 (the CNPG Database CRD).
        "bootstrap": { "initdb": { "database": "app", "owner": "app" } },
    });
    // WAL/PITR via the Barman Cloud plugin (wamn-e1g) for a backup-enabled cluster.
    if let Some(store) = backup_object_store {
        spec["plugins"] = json!([crate::backup::cluster_backup_plugin(store)]);
    }
    if policy.instances >= 2 {
        spec["affinity"] = json!({
            "enablePodAntiAffinity": true,
            "topologyKey": "kubernetes.io/hostname",
            "podAntiAffinityType": "preferred",
        });
        // Failover-slot CONTINUITY across switchover (D19 v3 §4/§11). Meaningful
        // only with a standby to sync the logical slot to, so it keys off the
        // SAME instance count as HA anti-affinity — no separate env-policy column
        // (the simplest correct shape; a single-instance pool needs only the WAL
        // bound above). CNPG mirrors each logical slot to the standbys
        // (`synchronizeLogicalDecoding`); `sync_replication_slots` is the PG-side
        // switch, `hot_standby_feedback` keeps the standby from pruning rows the
        // synced slot still needs, and `logical_decoding_work_mem` bounds each
        // walsender's reorder buffer (see LOGICAL_DECODING_WORK_MEM).
        spec["replicationSlots"] = json!({
            "highAvailability": { "enabled": true, "synchronizeLogicalDecoding": true },
        });
        let params = &mut spec["postgresql"]["parameters"];
        params["sync_replication_slots"] = json!("on");
        params["hot_standby_feedback"] = json!("on");
        params["logical_decoding_work_mem"] = json!(LOGICAL_DECODING_WORK_MEM);
    }
    if policy.hibernation_eligible() {
        metadata["annotations"] = json!({ "cnpg.io/hibernation": "off" });
    }
    json!({
        "apiVersion": "postgresql.cnpg.io/v1",
        "kind": "Cluster",
        "metadata": metadata,
        "spec": spec,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_registry::RecoveryDomain;

    /// The default policy set + a `canary` sharing prod's recovery domain.
    fn policies_with_shared_canary() -> Vec<EnvPolicy> {
        let mut ps = EnvPolicy::defaults();
        ps.push(EnvPolicy {
            name: Env::new("canary"),
            recovery_domain: RecoveryDomain::SharedWith(Env::new("prod")),
            promotion_rank: 20,
            ..EnvPolicy::prod()
        });
        ps
    }

    fn cluster_named<'a>(set: &'a OrgClusters, name: &str) -> &'a Value {
        set.clusters
            .iter()
            .find(|c| c["metadata"]["name"] == name)
            .unwrap_or_else(|| panic!("no cluster {name}"))
    }

    #[test]
    fn dedicated_org_renders_a_cluster_per_recovery_domain_sized_by_policy() {
        let set = render_org_cluster_set(&Org::dedicated("acme"), &EnvPolicy::defaults()).unwrap();
        // Two owners for the default set: dev + prod.
        assert_eq!(set.clusters.len(), 2);
        let dev = cluster_named(&set, "acme-dev");
        let prod = cluster_named(&set, "acme-prod");
        // Sized by the owner env's policy (the cjv.21 fix — not hard-coded).
        assert_eq!(dev["spec"]["instances"], 1);
        assert_eq!(prod["spec"]["instances"], 3);
        assert_eq!(prod["spec"]["storage"]["size"], "2Gi");
        assert_eq!(prod["spec"]["imageName"], EnvPolicy::prod().image);
        // Identity labels come from the org + owner env.
        assert_eq!(prod["metadata"]["labels"]["wamn.org"], "acme");
        assert_eq!(prod["metadata"]["labels"]["wamn.recovery-domain"], "prod");
    }

    #[test]
    fn canary_sharing_prod_collapses_onto_the_prod_cluster() {
        // canary shared-with prod → still two clusters (no <org>-canary).
        let set = render_org_cluster_set(&Org::dedicated("acme"), &policies_with_shared_canary())
            .unwrap();
        let names: Vec<_> = set
            .clusters
            .iter()
            .map(|c| c["metadata"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 2, "canary collapses onto prod (T2)");
        assert!(names.contains(&"acme-dev") && names.contains(&"acme-prod"));
        assert!(!names.contains(&"acme-canary"));
    }

    #[test]
    fn canary_own_domain_renders_a_third_cluster() {
        // canary as its own recovery domain → a third cluster <org>-canary (T4).
        let mut ps = EnvPolicy::defaults();
        ps.push(EnvPolicy {
            name: Env::new("canary"),
            recovery_domain: RecoveryDomain::Own,
            promotion_rank: 20,
            ..EnvPolicy::prod()
        });
        let set = render_org_cluster_set(&Org::dedicated("acme"), &ps).unwrap();
        let names: Vec<_> = set
            .clusters
            .iter()
            .map(|c| c["metadata"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"acme-canary"));
        let canary = cluster_named(&set, "acme-canary");
        assert_eq!(
            canary["metadata"]["labels"]["wamn.recovery-domain"],
            "canary"
        );
    }

    #[test]
    fn ha_and_hibernation_are_policy_driven() {
        let set = render_org_cluster_set(&Org::dedicated("acme"), &EnvPolicy::defaults()).unwrap();
        let dev = cluster_named(&set, "acme-dev");
        let prod = cluster_named(&set, "acme-prod");
        // prod (instances 3): HA anti-affinity, NOT hibernated.
        assert_eq!(prod["spec"]["affinity"]["enablePodAntiAffinity"], true);
        assert!(prod["metadata"]["annotations"].is_null());
        // dev (instances 1, hibernation eligible): hibernation annotation, no HA.
        assert_eq!(dev["metadata"]["annotations"]["cnpg.io/hibernation"], "off");
        assert!(dev["spec"]["affinity"].is_null());
    }

    #[test]
    fn every_cluster_carries_the_wal_retention_bound() {
        // §11 sharp edge: `max_slot_wal_keep_size` is ALWAYS-ON — a forgotten CDC
        // slot must never be able to pin WAL unbounded, on single- OR
        // multi-instance clusters.
        let set = render_org_cluster_set(&Org::dedicated("acme"), &EnvPolicy::defaults()).unwrap();
        assert!(!set.clusters.is_empty());
        for c in &set.clusters {
            assert_eq!(
                c["spec"]["postgresql"]["parameters"]["max_slot_wal_keep_size"], "1GB",
                "every rendered cluster carries the WAL bound"
            );
        }
    }

    #[test]
    fn single_instance_cluster_gets_only_the_wal_bound() {
        // dev (instances 1): the WAL bound, but NO failover-slot-sync config —
        // there is no standby to sync a slot to (the "single-instance pools need
        // only the WAL bound" decision).
        let set = render_org_cluster_set(&Org::dedicated("acme"), &EnvPolicy::defaults()).unwrap();
        let dev = cluster_named(&set, "acme-dev");
        let params = &dev["spec"]["postgresql"]["parameters"];
        assert_eq!(params["max_slot_wal_keep_size"], "1GB");
        assert!(dev["spec"]["replicationSlots"].is_null());
        assert!(params["sync_replication_slots"].is_null());
        assert!(params["hot_standby_feedback"].is_null());
        assert!(params["logical_decoding_work_mem"].is_null());
    }

    #[test]
    fn multi_instance_cluster_gets_the_failover_slot_sync_config() {
        // prod (instances 3): failover-slot continuity — HA slot sync + the GUCs
        // that keep a synced logical slot alive across switchover (D19 v3 §4).
        let set = render_org_cluster_set(&Org::dedicated("acme"), &EnvPolicy::defaults()).unwrap();
        let prod = cluster_named(&set, "acme-prod");
        let ha = &prod["spec"]["replicationSlots"]["highAvailability"];
        assert_eq!(ha["enabled"], true);
        assert_eq!(ha["synchronizeLogicalDecoding"], true);
        let params = &prod["spec"]["postgresql"]["parameters"];
        assert_eq!(params["max_slot_wal_keep_size"], "1GB");
        assert_eq!(params["sync_replication_slots"], "on");
        assert_eq!(params["hot_standby_feedback"], "on");
        assert_eq!(params["logical_decoding_work_mem"], "16MB");
    }

    #[test]
    fn all_clusters_have_no_cpu_limit_and_superuser_access() {
        let set = render_org_cluster_set(&Org::dedicated("acme"), &EnvPolicy::defaults()).unwrap();
        for c in &set.clusters {
            // Requests only — NO limits (the S2 CFS lesson).
            assert_eq!(c["spec"]["resources"]["requests"]["cpu"], "200m");
            assert!(
                c["spec"]["resources"]["limits"].is_null(),
                "the DB-serving path must not be CFS-throttled"
            );
            // WAL/PITR via .spec.plugins, never the deprecated in-tree stanza.
            assert!(c["spec"]["backup"].is_null());
            assert_eq!(c["spec"]["enableSuperuserAccess"], true);
            assert_eq!(
                c["spec"]["postgresql"]["pg_hba"][0],
                "host all all all scram-sha-256"
            );
        }
    }

    #[test]
    fn backup_enabled_clusters_carry_the_plugin_and_get_backup_crs() {
        // Default set: prod is backed (has a cadence), dev is not.
        let set = render_org_cluster_set(&Org::dedicated("acme"), &EnvPolicy::defaults()).unwrap();
        let prod = cluster_named(&set, "acme-prod");
        assert_eq!(
            prod["spec"]["plugins"][0]["name"],
            "barman-cloud.cloudnative-pg.io"
        );
        assert_eq!(
            prod["spec"]["plugins"][0]["parameters"]["barmanObjectName"],
            "acme-prod-store"
        );
        assert!(cluster_named(&set, "acme-dev")["spec"]["plugins"].is_null());
        // One ObjectStore + one ScheduledBackup — for prod only.
        assert_eq!(set.object_stores.len(), 1);
        assert_eq!(set.scheduled_backups.len(), 1);
        assert_eq!(set.object_stores[0]["metadata"]["name"], "acme-prod-store");
        assert_eq!(set.object_stores[0]["spec"]["retentionPolicy"], "14d");
        assert_eq!(
            set.scheduled_backups[0]["spec"]["cluster"]["name"],
            "acme-prod"
        );
    }

    #[test]
    fn a_pooled_org_renders_no_clusters() {
        let err = render_org_cluster_set(&Org::pooled("try", "wamn-pg"), &EnvPolicy::defaults())
            .unwrap_err();
        assert!(matches!(
            err,
            ProvisionError::OrgIsPooled { pool } if pool == "wamn-pg"
        ));
    }
}

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
///   nodes so a node loss drops at most one.
/// * **hibernation-eligible** (`policy.hibernation == "eligible"`): carries the
///   `cnpg.io/hibernation` annotation set `off` (opted into the lifecycle but
///   running, so it comes up ready at provision; the platform off-hours scheduler
///   may flip it `on`, roughly halving idle cost). Independent of instance count.
///
/// All: `enableSuperuserAccess` (the wamn-q3n.7 per-project-env path connects as
/// superuser to CREATE the databases/roles), a non-TLS `pg_hba` (the repo connects
/// `NoTls`), and **NO cpu limit** — requests only (the S2 CFS lesson).
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
        "postgresql": { "pg_hba": ["host all all all scram-sha-256"] },
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

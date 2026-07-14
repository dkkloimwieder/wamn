//! Rendering an org's CNPG `Cluster` SET (wamn-q3n.6, extended for T4 by
//! wamn-q3n.14).
//!
//! `provision-org` renders the Postgres clusters a paying org (T2 `standard` / T4
//! `dedicated`) is placed on:
//!
//! * **`<org>-prod`** — HA per tier, holds every project's `prod` env (and, for a
//!   standard org, `canary`, which shares prod's failure domain);
//! * **`<org>-canary`** — a **dedicated** (T4) org ONLY: canary's own HA cluster
//!   (a third recovery domain with independent PITR, wamn-q3n.14). Absent for a
//!   standard org, where canary shares `<org>-prod`;
//! * **`<org>-dev`** — a single, hibernation-managed instance, holds `dev` and
//!   preview/scratch envs (its own recovery domain — dev never rewinds prod).
//!
//! Rendered as `serde_json::Value` CNPG `Cluster` custom resources (`kubectl
//! apply -f` accepts JSON — the [`render_secret_manifest`](crate::secret::render_secret_manifest)
//! precedent); the `provision-org` driver emits them and the runbook/Job applies
//! them and waits ready. This crate is pure — no K8s client.
//!
//! **Scope:** the CLUSTER SHAPE only. Per-project-env database/role creation (the
//! CNPG `Database` CRD + `.spec.managed.roles`) is wamn-q3n.7; live WAL/PITR
//! backup config (a `backup` stanza + object-store prefix) is wamn-e1g — the
//! rendered clusters deliberately carry no `backup` stanza yet.

use serde_json::{Value, json};
use wamn_registry::{Org, Tier};

use crate::error::ProvisionError;

/// The Postgres image both org clusters run (matches the T1 / pool clusters).
const IMAGE: &str = "ghcr.io/cloudnative-pg/postgresql:18";
/// The namespace org clusters live in (alongside the guardrailed clusters).
const NAMESPACE: &str = "wamn-system";

/// The number of instances in an org's `<org>-prod` cluster, by tier. A paying
/// prod cluster is **HA** (≥ 2, so a single-instance loss fails over); the
/// regulated `dedicated` tier gets a third for stronger redundancy. `trials` has
/// no dedicated set (it shares the pool) → `None`; [`render_org_cluster_set`]
/// rejects it.
pub fn prod_instances(tier: Tier) -> Option<u32> {
    match tier {
        Tier::Standard => Some(2),
        Tier::Dedicated => Some(3),
        Tier::Trials => None,
    }
}

/// The number of instances in a dedicated org's `<org>-canary` cluster: HA (2).
/// Canary is its own recovery domain, but a pre-prod validation env, so it takes
/// minimal HA rather than prod's full redundancy.
const CANARY_INSTANCES: u32 = 2;

/// The rendered CNPG `Cluster` set for a paying org: `<org>-prod` (HA) always,
/// `<org>-canary` (HA) for a **dedicated** (T4) org only, and `<org>-dev`
/// (hibernation-managed). A standard (T2) org has `canary: None` — canary shares
/// the prod cluster (the T2 recovery-domain collapse).
#[derive(Debug, Clone)]
pub struct OrgClusters {
    /// The `<org>-prod` cluster CR (HA per tier).
    pub prod: Value,
    /// The `<org>-canary` cluster CR — set only for a dedicated (T4) org.
    pub canary: Option<Value>,
    /// The `<org>-dev` cluster CR (single, hibernation-managed).
    pub dev: Value,
}

/// Render an org's CNPG `Cluster` set — `<org>-prod`, the optional `<org>-canary`
/// (**dedicated** T4 only), and `<org>-dev`. The cluster **names** come from the
/// [`Org`]'s own `ClusterRef`s (built via [`wamn_registry::cluster_name`] /
/// [`wamn_registry::canary_cluster_name`]), so the rendered clusters and the org's
/// `registry.orgs` row always name the same clusters — what
/// [`Registry::resolve`](wamn_registry::Registry::resolve) relies on.
///
/// Errors with [`ProvisionError::TierHasNoDedicatedPair`] for a `trials` org: a
/// trials org lives on the shared pool (both cluster refs point at it), not a
/// dedicated set (T3 provisioning is wamn-q3n.9).
pub fn render_org_cluster_set(org: &Org) -> Result<OrgClusters, ProvisionError> {
    let prod_instances =
        prod_instances(org.tier).ok_or(ProvisionError::TierHasNoDedicatedPair {
            tier: org.tier.as_str(),
        })?;
    let prod = render_cluster(&org.id, "prod", &org.prod_cluster.name, prod_instances);
    let dev = render_cluster(&org.id, "dev", &org.dev_cluster.name, 1);
    // A dedicated org's canary cluster — present iff `Org::canary_cluster` is set
    // (the model sets it only for the dedicated tier, wamn-q3n.14).
    let canary = org
        .canary_cluster
        .as_ref()
        .map(|c| render_cluster(&org.id, "canary", &c.name, CANARY_INSTANCES));
    Ok(OrgClusters { prod, canary, dev })
}

/// Common labels stamped on every org cluster — platform ownership + identity
/// (the org and the recovery-domain `role`: `prod` / `canary` / `dev`), so
/// tooling never parses the name.
fn cluster_labels(org: &str, role: &str) -> Value {
    json!({
        "app.kubernetes.io/managed-by": "wamn",
        "app.kubernetes.io/component": "org-cluster",
        "wamn.org": org,
        "wamn.side": role,
    })
}

/// Render one org CNPG `Cluster`, labeled with its `role` (`prod`/`canary`/`dev`).
///
/// * **HA** (`instances ≥ 2` — every `prod`, and a dedicated `canary`): pod
///   anti-affinity spreads instances across nodes so a node loss drops at most
///   one; no hibernation.
/// * **Single** (`instances == 1` — `dev`): hibernation-managed — carries the
///   `cnpg.io/hibernation` annotation set `off` (opted into the lifecycle but
///   running, so it comes up ready at provision; the platform off-hours scheduler
///   flips it `on`, roughly halving idle-dev cost).
///
/// All: `enableSuperuserAccess` (the wamn-q3n.7 per-project-env provisioning path
/// connects as superuser to CREATE the databases/roles), a non-TLS `pg_hba` (the
/// repo connects `NoTls`), and **NO cpu limit** — requests only (the S2 CFS
/// lesson: the DB-serving path must not be CFS-throttled). No `backup` stanza —
/// WAL/PITR is wamn-e1g.
fn render_cluster(org: &str, role: &str, name: &str, instances: u32) -> Value {
    let ha = instances >= 2;
    let mut metadata = json!({
        "name": name,
        "namespace": NAMESPACE,
        "labels": cluster_labels(org, role),
    });
    let mut spec = json!({
        "instances": instances,
        "imageName": IMAGE,
        "primaryUpdateStrategy": "unsupervised",
        "enableSuperuserAccess": true,
        "resources": { "requests": { "cpu": "200m", "memory": "256Mi" } },
        "storage": { "size": "2Gi" },
        "postgresql": { "pg_hba": ["host all all all scram-sha-256"] },
        // A neutral placeholder DB; the real per-project-env databases are created
        // declaratively by wamn-q3n.7 (the CNPG Database CRD). No `backup` stanza.
        "bootstrap": { "initdb": { "database": "app", "owner": "app" } },
    });
    if ha {
        spec["affinity"] = json!({
            "enablePodAntiAffinity": true,
            "topologyKey": "kubernetes.io/hostname",
            "podAntiAffinityType": "preferred",
        });
    } else {
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

    fn org(tier: Tier) -> Org {
        Org::for_pair("acme", tier)
    }

    #[test]
    fn prod_is_ha_by_tier_dev_is_single() {
        let set = render_org_cluster_set(&org(Tier::Standard)).unwrap();
        assert_eq!(set.prod["spec"]["instances"], 2);
        assert_eq!(set.dev["spec"]["instances"], 1);
        assert!(
            set.canary.is_none(),
            "a standard org has no dedicated canary"
        );
        // The regulated dedicated tier gets a third prod instance (+ a canary).
        let set = render_org_cluster_set(&org(Tier::Dedicated)).unwrap();
        assert_eq!(set.prod["spec"]["instances"], 3);
        assert_eq!(set.dev["spec"]["instances"], 1);
        // The mapping itself.
        assert_eq!(prod_instances(Tier::Standard), Some(2));
        assert_eq!(prod_instances(Tier::Dedicated), Some(3));
    }

    #[test]
    fn cluster_names_and_kind_come_from_the_org() {
        let set = render_org_cluster_set(&org(Tier::Standard)).unwrap();
        for (c, name, side) in [
            (&set.prod, "acme-prod", "prod"),
            (&set.dev, "acme-dev", "dev"),
        ] {
            assert_eq!(c["apiVersion"], "postgresql.cnpg.io/v1");
            assert_eq!(c["kind"], "Cluster");
            assert_eq!(c["metadata"]["name"], name);
            assert_eq!(c["metadata"]["namespace"], "wamn-system");
            assert_eq!(c["metadata"]["labels"]["wamn.org"], "acme");
            assert_eq!(c["metadata"]["labels"]["wamn.side"], side);
            assert_eq!(
                c["metadata"]["labels"]["app.kubernetes.io/managed-by"],
                "wamn"
            );
        }
    }

    #[test]
    fn prod_is_ha_dev_is_hibernation_managed() {
        let set = render_org_cluster_set(&org(Tier::Standard)).unwrap();
        // Prod: pod anti-affinity for HA spread; NEVER hibernated.
        assert_eq!(set.prod["spec"]["affinity"]["enablePodAntiAffinity"], true);
        assert!(
            set.prod["metadata"]["annotations"].is_null(),
            "prod is never hibernated"
        );
        // Dev: hibernation-managed (annotation present, `off` = awake), no HA affinity.
        assert_eq!(
            set.dev["metadata"]["annotations"]["cnpg.io/hibernation"],
            "off"
        );
        assert!(
            set.dev["spec"]["affinity"].is_null(),
            "dev is a single instance"
        );
    }

    /// A dedicated (T4) org renders a THIRD cluster — `<org>-canary`, HA on its
    /// own anti-affinity spread (a third recovery domain), never hibernated —
    /// while a standard org renders none (wamn-q3n.14).
    #[test]
    fn dedicated_org_renders_a_dedicated_canary_cluster() {
        let set = render_org_cluster_set(&org(Tier::Dedicated)).unwrap();
        let canary = set
            .canary
            .expect("a dedicated org renders a canary cluster");
        assert_eq!(canary["metadata"]["name"], "acme-canary");
        assert_eq!(canary["metadata"]["labels"]["wamn.side"], "canary");
        // HA: 2 instances + anti-affinity spread, never hibernated (its OWN domain).
        assert_eq!(canary["spec"]["instances"], CANARY_INSTANCES);
        assert_eq!(canary["spec"]["affinity"]["enablePodAntiAffinity"], true);
        assert!(
            canary["metadata"]["annotations"].is_null(),
            "canary is HA, not hibernated"
        );
        // A distinct cluster resource from both prod and dev.
        assert_ne!(canary["metadata"]["name"], set.prod["metadata"]["name"]);
        assert_ne!(canary["metadata"]["name"], set.dev["metadata"]["name"]);
        // A standard org renders NO canary cluster.
        assert!(
            render_org_cluster_set(&org(Tier::Standard))
                .unwrap()
                .canary
                .is_none()
        );
    }

    #[test]
    fn all_clusters_have_no_cpu_limit_no_backup_and_superuser_access() {
        // Check the full dedicated set — prod, canary, and dev.
        let set = render_org_cluster_set(&org(Tier::Dedicated)).unwrap();
        let canary = set.canary.clone().unwrap();
        for c in [&set.prod, &canary, &set.dev] {
            // Requests only — NO `limits` (the S2 CFS lesson).
            assert_eq!(c["spec"]["resources"]["requests"]["cpu"], "200m");
            assert!(
                c["spec"]["resources"]["limits"].is_null(),
                "the DB-serving path must not be CFS-throttled — no cpu/mem limit"
            );
            // No `backup` stanza yet — WAL/PITR is wamn-e1g.
            assert!(
                c["spec"]["backup"].is_null(),
                "backup config is deferred to wamn-e1g"
            );
            // The .7 per-project-env path connects as superuser.
            assert_eq!(c["spec"]["enableSuperuserAccess"], true);
            // Non-TLS pg_hba (the repo connects NoTls).
            assert_eq!(
                c["spec"]["postgresql"]["pg_hba"][0],
                "host all all all scram-sha-256"
            );
        }
    }

    #[test]
    fn trials_has_no_dedicated_pair() {
        let err = render_org_cluster_set(&org(Tier::Trials)).unwrap_err();
        assert!(matches!(
            err,
            ProvisionError::TierHasNoDedicatedPair { tier: "trials" }
        ));
        assert_eq!(prod_instances(Tier::Trials), None);
    }
}

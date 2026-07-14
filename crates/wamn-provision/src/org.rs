//! Rendering an org's CNPG `Cluster` PAIR (wamn-q3n.6).
//!
//! `provision-org` renders the two Postgres clusters a paying org (T2 `standard`
//! / T4 `dedicated`) is placed on:
//!
//! * **`<org>-prod`** — HA per tier, holds every project's `prod` env (and
//!   `canary`, which shares prod's failure domain);
//! * **`<org>-dev`** — a single, hibernation-managed instance, holds `dev` and
//!   preview/scratch envs (its own recovery domain — dev never rewinds prod).
//!
//! Rendered as `serde_json::Value` CNPG `Cluster` custom resources (`kubectl
//! apply -f` accepts JSON — the [`render_secret_manifest`](crate::secret::render_secret_manifest)
//! precedent); the `provision-org` driver emits them and the runbook/Job applies
//! them and waits ready. This crate is pure — no K8s client.
//!
//! **Scope (wamn-q3n.6):** the CLUSTER SHAPE only. Per-project-env database/role
//! creation (the CNPG `Database` CRD + `.spec.managed.roles`) is wamn-q3n.7; live
//! WAL/PITR backup config (a `backup` stanza + object-store prefix) is wamn-e1g —
//! the rendered clusters deliberately carry no `backup` stanza yet.

use serde_json::{Value, json};
use wamn_registry::{Org, Side, Tier};

use crate::error::ProvisionError;

/// The Postgres image both org clusters run (matches the T1 / pool clusters).
const IMAGE: &str = "ghcr.io/cloudnative-pg/postgresql:18";
/// The namespace org clusters live in (alongside the guardrailed clusters).
const NAMESPACE: &str = "wamn-system";

/// The number of instances in an org's `<org>-prod` cluster, by tier. A paying
/// prod cluster is **HA** (≥ 2, so a single-instance loss fails over); the
/// regulated `dedicated` tier gets a third for stronger redundancy. `trials` has
/// no dedicated pair (it shares the pool) → `None`; [`render_org_cluster_pair`]
/// rejects it.
pub fn prod_instances(tier: Tier) -> Option<u32> {
    match tier {
        Tier::Standard => Some(2),
        Tier::Dedicated => Some(3),
        Tier::Trials => None,
    }
}

/// Render an org's `(<org>-prod, <org>-dev)` CNPG `Cluster` pair. The cluster
/// **names** come from the [`Org`]'s own `ClusterRef`s (built via
/// [`wamn_registry::cluster_name`]), so the rendered clusters and the org's
/// `registry.orgs` row always name the same clusters — what
/// [`Registry::resolve`](wamn_registry::Registry::resolve) relies on.
///
/// Errors with [`ProvisionError::TierHasNoDedicatedPair`] for a `trials` org: a
/// trials org lives on the shared pool (both cluster refs point at it), not a
/// dedicated pair (T3 provisioning is wamn-q3n.9).
pub fn render_org_cluster_pair(org: &Org) -> Result<(Value, Value), ProvisionError> {
    let instances = prod_instances(org.tier).ok_or(ProvisionError::TierHasNoDedicatedPair {
        tier: org.tier.as_str(),
    })?;
    let prod = render_cluster(&org.id, Side::Prod, &org.prod_cluster.name, instances);
    let dev = render_cluster(&org.id, Side::Dev, &org.dev_cluster.name, 1);
    Ok((prod, dev))
}

/// The wire form of a [`Side`] for labels.
fn side_str(side: Side) -> &'static str {
    match side {
        Side::Prod => "prod",
        Side::Dev => "dev",
    }
}

/// Common labels stamped on both org clusters — platform ownership + identity
/// (the org and the recovery-domain side), so tooling never parses the name.
fn cluster_labels(org: &str, side: Side) -> Value {
    json!({
        "app.kubernetes.io/managed-by": "wamn",
        "app.kubernetes.io/component": "org-cluster",
        "wamn.org": org,
        "wamn.side": side_str(side),
    })
}

/// Render one org CNPG `Cluster`.
///
/// * **Prod** (`instances ≥ 2`): HA — pod anti-affinity spreads instances across
///   nodes so a node loss drops at most one; no hibernation.
/// * **Dev** (`instances == 1`): hibernation-managed — carries the
///   `cnpg.io/hibernation` annotation set `off` (opted into the lifecycle but
///   running, so it comes up ready at provision; the platform off-hours scheduler
///   flips it `on`, roughly halving the cost of two clusters per org).
///
/// Both: `enableSuperuserAccess` (the wamn-q3n.7 per-project-env provisioning
/// path connects as superuser to CREATE the databases/roles), a non-TLS `pg_hba`
/// (the repo connects `NoTls`), and **NO cpu limit** — requests only (the S2 CFS
/// lesson: the DB-serving path must not be CFS-throttled). No `backup` stanza —
/// WAL/PITR is wamn-e1g.
fn render_cluster(org: &str, side: Side, name: &str, instances: u32) -> Value {
    let mut metadata = json!({
        "name": name,
        "namespace": NAMESPACE,
        "labels": cluster_labels(org, side),
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
    if side == Side::Prod {
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
        let (prod, dev) = render_org_cluster_pair(&org(Tier::Standard)).unwrap();
        assert_eq!(prod["spec"]["instances"], 2);
        assert_eq!(dev["spec"]["instances"], 1);
        // The regulated dedicated tier gets a third prod instance.
        let (prod, dev) = render_org_cluster_pair(&org(Tier::Dedicated)).unwrap();
        assert_eq!(prod["spec"]["instances"], 3);
        assert_eq!(dev["spec"]["instances"], 1);
        // The mapping itself.
        assert_eq!(prod_instances(Tier::Standard), Some(2));
        assert_eq!(prod_instances(Tier::Dedicated), Some(3));
    }

    #[test]
    fn cluster_names_and_kind_come_from_the_org() {
        let (prod, dev) = render_org_cluster_pair(&org(Tier::Standard)).unwrap();
        for (c, name, side) in [(&prod, "acme-prod", "prod"), (&dev, "acme-dev", "dev")] {
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
        let (prod, dev) = render_org_cluster_pair(&org(Tier::Standard)).unwrap();
        // Prod: pod anti-affinity for HA spread; NEVER hibernated.
        assert_eq!(prod["spec"]["affinity"]["enablePodAntiAffinity"], true);
        assert!(
            prod["metadata"]["annotations"].is_null(),
            "prod is never hibernated"
        );
        // Dev: hibernation-managed (annotation present, `off` = awake), no HA affinity.
        assert_eq!(dev["metadata"]["annotations"]["cnpg.io/hibernation"], "off");
        assert!(
            dev["spec"]["affinity"].is_null(),
            "dev is a single instance"
        );
    }

    #[test]
    fn both_clusters_have_no_cpu_limit_no_backup_and_superuser_access() {
        let (prod, dev) = render_org_cluster_pair(&org(Tier::Standard)).unwrap();
        for c in [&prod, &dev] {
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
        let err = render_org_cluster_pair(&org(Tier::Trials)).unwrap_err();
        assert!(matches!(
            err,
            ProvisionError::TierHasNoDedicatedPair { tier: "trials" }
        ));
        assert_eq!(prod_instances(Tier::Trials), None);
    }
}

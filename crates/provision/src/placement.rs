//! Deployment placement and environment policy values.

use serde::{Deserialize, Serialize};
use wamn_registry::{Env, OrgId};

/// The CNPG image every provisioned cluster runs (the default seeded into
/// [`EnvPolicy::dev`] / [`EnvPolicy::prod`]; a per-env `image` knob can override).
pub const DEFAULT_PG_IMAGE: &str = "ghcr.io/cloudnative-pg/postgresql:18";

/// Where an env's data physically lives, relative to other envs — the
/// recovery-domain knob that (with [`Placement`]) drives [`cluster_of`]. `own` =
/// its own recovery domain (an independent cluster on a dedicated org);
/// `shared-with(x)` = it co-locates in env `x`'s recovery domain (e.g. `canary`
/// shares `prod`, reproducing the shipped T2 canary with no enum variant).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryDomain {
    /// Its own recovery domain — a dedicated org gives it its own cluster.
    Own,
    /// Co-located in the named env's recovery domain (JSON: `{"shared-with": "prod"}`).
    SharedWith(Env),
}

/// A named, self-contained environment policy — the D18 replacement for the
/// closed `Tier` sizing/backup semantics. The policy `name` **is** the env slug
/// ([`Env`]); a project-env's `env` both identifies it in the [`wamn_registry::Triple`] and
/// resolves this policy. Standalone (no inheritance). Policies are **org-targeted**
/// (wamn-8df.4): each org owns its policy set ([`OrgEnvPolicy`]), stamped from a
/// [`crate::Template`](crate::Template) and then customized per-env — this value type
/// stays org-free so templates can carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct EnvPolicy {
    /// The env slug this policy configures (its primary key).
    pub name: Env,
    /// Whether this env owns its cluster or shares another env's recovery domain.
    pub recovery_domain: RecoveryDomain,
    /// Ordering for promotion (`dev` < `prod`).
    pub promotion_rank: i32,
    /// HA / replica count for the cluster sized by this env.
    pub instances: i32,
    /// Persistent volume size (e.g. `2Gi`).
    pub storage: String,
    /// CPU request (e.g. `200m`).
    pub cpu: String,
    /// Memory request (e.g. `256Mi`).
    pub memory: String,
    /// The CNPG image (e.g. [`DEFAULT_PG_IMAGE`]).
    pub image: String,
    /// Base-backup schedule (a 6-field CNPG cron); empty = no scheduled backup
    /// (`has_scheduled_backup`).
    #[serde(default)]
    pub backup_cadence: String,
    /// PITR window (e.g. `14d`); empty = none.
    #[serde(default)]
    pub wal_retention: String,
    /// Hibernation posture: `eligible` (the dev cost lever — annotate the cluster
    /// so the off-hours scheduler may hibernate it) or `off` (never).
    #[serde(default)]
    pub hibernation: String,
}

impl EnvPolicy {
    /// The recovery-domain **owner** env whose cluster holds this env's data:
    /// itself when `own`, else its `shared-with` target. The `{org}-{owner}` name
    /// component for a dedicated org ([`cluster_of`]).
    pub fn owner(&self) -> &Env {
        match &self.recovery_domain {
            RecoveryDomain::Own => &self.name,
            RecoveryDomain::SharedWith(target) => target,
        }
    }

    /// Whether this env is eligible to be hibernated (the dev cost lever).
    pub fn hibernation_eligible(&self) -> bool {
        self.hibernation == "eligible"
    }

    /// Whether this env has a scheduled base backup (a non-empty cadence).
    pub fn has_scheduled_backup(&self) -> bool {
        !self.backup_cadence.is_empty()
    }

    /// The seeded default `dev` policy — its own recovery domain, single instance,
    /// hibernation-eligible, no scheduled backup. The single source both
    /// `deploy/sql/system-schema.sql` (the seed, drift-guarded) and tests share.
    pub fn dev() -> EnvPolicy {
        EnvPolicy {
            name: Env::new("dev"),
            recovery_domain: RecoveryDomain::Own,
            promotion_rank: 10,
            instances: 1,
            storage: "2Gi".into(),
            cpu: "200m".into(),
            memory: "256Mi".into(),
            image: DEFAULT_PG_IMAGE.into(),
            backup_cadence: String::new(),
            wal_retention: String::new(),
            hibernation: "eligible".into(),
        }
    }

    /// The seeded default `prod` policy — its own recovery domain, HA (3
    /// instances), a 6-hourly base backup + 14-day PITR window, never hibernated.
    pub fn prod() -> EnvPolicy {
        EnvPolicy {
            name: Env::new("prod"),
            recovery_domain: RecoveryDomain::Own,
            promotion_rank: 30,
            instances: 3,
            storage: "2Gi".into(),
            cpu: "200m".into(),
            memory: "256Mi".into(),
            image: DEFAULT_PG_IMAGE.into(),
            backup_cadence: "0 0 */6 * * *".into(),
            wal_retention: "14d".into(),
            hibernation: "off".into(),
        }
    }

    /// The base `dev` + `prod` policy pair every shipped
    /// [`crate::Template`](crate::Template) builds on. `canary` and others are added as
    /// data (template policies or per-org rows), not built in.
    pub fn defaults() -> Vec<EnvPolicy> {
        vec![EnvPolicy::dev(), EnvPolicy::prod()]
    }
}

/// One org's copy of an [`EnvPolicy`] — the wamn-8df.4 org-scoping. Policies are
/// per-org rows (PK `(org, name)` in storage): a [`crate::Template`](crate::Template)
/// stamps an org's initial set at provision time, and the org customizes its own
/// rows without touching any other org's. The nested shape (org + org-free policy
/// value) mirrors [`wamn_registry::Triple`]'s triple nesting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct OrgEnvPolicy {
    /// The org this policy row belongs to.
    pub org: OrgId,
    /// The policy value (its `name` is the env slug, unique per org).
    pub policy: EnvPolicy,
}

/// How an org's databases are placed — the minimal descriptor replacing the
/// closed `Tier`. It couples placement to nothing but "shared pool vs. own
/// clusters"; sizing / HA / backup are [`EnvPolicy`] knobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Placement {
    /// Every env shares the given pool cluster (the T3-style shared pool).
    Pooled { pool: String },
    /// The org owns one cluster per recovery domain (`<org>-<owner(env)>`).
    Dedicated,
}

impl Placement {
    /// The `placement_type` storage literal (`pooled` / `dedicated`).
    pub fn type_str(&self) -> &'static str {
        match self {
            Placement::Pooled { .. } => "pooled",
            Placement::Dedicated => "dedicated",
        }
    }

    /// The pool cluster name, iff pooled.
    pub fn pool(&self) -> Option<&str> {
        match self {
            Placement::Pooled { pool } => Some(pool),
            Placement::Dedicated => None,
        }
    }
}

/// A reference to a CNPG `Cluster` that holds project-env databases. A name, not
/// the cluster itself — the registry records placement, not infrastructure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ClusterRef {
    /// The CNPG `Cluster` resource name (e.g. `acme-prod`, `wamn-pg`).
    pub name: String,
}

impl ClusterRef {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// The CNPG `Cluster` name holding an `(org, env)` database — the **one rule**
/// (D18) replacing `cluster_name` / `canary_cluster_name` / `Env::side` /
/// `Org::for_pair` / `Org::for_pool` / `Org::cluster_for_env`:
///
/// - a **pooled** org places every env on its pool cluster;
/// - a **dedicated** org owns one cluster per recovery domain,
///   `<org>-<owner(env_policy)>` — so `prod`(own) → `<org>-prod`, `dev`(own) →
///   `<org>-dev`, `canary` shared-with `prod` → `<org>-prod` (the T2 collapse),
///   `canary`(own) → `<org>-canary` (the T4 third recovery domain).
///
/// This is the single source both the cluster-CR renderer (`wamn-control-provision`) and
/// the provisioning flow derive cluster names from, so
/// a provisioned cluster and a resolved triple always agree.
pub fn cluster_of(org: &Org, policy: &EnvPolicy) -> ClusterRef {
    match &org.placement {
        Placement::Pooled { pool } => ClusterRef::new(pool.clone()),
        Placement::Dedicated => ClusterRef::new(wamn_registry::name::cluster(
            &org.id,
            policy.owner().as_str(),
        )),
    }
}

/// An organization: the unit of isolation and billing. Carries only its id and a
/// minimal [`Placement`] — the clusters holding its databases are **derived**
/// ([`cluster_of`]) from the placement plus each env's [`EnvPolicy`], not stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Org {
    pub id: OrgId,
    pub placement: Placement,
}

impl Org {
    /// An org that shares the given pool cluster (every env lands on it).
    pub fn pooled(id: impl Into<String>, pool: impl Into<String>) -> Org {
        Org {
            id: id.into(),
            placement: Placement::Pooled { pool: pool.into() },
        }
    }

    /// An org that owns per-recovery-domain clusters (`<org>-<owner(env)>`,
    /// derived by [`cluster_of`]).
    pub fn dedicated(id: impl Into<String>) -> Org {
        Org {
            id: id.into(),
            placement: Placement::Dedicated,
        }
    }
}

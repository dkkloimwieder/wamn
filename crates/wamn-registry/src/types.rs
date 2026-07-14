//! Canonical control-plane registry types (wamn-q3n.1).
//!
//! The registry is the platform's system-of-record for **identity** and
//! **placement**: which organizations exist, the projects within them, and the
//! provisioned `(org, project, env)` databases — each mapped to the CNPG
//! `Cluster` that holds it and a **reference** to the K8s Secret that
//! credentials it. It stores no credentials (R8b) and no tenant data; it lives
//! on the T1 system cluster (`docs/postgres-topology.md` §T1).
//!
//! [`Triple`] is the first-class control-plane identity every subsystem speaks —
//! provisioning, subdomain routing, dispatcher registration, and promotion
//! tooling key off it so nothing parses names.

use serde::{Deserialize, Serialize};

/// Schema-format version. Additive-within-major per the `0.1.x` freeze rule
/// (checked by [`crate::validate`]); this is a store model, not a published
/// JSON-Schema contract, so there is no generated schema file to keep in sync.
pub const SCHEMA_VERSION: &str = "0.1";

/// An organization id — a lowercase slug. It embeds into cluster / Secret /
/// subdomain names, so it follows the platform slug discipline (see
/// [`crate::validate`], mirroring `wamn-provision` / wi4 / 66x).
pub type OrgId = String;

/// A project id — a lowercase slug, unique within its org.
pub type ProjectId = String;

/// The hosting tier an org is placed on (`docs/postgres-topology.md`). The T1
/// system cluster, which holds *this* registry, is not an org tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tier {
    /// T3 — the shared trials pool (pre-contract: trials, demos, hobby). The
    /// RLS floor is load-bearing here.
    Trials,
    /// T2 — a dedicated prod/dev cluster pair (the standard paying tier).
    Standard,
    /// T4 — cluster-per-environment (the regulated tier). A dedicated org gives
    /// `canary` its OWN cluster (`<org>-canary`, [`Org::canary_cluster`]) — a
    /// third recovery domain with independent PITR, the §T4 maximal-separation
    /// property (wamn-q3n.14). `prod` and `dev` follow the T2 pair shape.
    Dedicated,
}

impl Tier {
    /// Every tier. The order is presentational (ascending isolation).
    pub const ALL: [Tier; 3] = [Tier::Trials, Tier::Standard, Tier::Dedicated];

    /// The wire / identifier form (`trials` / `standard` / `dedicated`) — matches
    /// the serde representation and the `tier` CHECK literals in the system-DB
    /// schema (`deploy/system-schema.sql`, tied by a drift guard).
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Trials => "trials",
            Tier::Standard => "standard",
            Tier::Dedicated => "dedicated",
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A first-class environment. The default set is closed — `dev`, `canary`,
/// `prod` (`docs/postgres-topology.md` §Environments). `canary` is prod-shaped
/// validation that deliberately shares prod's failure domain; `dev` has its own
/// recovery domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Env {
    Dev,
    Canary,
    Prod,
}

impl Env {
    /// Every environment, in dev → canary → prod (promotion) order.
    pub const ALL: [Env; 3] = [Env::Dev, Env::Canary, Env::Prod];

    /// The recovery-domain [`Side`] this env resolves to within a T2 org pair.
    /// `canary` and `prod` are prod-side (shared failure domain); `dev` is
    /// dev-side (its own recovery domain — "dev never rewinds prod").
    pub fn side(self) -> Side {
        match self {
            Env::Dev => Side::Dev,
            Env::Canary | Env::Prod => Side::Prod,
        }
    }

    /// The wire / identifier form (`dev` / `canary` / `prod`).
    pub fn as_str(self) -> &'static str {
        match self {
            Env::Dev => "dev",
            Env::Canary => "canary",
            Env::Prod => "prod",
        }
    }
}

impl std::fmt::Display for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The recovery-domain side of an environment within a T2 org cluster pair.
/// Determines which of an org's two clusters holds the database (collapsed to a
/// single pool cluster for a T3 trials org).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The `<org>-prod` cluster (holds `prod` and `canary`).
    Prod,
    /// The `<org>-dev` cluster (holds `dev`).
    Dev,
}

/// The CNPG `Cluster` name holding a paying org's databases on `side`:
/// `<org>-prod` (prod + canary) or `<org>-dev` (dev). This is the **single
/// source** both the cluster-CR renderer (`wamn-provision`, wamn-q3n.6) and the
/// org's `registry.orgs` row derive their cluster names from, so a provisioned
/// pair and its registry row always name the same clusters — what
/// [`Registry::resolve`](crate::Registry::resolve) relies on.
pub fn cluster_name(org: &str, side: Side) -> String {
    match side {
        Side::Prod => format!("{org}-prod"),
        Side::Dev => format!("{org}-dev"),
    }
}

/// The CNPG `Cluster` name holding a **dedicated** (T4) org's `canary` env:
/// `<org>-canary`. Unlike a standard (T2) org — where `canary` shares
/// `<org>-prod` — a dedicated org gives `canary` its own recovery domain and
/// independent PITR (wamn-q3n.14). The single source [`Org::for_pair`], the
/// cluster-CR renderer, and the `registry.orgs` row derive the canary name from
/// (a sibling of [`cluster_name`]); `canary` is deliberately not a [`Side`], since
/// [`Env::side`] is the *T2 pair* collapse (canary → prod) and per-env dedicated
/// placement is resolved by [`Org::cluster_for_env`] instead.
pub fn canary_cluster_name(org: &str) -> String {
    format!("{org}-canary")
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

/// A **reference** to the K8s Secret credentialing a project-env database —
/// never the credential itself (R8b: the registry stores references; actual
/// material lives in Secrets resolved by components holding the matching RBAC).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct SecretRef {
    /// The Secret name (e.g. `wamn-db-<project>`, the 5x0.1 lookup key).
    pub name: String,
    /// The Secret's namespace, if not the resolving component's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

impl SecretRef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            namespace: None,
        }
    }
}

/// The control-plane identity triple `(org, project, env)` — the key every
/// subsystem speaks (registry rows, provisioning, subdomain routing, dispatcher
/// registration, promotion tooling). Tooling keys off the triple rather than
/// parsing a provisioned name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Triple {
    pub org: OrgId,
    pub project: ProjectId,
    pub env: Env,
}

impl Triple {
    pub fn new(org: impl Into<String>, project: impl Into<String>, env: Env) -> Self {
        Self {
            org: org.into(),
            project: project.into(),
            env,
        }
    }

    /// The routing host label for this identity: `<project>--<env>.<org>`. The
    /// caller appends the platform base domain (e.g. `.wamn.example`). Derived
    /// wholly from the triple — routing never parses a provisioned name.
    pub fn host_label(&self) -> String {
        format!("{}--{}.{}", self.project, self.env, self.org)
    }
}

impl std::fmt::Display for Triple {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}/{}", self.org, self.project, self.env)
    }
}

/// An organization: the unit of isolation and billing. Placed on a [`Tier`],
/// with references to the CNPG cluster(s) that hold its project-env databases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Org {
    pub id: OrgId,
    pub tier: Tier,
    /// The cluster holding prod-side envs (`prod`, and `canary` unless the org is
    /// dedicated). For a T3 trials org this is the shared pool; for T2/T4 it is
    /// `<org>-prod`.
    pub prod_cluster: ClusterRef,
    /// The cluster holding the `canary` env — set **only** for a dedicated (T4)
    /// org, where canary is its own recovery domain (`<org>-canary`, independent
    /// PITR, wamn-q3n.14). `None` for standard/trials orgs, where canary shares
    /// [`prod_cluster`](Org::prod_cluster) (the T2 collapse). Serde-omitted when
    /// `None`, so T2/T3 rows round-trip unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canary_cluster: Option<ClusterRef>,
    /// The cluster holding dev-side envs (`dev`). For a T3 trials org this is
    /// the shared pool (same as `prod_cluster`); for T2/T4 it is `<org>-dev`.
    pub dev_cluster: ClusterRef,
}

impl Org {
    /// A paying (T2 standard / T4 dedicated) org placed on its own
    /// `<org>-prod` / `<org>-dev` cluster pair, with the cluster refs derived from
    /// [`cluster_name`] — the single source the CR renderer (wamn-q3n.6) and this
    /// row agree on. A `trials` org instead shares the pool (both refs point at
    /// it); it is not provisioned as a pair (T3 provisioning is wamn-q3n.9).
    pub fn for_pair(id: impl Into<String>, tier: Tier) -> Org {
        let id = id.into();
        // A dedicated (T4) org gives `canary` its OWN cluster (a per-env recovery
        // domain with independent PITR, wamn-q3n.14); a standard (T2) org shares
        // canary with prod, so `canary_cluster` is None (the T2 collapse).
        let canary_cluster =
            matches!(tier, Tier::Dedicated).then(|| ClusterRef::new(canary_cluster_name(&id)));
        Org {
            prod_cluster: ClusterRef::new(cluster_name(&id, Side::Prod)),
            canary_cluster,
            dev_cluster: ClusterRef::new(cluster_name(&id, Side::Dev)),
            id,
            tier,
        }
    }

    /// A T3 `trials` org placed on the shared `pool` cluster: it has no dedicated
    /// `<org>-prod` / `<org>-dev` pair (the pool already exists), so **both**
    /// cluster refs point at the pool and every env's [`Side`] collapses onto it.
    /// The `for_pair` counterpart for the pool tier — the placement
    /// [`provision-project-env`](crate) reads to route a trials project-env onto
    /// the pool via `env.side()`. The recovery-domain invariant
    /// (`tier='trials' OR prod_cluster<>dev_cluster`, [`crate::validate`]) admits
    /// this `prod == dev` collapse for `trials` only.
    pub fn for_pool(id: impl Into<String>, pool: impl Into<String>) -> Org {
        let pool = ClusterRef::new(pool);
        Org {
            id: id.into(),
            tier: Tier::Trials,
            prod_cluster: pool.clone(),
            canary_cluster: None,
            dev_cluster: pool,
        }
    }

    /// The cluster holding this org's `env` database. `dev` → the dev cluster,
    /// `prod` → the prod cluster; `canary` → its own cluster on a dedicated (T4)
    /// org ([`canary_cluster`](Org::canary_cluster)), falling back to the prod
    /// cluster on a standard/trials org (the T2 recovery-domain collapse, where
    /// `canary_cluster` is `None`).
    ///
    /// This is the per-env resolution [`Registry::resolve`](crate::Registry::resolve)
    /// uses — it **supersedes** routing by [`Env::side`] (the T2-pair collapse),
    /// which cannot express a dedicated org's third (canary) recovery domain.
    pub fn cluster_for_env(&self, env: Env) -> &ClusterRef {
        match env {
            Env::Dev => &self.dev_cluster,
            Env::Prod => &self.prod_cluster,
            Env::Canary => self.canary_cluster.as_ref().unwrap_or(&self.prod_cluster),
        }
    }
}

/// A project within an org.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Project {
    pub org: OrgId,
    pub id: ProjectId,
}

/// A provisioned `(org, project, env)` database: the [`Triple`] plus a reference
/// to its credential Secret. The registry's leaf row — what a triple resolves
/// to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ProjectEnv {
    pub triple: Triple,
    pub db_secret: SecretRef,
}

/// The whole control-plane registry: org / project / project-env membership plus
/// placement. Import/export via [`Registry::from_json`] / [`Registry::to_json`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Registry {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orgs: Vec<Org>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<Project>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_envs: Vec<ProjectEnv>,
}

impl Registry {
    /// An empty registry at the current [`SCHEMA_VERSION`].
    pub fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            orgs: Vec::new(),
            projects: Vec::new(),
            project_envs: Vec::new(),
        }
    }

    /// Parse a registry from JSON (import).
    pub fn from_json(s: &str) -> serde_json::Result<Registry> {
        serde_json::from_str(s)
    }

    /// Serialize to canonical pretty JSON (export). Default-empty collections are
    /// omitted, so a minimal registry round-trips minimally.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("registry serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::{Env, Tier};

    /// A paying org's clusters are `<org>-prod` / `<org>-dev`, and `Org::for_pair`
    /// stamps exactly those refs — the single source the CR renderer and the
    /// `registry.orgs` row share (so `resolve` finds the provisioned cluster).
    #[test]
    fn cluster_names_follow_the_org_pair_shape() {
        use super::{Org, Side, cluster_name};
        assert_eq!(cluster_name("acme", Side::Prod), "acme-prod");
        assert_eq!(cluster_name("acme", Side::Dev), "acme-dev");
        let org = Org::for_pair("acme", Tier::Standard);
        assert_eq!(org.id, "acme");
        assert_eq!(org.prod_cluster.name, "acme-prod");
        assert_eq!(org.dev_cluster.name, "acme-dev");
        // A standard (T2) org has NO dedicated canary cluster (canary shares prod).
        assert_eq!(org.canary_cluster, None);
        // prod/canary route to the prod cluster; dev to the dev cluster.
        assert_eq!(org.cluster_for_env(Env::Prod).name, "acme-prod");
        assert_eq!(org.cluster_for_env(Env::Canary).name, "acme-prod");
        assert_eq!(org.cluster_for_env(Env::Dev).name, "acme-dev");
    }

    /// A dedicated (T4) org gives `canary` its OWN cluster (`<org>-canary`) — a
    /// third recovery domain — so `cluster_for_env(Canary)` routes to it, NOT to
    /// prod (the §T4 maximal-separation property, wamn-q3n.14). `Env::side` still
    /// collapses canary onto prod (the T2-pair concept), which is exactly why
    /// resolution goes through `cluster_for_env` rather than `Env::side`.
    #[test]
    fn dedicated_org_gives_canary_its_own_cluster() {
        use super::{Org, Side, canary_cluster_name};
        assert_eq!(canary_cluster_name("acme"), "acme-canary");
        let org = Org::for_pair("acme", Tier::Dedicated);
        assert_eq!(
            org.canary_cluster.as_ref().map(|c| c.name.as_str()),
            Some("acme-canary")
        );
        assert_eq!(org.cluster_for_env(Env::Prod).name, "acme-prod");
        assert_eq!(org.cluster_for_env(Env::Canary).name, "acme-canary");
        assert_eq!(org.cluster_for_env(Env::Dev).name, "acme-dev");
        // Canary is its OWN recovery domain: distinct from both prod and dev.
        let canary = org.cluster_for_env(Env::Canary).name.clone();
        assert_ne!(canary, org.prod_cluster.name);
        assert_ne!(canary, org.dev_cluster.name);
        // Env::side is unchanged (the T2-pair collapse) — canary is prod-side.
        assert_eq!(Env::Canary.side(), Side::Prod);
    }

    /// A T3 trials org lives on the shared pool: `Org::for_pool` points **both**
    /// cluster refs at the pool (so every env's side collapses onto it), and the
    /// recovery-domain invariant admits `prod == dev` for the trials tier. This is
    /// the placement `provision-project-env` reads to route a trials project-env
    /// onto the pool.
    #[test]
    fn for_pool_places_a_trials_org_on_the_shared_pool() {
        use super::{Org, Registry, SCHEMA_VERSION};
        let org = Org::for_pool("acme", "wamn-pg");
        assert_eq!(org.id, "acme");
        assert_eq!(org.tier, Tier::Trials);
        // Both refs = the pool; every env's side resolves to it.
        assert_eq!(org.prod_cluster.name, "wamn-pg");
        assert_eq!(org.dev_cluster.name, "wamn-pg");
        assert_eq!(org.canary_cluster, None);
        assert_eq!(org.cluster_for_env(Env::Prod).name, "wamn-pg");
        assert_eq!(org.cluster_for_env(Env::Canary).name, "wamn-pg");
        assert_eq!(org.cluster_for_env(Env::Dev).name, "wamn-pg");
        // A one-org registry validates: invariant 4 admits prod==dev for trials.
        let reg = Registry {
            schema_version: SCHEMA_VERSION.to_string(),
            orgs: vec![org],
            projects: Vec::new(),
            project_envs: Vec::new(),
        };
        assert!(reg.validate().is_ok());
    }

    /// `as_str()` must equal the serde wire form for every variant: the system-DB
    /// `tier` / `env` CHECK literals are drift-guarded against `as_str()`, and a
    /// row is written from the serde value — the two must be the same string.
    #[test]
    fn as_str_matches_the_serde_wire_form() {
        for t in Tier::ALL {
            assert_eq!(
                serde_json::to_string(&t).unwrap(),
                format!("\"{}\"", t.as_str())
            );
        }
        for e in Env::ALL {
            assert_eq!(
                serde_json::to_string(&e).unwrap(),
                format!("\"{}\"", e.as_str())
            );
        }
    }
}

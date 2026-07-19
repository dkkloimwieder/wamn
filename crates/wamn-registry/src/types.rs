//! Canonical control-plane registry types (wamn-q3n.1; generalized in wamn-8df.3).
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
//!
//! ## The generic deployment model (D18, `docs/deployment-model.md`)
//!
//! The closed `Env` / `Tier` enums are gone. `env` is a validated [`Env`] slug
//! (the default set `dev`/`prod` is **data** — rows in [`EnvPolicy`] — not a
//! type; `canary` and others are addable policies). An org carries a minimal
//! [`Placement`] descriptor (`pooled` | `dedicated`), and the concrete cluster
//! holding a project-env is **derived** by one rule, [`cluster_of`], from the
//! placement plus the env policy's recovery domain — replacing the old
//! `Env::side` / `cluster_name` / `canary_cluster` special-casing.

use serde::{Deserialize, Serialize};

/// Schema-format version. Additive-within-major per the `0.1.x` freeze rule
/// (checked by [`crate::validate`]); this is a store model, not a published
/// JSON-Schema contract, so there is no generated schema file to keep in sync.
pub const SCHEMA_VERSION: &str = "0.1";

/// The CNPG image every provisioned cluster runs (the default seeded into
/// [`EnvPolicy::dev`] / [`EnvPolicy::prod`]; a per-env `image` knob can override).
pub const DEFAULT_PG_IMAGE: &str = "ghcr.io/cloudnative-pg/postgresql:18";

/// An organization id — a lowercase slug. It embeds into cluster / Secret /
/// subdomain names, so it follows the platform slug discipline (see
/// [`crate::validate`], mirroring `wamn-provision` / wi4 / 66x).
pub type OrgId = String;

/// A project id — a lowercase slug, unique within its org.
pub type ProjectId = String;

/// A validated environment slug — the D18 generic env model. `env` is **data,
/// not a closed type**: the default set is `dev` / `prod` (rows in
/// [`EnvPolicy`]), with `canary` and any others addable as policies. A slug both
/// names a project-env in the [`Triple`] and resolves its [`EnvPolicy`] (by
/// name). A serde-transparent `String` newtype (the `OrgId`-style discipline);
/// well-formedness is checked in [`crate::validate`], not the type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Env(String);

impl Env {
    /// Wrap a slug. Validation (lowercase slug, names a known policy) is
    /// [`crate::validate`]'s job — this is a plain constructor.
    pub fn new(s: impl Into<String>) -> Self {
        Env(s.into())
    }

    /// The slug string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Env {
    fn from(s: &str) -> Self {
        Env(s.to_string())
    }
}

impl From<String> for Env {
    fn from(s: String) -> Self {
        Env(s)
    }
}

impl std::ops::Deref for Env {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Env {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for Env {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Env {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

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
/// ([`Env`]); a project-env's `env` both identifies it in the [`Triple`] and
/// resolves this policy. Standalone (no inheritance). Policies are **org-scoped**
/// (wamn-8df.4): each org owns its policy set ([`OrgEnvPolicy`]), stamped from a
/// [`Template`](crate::Template) and then customized per-env — this value type
/// stays org-free so templates can carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct EnvPolicy {
    /// The env slug this policy configures (its primary key).
    pub name: Env,
    /// Whether this env owns its cluster or shares another env's recovery domain.
    pub recovery_domain: RecoveryDomain,
    /// Ordering for promotion (`dev` < `prod`); the `promote` env-order check
    /// reads this instead of the retired `Env::ALL`.
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
    /// `deploy/system-schema.sql` (the seed, drift-guarded) and tests share.
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
    /// [`Template`](crate::Template) builds on. `canary` and others are added as
    /// data (template policies or per-org rows), not built in.
    pub fn defaults() -> Vec<EnvPolicy> {
        vec![EnvPolicy::dev(), EnvPolicy::prod()]
    }
}

/// One org's copy of an [`EnvPolicy`] — the wamn-8df.4 org-scoping. Policies are
/// per-org rows (PK `(org, name)` in storage): a [`Template`](crate::Template)
/// stamps an org's initial set at provision time, and the org customizes its own
/// rows without touching any other org's. The nested shape (org + org-free policy
/// value) mirrors [`ProjectEnv`]'s triple nesting.
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
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Placement {
    /// Every env shares the given pool cluster (the T3-style shared pool).
    Pooled { pool: String },
    /// The org owns one cluster per recovery domain (`<org>-<owner(env)>`).
    Dedicated,
}

impl Placement {
    /// The `placement_kind` storage literal (`pooled` / `dedicated`).
    pub fn kind_str(&self) -> &'static str {
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
/// This is the single source both the cluster-CR renderer (`wamn-provision`) and
/// [`Registry::resolve`](crate::Registry::resolve) derive cluster names from, so
/// a provisioned cluster and a resolved triple always agree.
pub fn cluster_of(org: &Org, policy: &EnvPolicy) -> ClusterRef {
    match &org.placement {
        Placement::Pooled { pool } => ClusterRef::new(pool.clone()),
        Placement::Dedicated => ClusterRef::new(format!("{}-{}", org.id, policy.owner())),
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
    pub fn new(org: impl Into<String>, project: impl Into<String>, env: impl Into<Env>) -> Self {
        Self {
            org: org.into(),
            project: project.into(),
            env: env.into(),
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

/// A registered **CDC event reader** for a provisioned project-env (D19 v3,
/// wamn-l5i9.9): which publication + failover replication slot it streams from,
/// which JetStream stream its envelopes land in (`EVT_<org>_<env>` by default),
/// and a [`SecretRef`] to its **replication** credential — a reference, never
/// the material (R8b; the replication credential is its own tier, above the
/// `wamn_app` query credential and the dispatch role).
///
/// Deliberately a light row model like [`ProjectEnv`] — not folded into
/// [`Registry`] validation; the reader service (l5i9.10) deserializes its
/// registration to learn what to stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct EventReader {
    pub triple: Triple,
    pub publication: String,
    pub slot: String,
    pub stream: String,
    pub replication_secret: SecretRef,
    pub enabled: bool,
}

/// The whole control-plane registry: the per-org [`OrgEnvPolicy`] rows plus org /
/// project / project-env membership and placement. Import/export via
/// [`Registry::from_json`] / [`Registry::to_json`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Registry {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_policies: Vec<OrgEnvPolicy>,
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
            env_policies: Vec::new(),
            orgs: Vec::new(),
            projects: Vec::new(),
            project_envs: Vec::new(),
        }
    }

    /// The policy named `name` in org `org`'s set, if defined (policies are
    /// org-scoped — wamn-8df.4).
    pub fn env_policy(&self, org: &str, name: &Env) -> Option<&EnvPolicy> {
        self.env_policies
            .iter()
            .find(|p| p.org == org && &p.policy.name == name)
            .map(|p| &p.policy)
    }

    /// Org `org`'s whole policy set, in declaration order.
    pub fn org_env_policies(&self, org: &str) -> Vec<&EnvPolicy> {
        self.env_policies
            .iter()
            .filter(|p| p.org == org)
            .map(|p| &p.policy)
            .collect()
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
    use super::{
        ClusterRef, Env, EnvPolicy, Org, OrgEnvPolicy, RecoveryDomain, Registry, SCHEMA_VERSION,
        cluster_of,
    };

    /// `cluster_of` reproduces the shipped naming for a dedicated org: `prod`(own)
    /// → `<org>-prod`, `dev`(own) → `<org>-dev`, `canary` shared-with `prod` →
    /// `<org>-prod` (T2), `canary`(own) → `<org>-canary` (T4) — the one rule that
    /// replaced `cluster_name` / `canary_cluster` / `Env::side` / `for_pair`.
    #[test]
    fn cluster_of_derives_dedicated_clusters_per_recovery_domain() {
        let org = Org::dedicated("acme");
        let dev = EnvPolicy::dev();
        let prod = EnvPolicy::prod();
        assert_eq!(cluster_of(&org, &dev).name, "acme-dev");
        assert_eq!(cluster_of(&org, &prod).name, "acme-prod");

        // canary sharing prod's recovery domain → the prod cluster (T2 collapse).
        let canary_shared = EnvPolicy {
            recovery_domain: RecoveryDomain::SharedWith(Env::new("prod")),
            ..EnvPolicy {
                name: Env::new("canary"),
                promotion_rank: 20,
                ..EnvPolicy::prod()
            }
        };
        assert_eq!(canary_shared.owner(), &Env::new("prod"));
        assert_eq!(cluster_of(&org, &canary_shared).name, "acme-prod");

        // canary with its OWN recovery domain → its own cluster (the T4 property).
        let canary_own = EnvPolicy {
            name: Env::new("canary"),
            recovery_domain: RecoveryDomain::Own,
            ..EnvPolicy::prod()
        };
        assert_eq!(cluster_of(&org, &canary_own).name, "acme-canary");
    }

    /// A pooled org places every env on its pool, regardless of the env policy —
    /// the T3 collapse.
    #[test]
    fn cluster_of_collapses_a_pooled_org_onto_its_pool() {
        let org = Org::pooled("try", "wamn-pg");
        assert_eq!(org.placement.kind_str(), "pooled");
        assert_eq!(org.placement.pool(), Some("wamn-pg"));
        for policy in [EnvPolicy::dev(), EnvPolicy::prod()] {
            assert_eq!(cluster_of(&org, &policy).name, "wamn-pg");
        }
    }

    /// A dedicated org has no pool.
    #[test]
    fn dedicated_placement_has_no_pool() {
        let org = Org::dedicated("acme");
        assert_eq!(org.placement.kind_str(), "dedicated");
        assert_eq!(org.placement.pool(), None);
    }

    /// The seeded defaults are `dev` (rank 10, its own domain, hibernation-eligible,
    /// no backup) and `prod` (rank 30, HA, backup + PITR, never hibernated). The
    /// single source `deploy/system-schema.sql` seeds and tests share.
    #[test]
    fn default_policies_are_dev_and_prod() {
        let ps = EnvPolicy::defaults();
        assert_eq!(ps.len(), 2);
        let dev = &ps[0];
        assert_eq!(dev.name, Env::new("dev"));
        assert_eq!(dev.recovery_domain, RecoveryDomain::Own);
        assert_eq!(dev.owner(), &Env::new("dev"));
        assert!(dev.hibernation_eligible());
        assert!(!dev.has_scheduled_backup());
        let prod = &ps[1];
        assert_eq!(prod.name, Env::new("prod"));
        assert_eq!(prod.instances, 3);
        assert!(!prod.hibernation_eligible());
        assert!(prod.has_scheduled_backup());
        assert!(
            prod.promotion_rank > dev.promotion_rank,
            "dev promotes to prod"
        );
    }

    /// `recovery_domain` serializes as the D18 jsonb shape: `own` is a bare
    /// string, `shared-with` a `{"shared-with": "<env>"}` object.
    #[test]
    fn recovery_domain_json_shape() {
        assert_eq!(
            serde_json::to_string(&RecoveryDomain::Own).unwrap(),
            "\"own\""
        );
        assert_eq!(
            serde_json::to_string(&RecoveryDomain::SharedWith(Env::new("prod"))).unwrap(),
            "{\"shared-with\":\"prod\"}"
        );
    }

    /// A registry round-trips through JSON, and `env` serializes as a bare slug,
    /// placement as a `{kind, ...}` object, policies as per-org rows.
    #[test]
    fn registry_json_round_trips() {
        let reg = Registry {
            schema_version: SCHEMA_VERSION.into(),
            env_policies: EnvPolicy::defaults()
                .into_iter()
                .map(|policy| OrgEnvPolicy {
                    org: "acme".into(),
                    policy,
                })
                .collect(),
            orgs: vec![Org::pooled("try", "wamn-pg"), Org::dedicated("acme")],
            projects: Vec::new(),
            project_envs: Vec::new(),
        };
        let json = reg.to_json();
        let back = Registry::from_json(&json).expect("parses");
        assert_eq!(reg, back);
        // Placement is tagged; pooled carries its pool.
        assert!(json.contains("\"kind\": \"pooled\""));
        assert!(json.contains("\"pool\": \"wamn-pg\""));
        assert!(json.contains("\"kind\": \"dedicated\""));
        // env-policies are org-scoped rows with kebab-case wire keys.
        assert!(json.contains("\"env-policies\""));
        assert!(json.contains("\"org\": \"acme\""));
        assert!(json.contains("\"promotion-rank\""));
        // Lookups are keyed (org, name); another org has no policy rows.
        assert_eq!(
            reg.env_policy("acme", &Env::new("prod")).unwrap().instances,
            3
        );
        assert!(reg.env_policy("try", &Env::new("prod")).is_none());
        assert_eq!(reg.org_env_policies("acme").len(), 2);
    }

    /// `Env` behaves like a slug: Display / `as_str` / `==` against `&str`.
    #[test]
    fn env_slug_ergonomics() {
        let e = Env::new("prod");
        assert_eq!(e.as_str(), "prod");
        assert_eq!(e.to_string(), "prod");
        assert!(e == "prod");
        assert_eq!(ClusterRef::new(format!("acme-{e}")).name, "acme-prod");
    }
}

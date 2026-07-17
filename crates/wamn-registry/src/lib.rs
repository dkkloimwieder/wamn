//! Canonical control-plane registry model (wamn-q3n.1; generalized in
//! wamn-8df.3, `docs/deployment-model.md`).
//!
//! The registry is the platform's system-of-record for **identity** and
//! **placement** — the foundation of the four-tier Postgres topology
//! (`docs/postgres-topology.md`, epic `wamn-q3n`). It answers two questions and
//! deliberately nothing else:
//!
//! - *who exists* — [`Org`] / [`Project`] / [`ProjectEnv`] membership, keyed by
//!   the first-class [`Triple`] `(org, project, env)` every subsystem speaks;
//! - *where does it live and how is it credentialed* —
//!   [`Registry::resolve`] maps a triple to the CNPG [`ClusterRef`] holding the
//!   database (**derived** by [`cluster_of`] from the org's [`Placement`] and the
//!   env's [`EnvPolicy`]) and a [`SecretRef`] (a **reference**, never the
//!   credential — R8b).
//!
//! The generic deployment model (D18) replaces the closed `Env` / `Tier` enums:
//! `env` is a validated [`Env`] slug resolving a named [`EnvPolicy`], and an org
//! carries a minimal [`Placement`] (`pooled` | `dedicated`) from which clusters
//! derive. Policies are **org-scoped** ([`OrgEnvPolicy`], wamn-8df.4): a
//! [`Template`] preset (`trials` / `standard` / `dedicated` — the `Tier`
//! successor) stamps an org's placement + initial policy set in one step, and
//! the org customizes its own rows per-env.
//!
//! It is a **pure model** (SR6 rule 1: no DB, clock, or wasm): types +
//! [`validate`] + [`Registry::from_json`] / [`Registry::to_json`]. The live
//! system-DB tables that persist this model and their DB-enforced invariants are
//! `deploy/system-schema.sql` (`wamn-q3n.3`, tied here by a drift guard); the
//! environment lifecycle threads into `wamn-schema` (3.4). This is a store model,
//! not a published contract, so — like `wamn-run-store` — there is no generated
//! JSON-Schema file.

mod resolve;
pub mod sql;
mod template;
mod types;
mod validate;

pub use resolve::{RegistryError, Resolution};
pub use template::Template;
pub use types::{
    ClusterRef, DEFAULT_PG_IMAGE, Env, EnvPolicy, Org, OrgEnvPolicy, OrgId, Placement, Project,
    ProjectEnv, ProjectId, RecoveryDomain, Registry, SCHEMA_VERSION, SecretRef, Triple, cluster_of,
};
pub use validate::{Issue, Severity, validate, validate_org_id};

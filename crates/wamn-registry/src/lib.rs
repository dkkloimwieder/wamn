//! Canonical control-plane registry model (wamn-q3n.1).
//!
//! The registry is the platform's system-of-record for **identity** and
//! **placement** — the foundation of the four-tier Postgres topology
//! (`docs/postgres-topology.md`, epic `wamn-q3n`). It answers two questions and
//! deliberately nothing else:
//!
//! - *who exists* — [`Org`] / [`Project`] / [`ProjectEnv`] membership, keyed by
//!   the first-class [`Triple`] `(org, project, env)` every subsystem speaks;
//! - *where does it live and how is it credentialed* —
//!   [`Registry::resolve`] maps a triple to its [`Tier`], the CNPG [`ClusterRef`]
//!   holding the database, and a [`SecretRef`] (a **reference**, never the
//!   credential — R8b).
//!
//! It is a **pure model** (SR6 rule 1: no DB, clock, or wasm): types +
//! [`validate`] + [`Registry::from_json`] / [`Registry::to_json`]. The live
//! system-DB tables and their DB-enforced invariants land with `wamn-q3n.3`; the
//! `dev / canary / prod` lifecycle threads into `wamn-schema` (3.4) with
//! `wamn-q3n.5`. This is a store model, not a published contract, so — like
//! `wamn-run-store` — there is no generated JSON-Schema file.

mod resolve;
mod types;
mod validate;

pub use resolve::{RegistryError, Resolution};
pub use types::{
    ClusterRef, Env, Org, OrgId, Project, ProjectEnv, ProjectId, Registry, SCHEMA_VERSION,
    SecretRef, Side, Tier, Triple,
};
pub use validate::{Issue, Severity, validate};

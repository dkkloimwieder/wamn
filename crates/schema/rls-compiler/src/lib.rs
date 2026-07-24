//! wamn RLS policy builder (3.5).
//!
//! Turns per-entity access rules tied to roles — row ownership, role command
//! gates, and custom per-role predicates — into Postgres Row-Level Security
//! policies. It **composes** the two shipped Epic 3 crates:
//!
//! - [`wamn_catalog`] (3.1) — the model the rules resolve against;
//! - [`wamn_ddl`] (3.2) — whose [`MigrationPlan`] / gate this reuses for output,
//!   and whose **tenant floor** these policies layer on top of.
//!
//! ## Composition with the tenant floor
//!
//! 3.2 emits the tenant floor: a *permissive* `<t>_tenant` policy that isolates
//! rows by `app.tenant`. Postgres ORs permissive policies (a second one would
//! *widen* access) and ANDs restrictive ones — so every policy this crate emits
//! is `AS RESTRICTIVE`, narrowing access **within** a tenant while the floor
//! keeps tenant isolation intact. The rules key on the `app.role` /
//! `app.user_id` session claims, the per-role/user counterparts of the floor's
//! `app.tenant`, injected by the Postgres plugin alongside it (4.2).
//!
//! ```
//! use wamn_catalog::Catalog;
//! use wamn_rls::{AccessPolicy, compile, Confirmation};
//!
//! # fn go(catalog: &Catalog, policy: &AccessPolicy) -> Result<(), Box<dyn std::error::Error>> {
//! let plan = compile(policy, catalog)?;   // -> a wamn-ddl MigrationPlan
//! let sql = plan.sql(Confirmation::None)?; // policy creation is all-additive
//! # let _ = sql;
//! # Ok(())
//! # }
//! ```
//!
//! ## Scope
//!
//! This crate **emits and classifies** RLS policies. It does not execute them
//! (the live apply is the migration engine 2.5 / hosting), inject the session
//! claims (the Postgres plugin, 2.2 / 4.2), authenticate users (8.1), or model
//! field-level read/write masks (4.3). The tenant floor itself stays with 3.2 —
//! this crate only adds the per-role / ownership layer.
//!
//! ## SR12 — what the pure tests cover, and what they cannot
//!
//! This crate's tests exercise the **decision** (which statement, what shape,
//! which binds); they cannot exercise the **statement** — the pure model has no
//! planner, isolation level, lock manager, or RLS. A statement can be modelled
//! correctly here and still misbehave live: `wamn-run-queue`'s `claim_batch_sql`
//! passed every pure test while the real statement over-claimed on a
//! plan-dependent `SKIP LOCKED` re-scan — the `AS MATERIALIZED` fix is a
//! property of the emitted SQL no pure test can observe. Convention (SR12a):
//! every composed or plan-sensitive statement carries a comment naming what the
//! pure tests do NOT cover; the live half is the throwaway-PG gates over the
//! real prepared-statement path (SR12b).

mod compile;
mod model;
mod validate;

pub use compile::{CompileError, compile};
pub use model::{AccessPolicy, Command, CommandGrant, Rule, SCHEMA_VERSION};
pub use validate::validate;

// Re-exported so callers drive the (reused) 3.2 review / gate surface without a
// direct dependency on wamn-ddl.
pub use wamn_catalog::{Issue, Severity};
pub use wamn_ddl::{Confirmation, MigrationPlan};

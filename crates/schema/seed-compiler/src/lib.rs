//! wamn seed-data & fixtures tooling (3.6).
//!
//! Reference and fixture data for a catalog (3.1) as a typed [`Dataset`]: rows
//! grouped by entity, each identified by a **symbolic key** and referencing
//! other rows by key. [`compile`] validates the dataset against the catalog and
//! turns it into tenant-scoped, idempotent `INSERT`s against the tables the DDL
//! compiler (3.2) generates. It **composes** the two shipped Epic 3 crates:
//!
//! - [`wamn_catalog`] (3.1) — the model rows are typed against;
//! - [`wamn_ddl`] (3.2) — whose [`MigrationPlan`] this reuses for output, and
//!   whose tenant floor (`id` / `tenant_id`) these inserts target.
//!
//! Row ids are **deterministic** — `uuidv5("tenant:entity:key")` — so references
//! resolve at compile time and re-applying a seed (a test host cloning a schema,
//! a re-seed) is stable and, with `ON CONFLICT (id) DO NOTHING`, idempotent.
//!
//! ```
//! use wamn_catalog::Catalog;
//! use wamn_seed::{Dataset, compile, Confirmation};
//!
//! # fn go(catalog: &Catalog, dataset: &Dataset) -> Result<(), Box<dyn std::error::Error>> {
//! let plan = compile(dataset, catalog, "tenant-a")?; // -> a wamn-ddl MigrationPlan
//! let sql = plan.sql(Confirmation::None)?;            // a seed load is all-additive
//! # let _ = sql;
//! # Ok(())
//! # }
//! ```
//!
//! ## Scope
//!
//! This crate **emits and classifies** seed SQL. It does not apply it (the live
//! load is the migration engine 2.5 / hosting / the test host 11.1), record-and-
//! replay run fixtures (11.3), or mask sensitive seed data for preview
//! environments (11.9) — though it carries the catalog's `sensitive` flag so 11.9
//! can. The generated tables themselves are 3.2's; this crate only populates them.
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

mod emit;
mod id;
mod model;
mod validate;

pub use emit::{CompileError, compile};
pub use model::{Dataset, EntitySeed, SCHEMA_VERSION, SeedRow};
pub use validate::validate;

// Re-exported so callers drive the (reused) 3.2 review / gate surface without a
// direct dependency on wamn-ddl.
pub use wamn_catalog::{Issue, Severity};
pub use wamn_ddl::{Confirmation, MigrationPlan};

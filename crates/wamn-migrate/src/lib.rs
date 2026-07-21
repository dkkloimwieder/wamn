//! wamn migration engine (2.5).
//!
//! The **live executor** that applies a catalog to a project database. It does
//! not re-derive migration logic — it **composes the shipped machinery**:
//!
//! - [`wamn_ddl`] (3.2) — computes the DDL (`Migration::create` / `migrate`) and
//!   owns the additive/destructive [`Confirmation`] gate, reused verbatim (a
//!   destructive plan is refused without a confirmed backup, and the emitted DDL
//!   carries the backup-checkpoint marker);
//! - [`wamn_schema`] (3.4) — the `draft → staged → applied → superseded`
//!   lifecycle with the *single-applied* and *stale-base* guards, reused as the
//!   validation oracle so the live engine can never diverge from them;
//! - [`wamn_catalog`] (3.1) — the canonical model and its JSON, which is what the
//!   engine stores (the applied catalog `document`) and diffs against.
//!
//! Given the current applied catalog (read from the DB by the driver) and a
//! target, the engine produces:
//!
//! - an [`ApplyPlan`] — the ordered `$n`-parameterized statements to run in **one
//!   transaction**: the DDL, the lifecycle advance in `catalog.catalogs`
//!   (demote the prior applied, promote the target, storing its `document`), and
//!   an immutable row in `catalog.schema_migrations`;
//! - a [`MigrationReport`] — a dry run (no gate, no mutation) with the DDL report
//!   and the rollback plan;
//! - a [`RollbackPlan`] — a generated inverse forward-migration plus a
//!   restore-to-last-dump pointer.
//!
//! ## Scope (v1)
//!
//! The **tenant catalog** migration engine: execute wamn-ddl plans over catalog
//! versions, advance the lifecycle, record history, dry-run, and generate a
//! rollback. Versioned + **forward-only** (a version applies only if newer than
//! the current applied one). The "system-schema migrations shipped with platform
//! releases" flavor (hand-written SQL evolving `app_system` / `catalog` across
//! every project DB on upgrade — different inputs, different trigger) is a
//! separate follow-up.
//!
//! ## Purity + the one-transaction invariant
//!
//! This crate is **pure** (no DB, clock, or wasm — the wamn-ddl/wamn-schema
//! SR6 precedent): it emits SQL text and the driver
//! ([`wamn-host migrate-catalog`](../wamn_host/migrate_catalog/index.html))
//! executes it. The whole [`ApplyPlan`] runs in **one transaction**, which is
//! what makes the wamn-ddl name-freeing preamble's *zero-residue* guarantee hold
//! (a mid-plan failure rolls the aside-renames back, so no `wamn_mig_drop_*`
//! survives — no compensation path is needed). This holds while the compiler
//! emits no non-transactional step; `CREATE INDEX CONCURRENTLY` is the known
//! breaker, deferred (it would need a residue janitor + an apply journal — see
//! `docs/migration-engine.md`).
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

mod engine;
mod model;
mod orphan;
mod replica_identity;
mod run_plane;
pub mod sql;

pub use engine::{dry_run, plan_migration, rollback_plan};
pub use model::{
    ApplyPlan, Confirmation, Env, MigrationError, MigrationReport, MigrationRequest, RollbackPlan,
    SqlStatement, Value,
};
pub use orphan::{OrphaningPublish, RegistrationRef, check_registration_orphans};
pub use replica_identity::{
    ReplicaIdentity, ReplicaIdentityFlip, ReplicaIdentityPlan, alter_replica_identity_sql,
    entities_requiring_full, reconcile_replica_identity, select_replica_identity_sql,
};
pub use run_plane::{
    LEGACY_OUTBOX_TABLES, OUTBOX_TRIGGER_NAME, RunPlaneAction, RunPlaneActionKind,
    RunPlaneObservation, RunPlanePlan, catalog_schema_present_sql,
    count_stale_registration_state_sql, plan_run_plane, rewrite_schema,
    select_outbox_function_present_sql, select_outbox_trigger_tables_sql,
    select_runs_fail_kind_check_sql, select_schema_columns_sql, select_schema_indexes_sql,
    strip_registration_state_sql,
};

// Re-exported so a driver can name the registration type the reconciler folds
// without a direct dependency on wamn-event-reg.
pub use wamn_event_reg::EventRegistration;

// Re-exported so a driver can name the wamn-ddl / wamn-schema types the engine
// returns without a direct dependency on those crates.
pub use wamn_catalog::Catalog;
pub use wamn_ddl::{MigrationPlan, RequiresConfirmation};

//! wamn migration engine (2.5).
//!
//! The **live executor** that applies a catalog to a project database. It does
//! not re-derive migration logic — it **composes the shipped machinery**:
//!
//! - [`wamn_schema_compiler`] (3.2) — computes and classifies the DDL
//!   (`Migration::create` / `migrate`); the public planner accepts additive
//!   changes only;
//! - [`crate::lifecycle`] (3.4) — the `draft → staged → applied → superseded`
//!   lifecycle with the *single-applied* and *stale-base* guards, reused as the
//!   validation oracle so the live engine can never diverge from them;
//! - [`wamn_schema_model`] (3.1) — the canonical model and its JSON, which is what the
//!   engine stores (the applied catalog `document`) and diffs against.
//!
//! Given the current applied catalog (read from the DB by the driver) and a
//! target, the engine produces:
//!
//! - an [`ApplyPlan`] — the ordered `$n`-parameterized statements to run in **one
//!   transaction**: the DDL, the lifecycle advance in `catalog.catalogs`
//!   (demote the prior applied, promote the target, storing its `document`), and
//!   an immutable row in `catalog.schema_migrations`.
//!
//! Destructive target reconciliation and read-only impact compilation are
//! available only through the `ops` feature. Their authorization evidence lives
//! in operations state, outside this pure planner.
//!
//! ## Scope (v1)
//!
//! The **tenant catalog** migration engine: execute wamn-schema-compiler plans over catalog
//! versions, advance the lifecycle, and record history. Versioned +
//! **forward-only** (a version applies only if newer than
//! the current applied one). The "system-schema migrations shipped with platform
//! releases" flavor (hand-written SQL evolving `app_system` / `catalog` across
//! every project DB on upgrade — different inputs, different trigger) is a
//! separate follow-up.
//!
//! ## Purity + the one-transaction invariant
//!
//! This crate is **pure** (no DB, clock, or wasm — the wamn-schema-compiler/wamn-schema-control
//! SR6 precedent): it emits SQL text and the driver
//! (`wamn-ctl migrate-catalog`) executes it. The whole [`ApplyPlan`] runs in
//! **one transaction**, which is
//! what makes the wamn-schema-compiler name-freeing preamble's *zero-residue* guarantee hold
//! (a mid-plan failure rolls the aside-renames back, so no `wamn_mig_drop_*`
//! survives — no compensation path is needed). This holds while the compiler
//! emits no non-transactional step; `CREATE INDEX CONCURRENTLY` is the known
//! breaker, deferred (it would need a residue janitor + an apply journal — see
//! `docs/archive/schema/migration-engine.md`).
//!
//! ## SR12 — what the pure tests cover, and what they cannot
//!
//! This crate's tests exercise the **decision** (which statement, what shape,
//! which binds); they cannot exercise the **statement** — the pure model has no
//! planner, isolation level, lock manager, or RLS. A statement can be modelled
//! correctly here and still misbehave live: `wamn-run-state`'s `claim_batch_sql`
//! passed every pure test while the real statement over-claimed on a
//! plan-dependent `SKIP LOCKED` re-scan — the `AS MATERIALIZED` fix is a
//! property of the emitted SQL no pure test can observe. Convention (SR12a):
//! every composed or plan-sensitive statement carries a comment naming what the
//! pure tests do NOT cover; the live half is the throwaway-PG gates over the
//! real prepared-statement path (SR12b).

pub mod connections;
mod engine;
pub mod exposure;
#[cfg(feature = "ops")]
pub mod impact;
pub mod lifecycle;
mod model;
#[cfg(feature = "ops")]
pub mod ops;
mod orphan;
mod publication;
mod replica_identity;
mod run_plane;
pub mod sql;

pub use engine::plan_migration;
pub use exposure::{
    Attachment, AttachmentKind, Cardinality, ExposureError, ExposureRelease, FlowExposure,
    HttpRoute, InputMapping, MappingSource, ResolvedAttachment, Source, SourceKind,
    resolve_exposure,
};
pub use model::{
    ApplyPlan, DestructiveMigration, Env, MigrationError, MigrationRequest, SqlStatement, Value,
};
pub use orphan::{OrphaningPublish, RegistrationRef, check_registration_orphans};
pub use publication::{
    PublicationError, PublicationGuard, ReleaseFlow, canonical_release_flows, guard_publication,
};
pub use replica_identity::{
    ReplicaIdentity, ReplicaIdentityFlip, ReplicaIdentityPlan, alter_replica_identity_sql,
    entities_requiring_full, reconcile_replica_identity, select_replica_identity_sql,
};
pub use run_plane::{
    BareSchemaName, EFFECT_WRITER_ROLE, EffectWriterRoleObservation, InvalidBareSchemaName,
    LEGACY_OUTBOX_TABLES, OUTBOX_TRIGGER_NAME, RunPlaneAction, RunPlaneActionKind,
    RunPlaneObservation, RunPlanePlan, ScenarioAuthorRoleObservation, catalog_schema_present_sql,
    count_release_flow_rows_sql, count_run_rows_sql, count_stale_registration_state_sql,
    ensure_scenario_author_role_sql, plan_run_plane, rewrite_schema,
    select_app_scenario_author_membership_sql, select_authoring_effective_column_privileges_sql,
    select_authoring_effective_table_privileges_sql, select_authoring_table_owners_sql,
    select_authoring_table_privileges_sql, select_effect_ledger_effective_column_privileges_sql,
    select_effect_ledger_effective_privileges_sql, select_effect_ledger_table_privileges_sql,
    select_effect_writer_role_sql, select_effect_writer_schema_privileges_sql,
    select_outbox_function_present_sql, select_outbox_trigger_tables_sql,
    select_run_capture_privileges_sql, select_run_plane_helper_functions_sql,
    select_scenario_author_catalog_lock_privilege_sql, select_scenario_author_role_sql,
    select_scenario_author_schema_usage_sql, select_schema_checks_sql, select_schema_columns_sql,
    select_schema_foreign_keys_sql, select_schema_indexes_sql, select_schema_triggers_sql,
    strip_registration_state_sql,
};

// Re-exported so a driver can name the registration type the reconciler folds
// without a direct dependency on wamn-event-reg.
pub use wamn_event_reg::EventRegistration;

// Re-exported so an operations impact driver can name the classified plan
// without a direct compiler dependency.
#[cfg(feature = "ops")]
pub use wamn_schema_compiler::MigrationPlan;
pub use wamn_schema_model::Catalog;

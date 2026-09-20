//! # wamn-run-state — the durable execution lifecycle
//!
//! MVP outcome: crash floor · M0 execution · flow composition.
//!
//! This crate owns the transactionally coupled `runs`, `run_queue`, lease,
//! timer, and terminal lifecycle. It contains only decisions and
//! parameterized SQL; Postgres, clocks, and
//! database calls remain adapter effects.
//!
//! This crate's default graph is **pure**: no DB, no wasm, no clock. The
//! crate maps execution outcomes to storage literals ([`RunStatus`]); the
//! host-owned executor adapter supplies the `wamn:postgres` effects against the
//! schema in `deploy/sql/run-state.sql`.
//!
//! ## SR12 — what the pure tests cover, and what they cannot
//!
//! This crate's tests exercise the **decision** (which statement, what shape,
//! which binds); they cannot exercise the **statement** — the pure model has no
//! planner, isolation level, lock manager, or RLS. A statement can be modelled
//! correctly here and still misbehave live: the production claim selector
//! passed every pure test while the real statement over-claimed on a
//! plan-dependent `SKIP LOCKED` re-scan — the `AS MATERIALIZED` fix is a
//! property of the emitted SQL no pure test can observe. Convention (SR12a):
//! every composed or plan-sensitive statement carries a comment naming what the
//! pure tests do NOT cover; the live half is the throwaway-PG gates over the
//! real prepared-statement path (SR12b).

/// Capture-independent effect-attempt generation facts.
///
/// Durable-tier shelf (`wamn-hopk` R1): behind `durable-tier`, off by default,
/// so a live path referencing it fails to compile instead of being grepped for.
#[cfg(feature = "durable-tier")]
pub mod attempt;
/// The closed authority a trusted caller selects its credential under.
pub mod authority_class;
/// The two reusable credential-generation slots.
mod credential_generation;
/// The durability class a run was admitted under, and the crash-floor gate.
pub mod durability;
/// RUN-* as plain `fn check(state)` functions, for the pure decision tests to
/// call after every step.
pub mod invariants;
/// Versioned identity shared by persisted admission and trusted effect calls.
pub mod invocation_context;
/// Operator resolution of an effect-uncertain run.
pub mod operator_action;
/// Durable global queue, lease, timer, and terminal decisions and SQL.
pub mod queue;
/// The JSON payload redaction policy extracted from node I/O capture.
pub mod redaction;
/// Contract-owned helpers for checking repository stand-in schemas.
#[cfg(feature = "test-util")]
pub mod schema_drift;
/// Run-state SQL text builders (SR2): the single source adapters execute.
pub mod sql;
mod status;
/// The framed scope digest and the guest-SQL tenant key.
#[cfg(feature = "tenant-key")]
pub mod tenant_scope;
/// Typed, queue-joined executor transitions.
pub mod transitions;

pub use authority_class::AuthorityClass;
pub use credential_generation::CredentialGeneration;
pub use durability::{DURABLE_CLASS_SQL_PREDICATE, DurabilityClass};
pub use status::{
    EffectUncertainFailure, FailKind, InvalidEffectUncertainRunId, NodeErrorKind, NodeRunStatus,
    RunStatus,
};
#[cfg(feature = "tenant-key")]
pub use tenant_scope::app_scope_hash;

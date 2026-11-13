//! # wamn-run-state — the durable execution lifecycle
//!
//! Queue recovery, execution state, and flow composition.
//!
//! This crate owns the transactionally coupled `runs`, `run_queue`, lease,
//! timer, and terminal lifecycle. It contains decisions, parameterized SQL,
//! and the storage traits: [`RunStore`] for the queued-run lifecycle and
//! [`IntentStore`] for the per-call intent record. Postgres, clocks, and
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
//! correctly here and still misbehave live: the production lease selector
//! passed every pure test while the real statement over-acquired on a
//! plan-dependent `SKIP LOCKED` re-scan — the `AS MATERIALIZED` fix is a
//! property of the emitted SQL no pure test can observe. Convention (SR12a):
//! every composed or plan-sensitive statement carries a comment naming what the
//! pure tests do NOT cover; the live half is the throwaway-PG acceptance tests over the
//! real prepared-statement path (SR12b).

/// The closed authority a trusted caller selects its credential under.
pub mod authority_class;
/// The two reusable credential-generation slots.
mod credential_generation;
/// The intent store case set that every store runs.
#[cfg(feature = "test-util")]
pub mod intent_cases;
/// The per-call intent record as a storage trait.
pub mod intent_store;
/// RUN-* as plain `fn check(state)` functions, for the pure decision tests to
/// call after every step.
pub mod invariants;
/// Evidence basis for operator resolution of intents.
pub mod operator_action;
/// The framed identity digest and the guest-SQL project key.
#[cfg(feature = "project-key")]
pub mod project_identity;
/// Durable global queue, lease, timer, and terminal decisions and SQL.
pub mod queue;
/// The JSON payload redaction policy extracted from node I/O capture.
pub mod redaction;
/// The queued-run lifecycle as a storage trait.
pub mod run_store;
/// Run-state SQL text builders (SR2): the single source adapters execute.
pub mod sql;
mod status;
/// Typed, queue-joined executor transitions.
pub mod transitions;

pub use authority_class::AuthorityClass;
pub use credential_generation::CredentialGeneration;
pub use intent_store::IntentStore;
#[cfg(feature = "project-key")]
pub use project_identity::app_identity_hash;
pub use run_store::RunStore;
pub use status::{FailType, NodeErrorType, NodeRunStatus, RunStatus};

//! # wamn-run-state — the durable execution lifecycle
//!
//! MVP outcome: crash floor · M0 execution · flow composition.
//!
//! This crate owns the transactionally coupled `runs`, `run_queue`, lease,
//! timer, and terminal lifecycle. It contains only decisions and
//! parameterized SQL; Postgres, clocks, and
//! doorbells remain adapter effects.
//!
//! This crate's default graph is **pure**: no DB, no wasm, no clock. The
//! non-default `native` feature contains the private effect-writer adapter. The
//! crate maps execution outcomes to storage literals ([`RunStatus`]); the
//! host-owned executor adapter supplies the `wamn:postgres` effects against the
//! schema in `deploy/sql/run-state.sql`.
//!
//! Private management admission composes the one surviving admission
//! transaction; hot HTTP and stream ingress execute through the router.
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

/// The private management admission transaction.
pub mod admission;
/// Capture-independent effect-attempt generation facts.
///
/// Durable-tier shelf (`wamn-hopk` R1): behind `durable-tier`, off by default,
/// so a live path referencing it fails to compile instead of being grepped for.
#[cfg(feature = "durable-tier")]
pub mod attempt;
/// The closed authority a trusted caller selects its credential under.
pub mod authority_class;
/// The durability class a run was admitted under, and the crash-floor gate.
pub mod durability;
/// Shared strict credential document for the private native effect writer.
#[cfg(feature = "effect-writer-credential")]
pub mod effect_writer_credential;
/// The framed scope digest and the guest-SQL tenant key.
#[cfg(feature = "tenant-key")]
pub mod tenant_scope;
// Host-only effect-ledger statements stay out of the default guest-safe graph.
// The attempt-ledger adapter remains unmounted in production. See the module doc.
#[cfg(feature = "native")]
mod effect_writer;
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
/// Typed, queue-joined executor transitions.
pub mod transitions;

pub use authority_class::AuthorityClass;
pub use durability::{DURABLE_CLASS_SQL_PREDICATE, DurabilityClass};
#[cfg(feature = "native")]
pub use effect_writer::{
    BeginEffectAttempt, EffectAttempt, EffectAttemptId, EffectDispatchPermit, EffectOutcome,
    RecordEffectOutcome,
};
#[cfg(feature = "native")]
pub use effect_writer::{
    EffectWriterClient, EffectWriterError, EffectWriterErrorKind, EffectWriterScope,
};
#[cfg(feature = "effect-writer-credential")]
pub use effect_writer_credential::{
    CredentialGeneration, EFFECT_WRITER_CREDENTIAL_KEY, EFFECT_WRITER_CREDENTIAL_PATH,
    EFFECT_WRITER_CREDENTIAL_SCHEMA_VERSION, EFFECT_WRITER_ROLE, EffectWriterCredential,
    EffectWriterCredentialError, EffectWriterCredentialErrorKind, EffectWriterCredentialScope,
    EffectWriterCredentialValidity, RUN_PROJECTION_WRITER_ROLE, effect_writer_credential,
    effect_writer_generation_role, effect_writer_scope_hash, parse_effect_writer_credential,
    validate_effect_writer_credential,
};
pub use status::{
    EffectUncertainFailure, FailKind, InvalidEffectUncertainRunId, NodeErrorKind, NodeRunStatus,
    RunStatus,
};
#[cfg(feature = "tenant-key")]
pub use tenant_scope::app_scope_hash;

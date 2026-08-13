//! # wamn-run-state — the durable execution lifecycle
//!
//! This crate owns the transactionally coupled `runs`, `node_runs`, `run_queue`,
//! lease, partition, timer, and dead-letter lifecycle. It contains only models,
//! decisions, reconstruction, and parameterized SQL; Postgres, clocks, and
//! doorbells remain adapter effects.
//!
//! Like [`wamn_runner`], this crate's default guest-safe graph is **pure**: no DB,
//! no wasm, no clock. The non-default `native` feature contains the private
//! effect-writer adapter. The crate maps the engine's execution taxonomy to storage literals
//! ([`RunStatus`]) and drives the engine's [`resume`](wamn_runner::Plan::resume) /
//! [`seed_at`](wamn_runner::Plan::seed_at) primitives; the driver
//! (`components/execution/flowrunner`) supplies the `wamn:postgres` effects against the
//! schema in `deploy/sql/run-state.sql`.
//!
//! ```
//! use wamn_run_state::{reconstruct, NodeRunRecord, RunRecord};
//! use wamn_runner::{ExecutionStatus, Plan};
//! use wamn_flow::{Flow, ResolvedInterfaces};
//! use serde_json::json;
//!
//! let flow = Flow::from_json(r#"{
//!   "schema-version": "0.1", "flow-id": "f", "version": 1,
//!   "nodes": [{"id": "a", "type": "cron"}, {"id": "b", "type": "echo"}],
//!   "edges": [{"from": "a", "to": "b"}]
//! }"#).unwrap();
//! let interfaces = ResolvedInterfaces::from([
//!     ("echo".to_string(), vec!["main".to_string()])
//! ]);
//! let plan = Plan::compile(&flow, &interfaces).unwrap();
//!
//! // The run was killed after `a` committed: only `a` is persisted. `a` is a
//! // `cron` entry, so its recorded output is the admitted input, unchanged.
//! let run = RunRecord::new("run-1", "f", 1, json!({"n": 1}));
//! let node_runs = [NodeRunRecord::success(
//!     "run-1",
//!     "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
//!     "a",
//!     0,
//!     "main",
//!     json!({"n": 1}),
//! )];
//! let st = reconstruct(&plan, &run, &node_runs).unwrap();
//! assert_eq!(st.status(), ExecutionStatus::Running);
//! assert_eq!(st.step_seq(), 1); // `a` folded; `b` is the outstanding frontier
//! ```
//!
//! Admission and enqueue producers compose the transaction API exposed by this
//! crate while owning their database transactions.
//!
//! ## SR12 — what the pure tests cover, and what they cannot
//!
//! This crate's tests exercise the **decision** (which statement, what shape,
//! which binds); they cannot exercise the **statement** — the pure model has no
//! planner, isolation level, lock manager, or RLS. A statement can be modelled
//! correctly here and still misbehave live: `queue::claim_batch_sql`
//! passed every pure test while the real statement over-claimed on a
//! plan-dependent `SKIP LOCKED` re-scan — the `AS MATERIALIZED` fix is a
//! property of the emitted SQL no pure test can observe. Convention (SR12a):
//! every composed or plan-sensitive statement carries a comment naming what the
//! pure tests do NOT cover; the live half is the throwaway-PG gates over the
//! real prepared-statement path (SR12b).

/// The single producer-shaped callable-flow admission transition.
pub mod admission;
/// Capture-independent effect-attempt generation facts.
pub mod attempt;
/// Node-level I/O capture and durable output projection.
pub mod capture;
/// Occurrence-keyed child creation, parent parking, and atomic child release.
pub mod child;
pub mod context;
/// Shared strict credential document for the private native effect writer.
#[cfg(feature = "effect-writer-credential")]
pub mod effect_writer_credential;
// Host-only effect-ledger statements stay out of the default guest-safe graph.
// The first production caller/invocation is intentionally deferred to .5.4.
#[cfg(feature = "native")]
mod effect_writer;
/// Durable lookup and bounded-wait queries for flow invocation.
pub mod invocation;
/// Versioned identity shared by persisted admission and trusted effect calls.
pub mod invocation_context;
mod model;
/// Durable queue, lease, partition, timer, and dead-letter decisions and SQL.
pub mod queue;
mod reconstruct;
mod rerun;
mod resolution;
/// Contract-owned helpers for checking repository stand-in schemas.
#[cfg(feature = "test-util")]
pub mod schema_drift;
/// Run-state SQL text builders (SR2): the single source both guests and
/// host drivers execute.
pub mod sql;
mod status;
/// Typed, queue-joined executor transitions.
pub mod transitions;

pub use capture::{
    CaptureMode, Captured, NodeOutputProjection, OutputTooLarge, derive as derive_capture,
    project_output,
};
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
    EffectWriterCredentialValidity, effect_writer_credential, effect_writer_generation_role,
    effect_writer_scope_hash, parse_effect_writer_credential, validate_effect_writer_credential,
};
pub use model::{NodeRunRecord, RunRecord};
pub use reconstruct::{ReconstructError, reconstruct, reconstruct_with_context};
pub use rerun::{PartialRerun, RerunError, plan_partial_rerun, plan_replay};
pub use resolution::{
    BoundConnectionRequirement, CandidatePlanOverride, CatalogResolutionPlan,
    FlowResolutionRefusal, PinnedRunResolution, RunFlowResolution, resolve_run_flow_resolutions,
};
pub use status::{
    EffectUncertainFailure, FailKind, InvalidEffectUncertainRunId, NodeErrorKind, NodeRunStatus,
    RunStatus,
};

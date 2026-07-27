//! # wamn-run-state — the durable execution lifecycle
//!
//! This crate owns the transactionally coupled `runs`, `node_runs`, `run_queue`,
//! lease, partition, timer, and dead-letter lifecycle. It contains only models,
//! decisions, reconstruction, and parameterized SQL; Postgres, clocks, and
//! doorbells remain adapter effects.
//!
//! Like [`wamn_runner`], this crate is **pure**: no DB, no wasm, no
//! clock. It maps the engine's execution taxonomy to storage literals
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
//! // The run was killed after `a` committed: only `a` is persisted.
//! let run = RunRecord::new("run-1", "f", 1, json!({"n": 1}));
//! let node_runs = [NodeRunRecord::success("run-1", "a", 0, "main", json!({"at": "a"}))];
//! let st = reconstruct(&plan, &run, &node_runs).unwrap();
//! assert_eq!(st.status(), ExecutionStatus::Running);
//! assert_eq!(st.step_seq(), 1); // `a` folded; `b` is the outstanding frontier
//! ```
//!
//! Trigger schedule evaluation and polling cadence live in `wamn-scheduler`;
//! this crate owns the durable anchor and enqueue operations they invoke.
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

/// Node-level I/O capture policy application (9.6): the pure scrub / truncate /
/// preview-derivation the flowrunner guest links to fill a `node_runs` row's
/// capture columns before the write.
pub mod capture;
mod model;
/// Durable queue, lease, partition, timer, and dead-letter decisions and SQL.
pub mod queue;
mod reconstruct;
mod rerun;
/// Contract-owned helpers for checking repository stand-in schemas.
#[cfg(feature = "test-util")]
pub mod schema_drift;
/// Run-state SQL text builders (SR2): the single source both guests and
/// host drivers execute.
pub mod sql;
mod status;
/// Typed, queue-joined executor transitions.
pub mod transitions;

pub use capture::{Captured, derive as derive_capture};
pub use model::{NodeRunRecord, RunRecord};
pub use reconstruct::{ReconstructError, reconstruct};
pub use rerun::{PartialRerun, RerunError, plan_partial_rerun, plan_replay};
pub use status::{FailKind, NodeErrorKind, NodeRunStatus, RunStatus};

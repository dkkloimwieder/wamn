//! # wamn-run-store (5.7) — durable run state
//!
//! The persistence half of the flow runner: the `runs` / `node_runs` record model
//! plus **branch-aware replay reconstruction** and **partial re-run** planning
//! over the pure engine ([`wamn_runner`], 5.2). Where 5.2 left an in-memory
//! [`RunState`](wamn_runner::RunState) with a single `step_seq` seam, 5.7 persists
//! one row per node execution and rebuilds the exact frontier from those rows —
//! so a rescheduled runner resumes precisely where it was killed, down the branch
//! it took, and a fixed transient error re-runs from just the failed node.
//!
//! Like [`wamn_runner`] / `wamn-api`, this crate is **pure**: no DB, no wasm, no
//! clock. It maps the engine's execution taxonomy to storage literals
//! ([`status`]) and drives the engine's [`resume`](wamn_runner::Plan::resume) /
//! [`seed_at`](wamn_runner::Plan::seed_at) primitives; the driver
//! (`components/flowrunner`) supplies the `wamn:postgres` effects against the
//! schema in `deploy/sql/run-state.sql`.
//!
//! ```
//! use wamn_run_store::{reconstruct, RunRecord, NodeRunRecord};
//! use wamn_runner::{Plan, RunStatus};
//! use wamn_flow::Flow;
//! use serde_json::json;
//!
//! let flow = Flow::from_json(r#"{
//!   "schema-version": "0.1", "flow-id": "f", "version": 1,
//!   "trigger": {"type": "manual"}, "entry": "a",
//!   "nodes": [{"id": "a", "type": "echo"}, {"id": "b", "type": "echo"}],
//!   "edges": [{"from": "a", "to": "b"}]
//! }"#).unwrap();
//! let plan = Plan::compile(&flow).unwrap();
//!
//! // The run was killed after `a` committed: only `a` is persisted.
//! let run = RunRecord::new("run-1", "f", 1, json!({"n": 1}));
//! let node_runs = [NodeRunRecord::success("run-1", "a", 0, "main", json!({"at": "a"}))];
//! let st = reconstruct(&plan, &run, &node_runs).unwrap();
//! assert_eq!(st.status(), RunStatus::Running);
//! assert_eq!(st.step_seq(), 1); // `a` folded; `b` is the outstanding frontier
//! ```
//!
//! ## Scope (5.7) vs siblings
//! Owns: the `runs`/`node_runs` model + DDL (`deploy/sql/run-state.sql`), at-least-once
//! idempotency keying, the run-history read model, branch-aware replay
//! reconstruction, and partial-re-run planning. Does **not** own: the durable run
//! QUEUE + leases + NATS doorbell + dispatcher (5.14 — co-transacts with these
//! INSERTs but owns its own table); the node-level I/O CAPTURE policy (9.6 — fills
//! the `input`/`output`/`preview`/`redacted` slots); the content-addressed payload
//! BYTE store (5.10 — pointed at by the reserved `*_ref`/preview columns); per-node
//! ordering (5.11); the cancel operation (5.12).

mod model;
mod reconstruct;
mod rerun;
/// Run-state SQL text builders (SR2): the single source both guests and
/// host drivers execute.
pub mod sql;
mod status;

pub use model::{NodeRunRecord, RunRecord};
pub use reconstruct::{ReconstructError, reconstruct};
pub use rerun::{PartialRerun, RerunError, plan_partial_rerun, plan_replay};
pub use status::{FailKind, NodeErrorKind, NodeRunStatus, RunStatus};

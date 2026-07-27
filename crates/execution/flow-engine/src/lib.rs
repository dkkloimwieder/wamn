//! # wamn-runner (5.2) — the flow-runner engine
//!
//! A **pure, synchronous reducer** over a [`wamn_flow::Flow`]: it walks the graph
//! from `entry` following ported edges, branches and merges, routes errors, and
//! schedules retries with backoff — deciding everything **mechanically** from the
//! [`NodeOutcome`] variant, never by string-matching a message. Every effect
//! (dispatching a node, sleeping, checkpointing to Postgres, ringing the reload
//! doorbell) is the driver's; this crate holds no clock, no DB, no host, no wasm.
//! That is what makes the whole execution engine unit-testable with no cluster —
//! the same split `wamn-api` uses for the gateway.
//!
//! ```no_run
//! use wamn_runner::{Plan, NodeOutcome, ExecutionStatus};
//! use wamn_flow::Flow;
//! use serde_json::json;
//! use std::collections::BTreeMap;
//!
//! let flow = Flow::from_json(r#"{
//!   "schema-version": "0.1", "flow-id": "f", "version": 1,
//!   "nodes": [
//!     {"id": "entry", "type": "cron"},
//!     {"id": "a", "type": "echo"},
//!     {"id": "b", "type": "echo"}
//!   ],
//!   "edges": [{"from": "entry", "to": "a"}, {"from": "a", "to": "b"}]
//! }"#).unwrap();
//! let interfaces = BTreeMap::from([("echo".to_string(), vec!["main".to_string()])]);
//! let plan = Plan::compile(&flow, &interfaces).unwrap();
//!
//! let mut st = plan.start("run-1", json!({"n": 1}));
//! let clock = std::cell::Cell::new(0u64);
//! let status = plan.drive(
//!     &mut st,
//!     || clock.get(),
//!     |until, _key| clock.set(until),        // "sleep": jump the clock
//!     |d| NodeOutcome::ok(d.config.clone()), // trivial echo node
//! );
//! assert_eq!(status, ExecutionStatus::Completed);
//! ```
//!
//! ## Scope (5.2) vs siblings
//! Owns: the ported-edge walk, branch/merge, error-path routing, the
//! retry/backoff loop, the shared per-target throttle key + per-flow concurrency
//! accounting ([`ThrottleTable`]), the hot-reload *consumer* seam (recompile a
//! [`Plan`] on a new flow version), and the pure **reconstruction primitives**
//! [`resume`](Plan::resume) (rebuild a run's frontier from its recorded steps —
//! branch-aware) and [`seed_at`](Plan::seed_at) (partial re-run from one node).
//! Does **not** own: the `wamn:node` taxonomy (5.4 — mirrored here as
//! [`NodeError`]), the durable `runs`/`node_runs` **schema, persistence, and
//! run-history read model** (5.7 — `wamn-run-state` drives these primitives over
//! the store), per-node ordering (5.11), the cancel operation (5.12), the durable
//! queue + NATS doorbell + dispatcher (5.14), the payload store (5.10), the
//! standard node contents (5.3), or the custom-node transport (5.6). The driver
//! (`components/execution/flowrunner`) wires those in.

mod engine;
mod outcome;
mod plan;
mod retry;
mod throttle;

pub use engine::{
    Dispatch, ExecutionFailureKind, ExecutionState, ExecutionStatus, Failure, Recorded,
    ResumeError, Step, UnknownNode,
};
pub use outcome::{ERROR_PORT, ErrorDetail, MAIN_PORT, NodeError, NodeOutcome, RateLimitDetail};
pub use plan::{DEFAULT_DISPATCH_BUDGET, EngineError, Plan};
pub use retry::RetryPolicy;
pub use throttle::{ConcurrencyGate, ThrottleKey, ThrottleTable};

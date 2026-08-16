//! # wamn-runner (5.2) — the flow-runner engine
//!
//! MVP outcome: crash floor · M0 execution · flow composition.
//!
//! A **pure, synchronous reducer** over a [`wamn_flow::Flow`]: it walks the graph
//! from `entry` following ported edges, branches and merges, routes errors, and
//! schedules retries with backoff — deciding everything **mechanically** from the
//! [`NodeOutcome`] variant, never by string-matching a message. Every effect
//! (dispatching a node, sleeping, recording history, ringing the reload
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
//!     {"id": "entry", "type": "event"},
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
//! Owns: caller-release and fail lifecycle state; event admission and
//! entry ordering; the ported-edge walk, branch/merge, error-path routing;
//! the retry/backoff loop; the shared per-target throttle key + per-flow concurrency
//! accounting ([`ThrottleTable`]) and the hot-reload *consumer* seam (recompile
//! a [`Plan`] on a new flow version).
//! Does **not** own: the node error taxonomy (owned by
//! [`NodeError`](wamn_flow::node_contract::NodeError)), the durable
//! `runs`/`node_runs` **schema, persistence, and
//! run-history read model** (5.7), the durable queue + NATS doorbell + dispatcher
//! (5.14), the payload store (5.10), the
//! standard node contents (5.3). The driver
//! (`components/execution/flowrunner`) wires those in.

mod engine;
mod outcome;
mod plan;
mod retry;
mod throttle;

pub use engine::{
    ApplyError, CallerState, Dispatch, ExecutionFailureKind, ExecutionState, ExecutionStatus,
    Failure, ReservedStep, Step, validate_event_outcome, validate_fail_outcome,
    validate_request_outcome,
};
pub use outcome::{ERROR_PORT, MAIN_PORT, NodeOutcome};
pub use plan::{DEFAULT_DISPATCH_BUDGET, EngineError, Plan};
pub use retry::RetryPolicy;
pub use throttle::{ConcurrencyGate, ThrottleKey, ThrottleTable};

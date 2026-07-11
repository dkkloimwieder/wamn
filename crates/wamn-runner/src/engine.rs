//! The reducer: a pure, synchronous state machine that walks one run through the
//! [`Plan`]. Every effect — dispatching a node, sleeping for a backoff,
//! checkpointing to Postgres — belongs to the driver; the engine only decides the
//! next [`Step`] and folds a [`NodeOutcome`] into [`RunState`].
//!
//! ## Driver loop
//! ```ignore
//! let mut st = plan.start(run_id, input);
//! loop {
//!     match plan.next(&mut st, now_ms()) {
//!         Step::Dispatch(d)  => { let o = run_node(&d); plan.apply(&mut st, &d, o, now_ms()); }
//!         Step::Wait { until_ms, throttle, .. } => { /* gate `throttle`, sleep to until_ms */ }
//!         Step::Done(status) => break status,
//!     }
//! }
//! ```
//!
//! ## Walk model (v1)
//! A single-token BFS frontier. A node emits on a **port**; the engine enqueues
//! the edges leaving that port. Branch = several ports; merge = several edges into
//! one node (no join *barrier* in v1 — a merged node runs once per arriving token;
//! join barriers are a later item). Fan-out (several edges from one port) runs
//! **sequentially** in frontier order (true per-node parallelism is 5.11).
//! Branch-aware *durable* resume (persisting the frontier) is 5.7; v1 checkpoints
//! only `step_seq` (completed-step count), which the driver uses for the linear
//! resume path.

use std::collections::VecDeque;

use serde_json::Value;

use crate::outcome::{ErrorDetail, NodeError, NodeOutcome};
use crate::plan::Plan;
use crate::retry::RetryPolicy;
use crate::throttle::ThrottleKey;

/// Terminal + in-progress run status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, RunStatus::Running)
    }
}

/// Why a run ended in [`RunStatus::Failed`] — kept distinct so run history (5.7)
/// can flag an upstream bug (`InvalidInput`) apart from a genuine terminal error
/// or an exhausted retry budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    /// A node returned `terminal` and no error path caught it.
    Terminal,
    /// A node kept returning `retryable`/`rate-limited` past its budget.
    RetryExhausted,
    /// A node returned `invalid-input` (never retried) with no error path.
    InvalidInput,
}

/// The recorded failure of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub node: String,
    pub kind: FailKind,
    pub detail: ErrorDetail,
}

/// One pending unit of work: a node to run with the payload that entered it.
#[derive(Debug, Clone, PartialEq)]
struct Token {
    node: String,
    payload: Value,
}

/// The node currently being executed (or awaiting a scheduled retry).
#[derive(Debug, Clone, PartialEq)]
struct Active {
    node: String,
    payload: Value,
    /// 0 on first execution, incremented per retry.
    attempt: u32,
    /// Monotonic-ms deadline before which this node must not be re-dispatched
    /// (0 = ready now). Set when a retry is scheduled.
    retry_until_ms: u64,
    /// The shared-throttle key to coordinate on before the next dispatch (set on
    /// a `rate-limited` retry).
    throttle: Option<ThrottleKey>,
}

/// One run's mutable state. Opaque; inspect via the accessors.
#[derive(Debug, Clone, PartialEq)]
pub struct RunState {
    run_id: String,
    status: RunStatus,
    frontier: VecDeque<Token>,
    current: Option<Active>,
    step_seq: u64,
    result: Value,
    failure: Option<Failure>,
}

impl RunState {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
    pub fn status(&self) -> RunStatus {
        self.status
    }
    /// Count of successfully-completed node steps — the checkpoint key the driver
    /// persists (and compares on resume).
    pub fn step_seq(&self) -> u64 {
        self.step_seq
    }
    /// The payload of the last node that completed successfully (the run result on
    /// completion).
    pub fn result(&self) -> &Value {
        &self.result
    }
    /// The recorded failure, if the run failed.
    pub fn failure(&self) -> Option<&Failure> {
        self.failure.as_ref()
    }
}

/// What the driver should do next.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Run this node, then feed its outcome back via [`Plan::apply`].
    Dispatch(Dispatch),
    /// The next node is a scheduled retry not yet due: coordinate on `throttle`
    /// (if any) and sleep until `until_ms`, then call [`Plan::next`] again.
    Wait {
        node: String,
        until_ms: u64,
        throttle: Option<ThrottleKey>,
    },
    /// The run reached a terminal status.
    Done(RunStatus),
}

/// A single node execution the driver must perform. Mirrors the runner-owned
/// fields of `wamn:node`'s `run-context`.
#[derive(Debug, Clone, PartialEq)]
pub struct Dispatch {
    pub node: String,
    pub node_type: String,
    pub config: Value,
    pub credential: Option<String>,
    /// The payload entering this node — the trigger payload at `entry`, otherwise
    /// the upstream node's output (unchanged across retries of this node).
    pub payload: Value,
    /// 0 on first execution, incremented per retry.
    pub attempt: u32,
    /// Stable across retries of this node in this run — forward to external
    /// systems that support idempotency headers.
    pub idempotency_key: String,
    /// Remaining time budget for this node, if the flow set one.
    pub deadline_ms: Option<u64>,
}

impl<'f> Plan<'f> {
    /// Start a run: the entry node holds the trigger payload.
    pub fn start(&self, run_id: impl Into<String>, input: Value) -> RunState {
        let mut frontier = VecDeque::new();
        frontier.push_back(Token {
            node: self.entry().id.clone(),
            payload: input,
        });
        RunState {
            run_id: run_id.into(),
            status: RunStatus::Running,
            frontier,
            current: None,
            step_seq: 0,
            result: Value::Null,
            failure: None,
        }
    }

    /// Decide the next [`Step`] from the current state. Promotes the next frontier
    /// token to the active slot when idle; a due active node dispatches, a
    /// not-yet-due retry waits, an empty frontier completes the run.
    pub fn next(&self, state: &mut RunState, now_ms: u64) -> Step {
        if state.status.is_terminal() {
            return Step::Done(state.status);
        }
        if state.current.is_none() {
            match state.frontier.pop_front() {
                Some(tok) => {
                    state.current = Some(Active {
                        node: tok.node,
                        payload: tok.payload,
                        attempt: 0,
                        retry_until_ms: 0,
                        throttle: None,
                    });
                }
                None => {
                    state.status = RunStatus::Completed;
                    return Step::Done(RunStatus::Completed);
                }
            }
        }
        let a = state.current.as_ref().expect("current set above");
        if now_ms < a.retry_until_ms {
            return Step::Wait {
                node: a.node.clone(),
                until_ms: a.retry_until_ms,
                throttle: a.throttle.clone(),
            };
        }
        Step::Dispatch(self.build_dispatch(state, a))
    }

    fn build_dispatch(&self, state: &RunState, a: &Active) -> Dispatch {
        // The node is guaranteed present: the entry token and every enqueued
        // edge target resolve against the validated flow.
        let node = self.node(&a.node).expect("active node in flow");
        Dispatch {
            node: a.node.clone(),
            node_type: node.node_type.clone(),
            config: node.config.clone(),
            credential: node.credential.clone(),
            payload: a.payload.clone(),
            attempt: a.attempt,
            idempotency_key: format!("{}:{}", state.run_id, a.node),
            deadline_ms: node.config.get("deadline-ms").and_then(Value::as_u64),
        }
    }

    /// Fold a node's outcome into the run: advance on success, schedule a retry,
    /// route to the error path, or fail — all decided mechanically from the
    /// outcome variant. `dispatch` is the [`Dispatch`] whose node just ran.
    pub fn apply(
        &self,
        state: &mut RunState,
        dispatch: &Dispatch,
        outcome: NodeOutcome,
        now_ms: u64,
    ) {
        // Defensive: only act while running and on the active node.
        if state.status.is_terminal() {
            return;
        }
        let attempt = state
            .current
            .as_ref()
            .filter(|a| a.node == dispatch.node)
            .map(|a| a.attempt)
            .unwrap_or(dispatch.attempt);

        match outcome {
            NodeOutcome::Success { payload, port } => {
                state.current = None;
                state.step_seq += 1;
                state.result = payload.clone();
                self.enqueue_successors(state, &dispatch.node, &port, payload);
            }
            NodeOutcome::Error(NodeError::Retryable(detail)) => {
                let policy = RetryPolicy::from_config(&dispatch.config);
                if policy.may_retry(attempt) {
                    self.schedule_retry(state, now_ms + policy.backoff_ms(attempt), None);
                } else {
                    self.error_or_fail(state, &dispatch.node, detail, FailKind::RetryExhausted);
                }
            }
            NodeOutcome::Error(NodeError::RateLimited(rl)) => {
                let policy = RetryPolicy::from_config(&dispatch.config);
                if policy.may_retry(attempt) {
                    let delay = rl
                        .retry_after_ms
                        .unwrap_or_else(|| policy.backoff_ms(attempt));
                    let key = ThrottleKey::new(
                        dispatch.node_type.clone(),
                        dispatch.credential.clone(),
                        rl.target_host.clone(),
                    );
                    self.schedule_retry(state, now_ms + delay, Some(key));
                } else {
                    self.error_or_fail(state, &dispatch.node, rl.detail, FailKind::RetryExhausted);
                }
            }
            NodeOutcome::Error(NodeError::Terminal(detail)) => {
                self.error_or_fail(state, &dispatch.node, detail, FailKind::Terminal);
            }
            NodeOutcome::Error(NodeError::InvalidInput(detail)) => {
                // Never retried, regardless of budget.
                self.error_or_fail(state, &dispatch.node, detail, FailKind::InvalidInput);
            }
            NodeOutcome::Error(NodeError::Cancelled) => {
                state.current = None;
                state.status = RunStatus::Cancelled;
            }
        }
    }

    /// Enqueue the edges leaving `node` on `port`, each carrying `payload`.
    fn enqueue_successors(&self, state: &mut RunState, node: &str, port: &str, payload: Value) {
        let edges = self.successors(node, port);
        for edge in edges {
            state.frontier.push_back(Token {
                node: edge.to.clone(),
                payload: payload.clone(),
            });
        }
    }

    /// Mark the active node for a retry at `until_ms` (keeping the same input
    /// payload), coordinating on `throttle` if set.
    fn schedule_retry(&self, state: &mut RunState, until_ms: u64, throttle: Option<ThrottleKey>) {
        if let Some(a) = state.current.as_mut() {
            a.attempt += 1;
            a.retry_until_ms = until_ms;
            a.throttle = throttle;
        }
    }

    /// Route the failed node to its error path if one exists; otherwise end the
    /// run as failed with the given `kind`.
    fn error_or_fail(&self, state: &mut RunState, node: &str, detail: ErrorDetail, kind: FailKind) {
        state.current = None;
        let error_edges = self.successors(node, crate::outcome::ERROR_PORT);
        if error_edges.is_empty() {
            state.status = RunStatus::Failed;
            state.failure = Some(Failure {
                node: node.to_string(),
                kind,
                detail,
            });
        } else {
            let payload = detail.to_error_payload();
            for edge in error_edges {
                state.frontier.push_back(Token {
                    node: edge.to.clone(),
                    payload: payload.clone(),
                });
            }
        }
    }

    /// Drive a run to a terminal status synchronously, for callers that own a
    /// clock and a node dispatcher. `sleep_until` is invoked for a not-yet-due
    /// retry (a driver enforcing the shared throttle would gate the returned key
    /// here); a single-run helper may simply advance its clock. `dispatch` runs
    /// one node and returns its outcome.
    pub fn drive(
        &self,
        state: &mut RunState,
        mut now: impl FnMut() -> u64,
        mut sleep_until: impl FnMut(u64, Option<&ThrottleKey>),
        mut dispatch: impl FnMut(&Dispatch) -> NodeOutcome,
    ) -> RunStatus {
        loop {
            match self.next(state, now()) {
                Step::Done(status) => return status,
                Step::Wait {
                    until_ms, throttle, ..
                } => sleep_until(until_ms, throttle.as_ref()),
                Step::Dispatch(d) => {
                    let outcome = dispatch(&d);
                    self.apply(state, &d, outcome, now());
                }
            }
        }
    }
}

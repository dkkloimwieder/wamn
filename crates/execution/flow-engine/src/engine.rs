//! The reducer: a pure, synchronous state machine that walks one run through the
//! [`Plan`]. Every effect — dispatching a node, sleeping for a backoff,
//! recording run history — belongs to the driver; the engine only decides the
//! next [`Step`] and folds a [`NodeOutcome`] into [`ExecutionState`].
//!
//! ## Driver loop
//! ```ignore
//! let mut st = plan.start(run_id, input);
//! loop {
//!     match plan.next(&mut st, now_ms()) {
//!         Step::Dispatch(d)  => { let o = run_node(&d); plan.apply(&mut st, &d, o, now_ms()); }
//!         Step::Reserved(r)  => { commit_reserved(&r); plan.apply_reserved(&mut st, &r); }
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
//! **sequentially** in frontier order; parallel frame scheduling needs a
//! separate design.
//! The reducer is single-shot and in-memory; it does not expose a durable
//! recovery or frontier-restoration contract.

use std::collections::{HashMap, VecDeque};

use serde_json::Value;
use wamn_flow::node_contract::{ErrorDetail, NodeError};
use wamn_flow::{EntryKind, FailConfig};

use crate::outcome::NodeOutcome;
use crate::plan::Plan;
use crate::retry::RetryPolicy;
use crate::throttle::ThrottleKey;

/// Terminal + in-progress run status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionStatus {
    Running,
    Completed,
    Failed,
}

impl ExecutionStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, ExecutionStatus::Running)
    }
}

/// Why a run ended in [`ExecutionStatus::Failed`] — kept distinct so run history (5.7)
/// can flag an upstream bug (`InvalidInput`) apart from a genuine terminal error
/// or an exhausted retry budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionFailureKind {
    /// A node returned `terminal` and no error path caught it.
    Terminal,
    /// A node kept returning `retryable`/`rate-limited` past its budget.
    RetryExhausted,
    /// A node returned `invalid-input` (never retried) with no error path.
    InvalidInput,
    /// The run spent its per-invocation node-execution budget
    /// ([`Plan::set_dispatch_budget`](crate::Plan::set_dispatch_budget)) — a
    /// permitted loop that never terminated (cjv.4). Unconditionally terminal:
    /// never routed to an error path, which could itself be part of the loop.
    RunawayBudget,
    /// The root run exhausted its maximum call-depth resource-safety floor
    /// across the in-memory frame stack. Unconditionally terminal: never routed
    /// to a node error path, which could recurse into another frame.
    DepthBudget,
    /// The root run exhausted its total-dispatched-node resource-safety floor
    /// across all in-memory frames. Unconditionally terminal: never routed to a
    /// node error path, which could consume more of the exhausted budget.
    DispatchBudget,
}

/// The recorded failure of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub node: String,
    pub kind: ExecutionFailureKind,
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
pub struct ExecutionState {
    run_id: String,
    status: ExecutionStatus,
    frontier: VecDeque<Token>,
    current: Option<Active>,
    step_seq: u64,
    /// Live dispatches [`Plan::next`] has handed out for this state — the
    /// per-invocation counter the dispatch budget (cjv.4) is checked against.
    dispatched: u64,
    /// COMPLETED visits per node (a success or an error-ROUTED emission; retries
    /// of one visit do not count) — the source of [`Dispatch::occurrence`], so a
    /// merge/loop node's Nth visit persists as its own `node_runs` row instead of
    /// colliding on occurrence 0 (wamn-03m / R24).
    visits: HashMap<String, u32>,
    /// Per-run context document. Successful emissions may replace it; error
    /// emissions never can.
    context: Value,
    result: Value,
    failure: Option<Failure>,
    caller: CallerState,
}

impl ExecutionState {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
    pub fn status(&self) -> ExecutionStatus {
        self.status
    }
    /// Count of successfully-completed node steps.
    pub fn step_seq(&self) -> u64 {
        self.step_seq
    }
    /// Live node executions dispatched for this state (the counter the dispatch
    /// budget is checked against).
    pub fn dispatched(&self) -> u64 {
        self.dispatched
    }
    /// The payload of the last node that completed successfully (the run result on
    /// completion).
    pub fn result(&self) -> &Value {
        &self.result
    }
    /// The per-run context document visible to the next dispatch.
    pub fn context(&self) -> &Value {
        &self.context
    }
    /// The recorded failure, if the run failed.
    pub fn failure(&self) -> Option<&Failure> {
        self.failure.as_ref()
    }
    /// Caller ownership as understood by the graph walk.
    pub fn caller_state(&self) -> CallerState {
        self.caller
    }
}

/// What the driver should do next.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Run this node, then feed its outcome back via [`Plan::apply`].
    Dispatch(Dispatch),
    /// Apply an engine-reserved graph transition. These nodes are never handed
    /// to the user node dispatcher. The driver applies the described boundary,
    /// then acknowledges it with [`Plan::apply_reserved`].
    Reserved(ReservedStep),
    /// The next node is a scheduled retry not yet due: coordinate on `throttle`
    /// (if any) and sleep until `until_ms`, then call [`Plan::next`] again. A
    /// This wait belongs to the current in-memory execution only.
    Wait {
        node: String,
        until_ms: u64,
        /// The attempt the pending retry will run as — the budget cursor a
        /// retry cursor for the current reducer state.
        attempt: u32,
        throttle: Option<ThrottleKey>,
    },
    /// The run reached a terminal status.
    Done(ExecutionStatus),
}

/// Caller ownership tracked independently from run terminality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerState {
    /// Event runs have no attached synchronous caller.
    None,
    /// A request caller is attached and has not received an outcome.
    Attached,
    /// The request caller outcome has been durably chosen.
    Released,
}

/// An engine-owned node transition. Repeated calls to [`Plan::next`] return the
/// same value within this execution state until the driver acknowledges it.
#[derive(Debug, Clone, PartialEq)]
pub enum ReservedStep {
    /// Synthetically complete the unique entry with the admitted payload.
    Entry {
        node: String,
        payload: Value,
        occurrence: u32,
    },
    /// End the run with the authored failure, releasing an attached request
    /// caller with the same body and status.
    Fail {
        node: String,
        payload: Value,
        occurrence: u32,
        code: String,
        message: Option<String>,
        status: u16,
    },
}

impl ReservedStep {
    pub fn node(&self) -> &str {
        match self {
            ReservedStep::Entry { node, .. } | ReservedStep::Fail { node, .. } => node,
        }
    }

    pub fn payload(&self) -> &Value {
        match self {
            ReservedStep::Entry { payload, .. } | ReservedStep::Fail { payload, .. } => payload,
        }
    }

    pub fn occurrence(&self) -> u32 {
        match self {
            ReservedStep::Entry { occurrence, .. } | ReservedStep::Fail { occurrence, .. } => {
                *occurrence
            }
        }
    }
}

/// A graph transition was applied against the wrong state boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    Terminal(ExecutionStatus),
    NoActiveNode,
    MismatchedNode { expected: String, actual: String },
    NotReserved(String),
    CallerAlreadyReleased,
    RespondWithoutCaller,
    InvalidRequestEmission,
    InvalidEventEmission,
    InvalidFailOutcome,
    InvalidContext(Value),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Terminal(status) => write!(f, "run is already terminal: {status:?}"),
            ApplyError::NoActiveNode => write!(f, "run has no active node"),
            ApplyError::MismatchedNode { expected, actual } => {
                write!(f, "active node is {expected:?}, not {actual:?}")
            }
            ApplyError::NotReserved(node) => write!(f, "node {node:?} is not engine-reserved"),
            ApplyError::CallerAlreadyReleased => write!(f, "caller was already released"),
            ApplyError::RespondWithoutCaller => write!(f, "respond requires a request caller"),
            ApplyError::InvalidRequestEmission => {
                write!(
                    f,
                    "request must emit its admitted input unchanged on main without context"
                )
            }
            ApplyError::InvalidEventEmission => {
                write!(
                    f,
                    "event must emit its externally admitted input unchanged on main without context"
                )
            }
            ApplyError::InvalidFailOutcome => {
                write!(f, "fail must return its authored terminal detail")
            }
            ApplyError::InvalidContext(value) => {
                write!(f, "run context replacement must be an object, got {value}")
            }
        }
    }
}

impl std::error::Error for ApplyError {}

/// A single node execution the driver must perform.
#[derive(Debug, Clone, PartialEq)]
pub struct Dispatch {
    pub node: String,
    pub node_type: String,
    pub config: Value,
    pub connection: Option<String>,
    pub credential: Option<String>,
    /// The payload entering this node — the trigger payload at `entry`, otherwise
    /// the upstream node's output (unchanged across retries of this node).
    pub payload: Value,
    /// Snapshot of the run context observed by this dispatch.
    pub context: Value,
    /// 0 on first execution, incremented per retry.
    pub attempt: u32,
    /// Which VISIT of this node in this run (0 = first): a merge runs once per
    /// arriving token and a loop revisits its nodes, each visit a distinct
    /// occurrence. Stable across retries of one visit; only a completed visit
    /// advances it (wamn-03m / R24).
    pub occurrence: u32,
    /// Remaining time budget for this node, if the flow set one.
    pub deadline_ms: Option<u64>,
}

impl Dispatch {
    /// Canonical logical input captured before an effect attempt is dispatched.
    ///
    /// The attempt protocol stores this document (or a reference to its bytes),
    /// preserving the exact context-dependent parameters observed by the
    /// dispatched attempt.
    pub fn attempt_input(&self) -> Value {
        serde_json::json!({
            "connection": self.connection,
            "config": self.config,
            "context": self.context,
            "input": self.payload,
        })
    }
}

impl<'f> Plan<'f> {
    /// Start a run: the entry node holds the trigger payload.
    pub fn start(&self, run_id: impl Into<String>, input: Value) -> ExecutionState {
        let mut frontier = VecDeque::new();
        frontier.push_back(Token {
            node: self.entry().id.clone(),
            payload: input,
        });
        ExecutionState {
            run_id: run_id.into(),
            status: ExecutionStatus::Running,
            frontier,
            current: None,
            step_seq: 0,
            dispatched: 0,
            visits: HashMap::new(),
            context: Value::Object(Default::default()),
            result: Value::Null,
            failure: None,
            caller: match self
                .entry()
                .entry_kind()
                .expect("validated entry has a reserved kind")
            {
                EntryKind::Request => CallerState::Attached,
                EntryKind::Event => CallerState::None,
            },
        }
    }

    /// Decide the next [`Step`] from the current state. Promotes the next frontier
    /// token to the active slot when idle; a due active node dispatches, a
    /// not-yet-due retry waits, an empty frontier completes the run. Each
    /// dispatch handed out counts against the per-invocation budget
    /// ([`Plan::set_dispatch_budget`]); once spent, the run fails
    /// [`ExecutionFailureKind::RunawayBudget`] — the runtime bound that keeps a permitted
    /// cycle from running forever (cjv.4).
    pub fn next(&self, state: &mut ExecutionState, now_ms: u64) -> Step {
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
                    if state.caller == CallerState::Attached {
                        state.status = ExecutionStatus::Failed;
                        state.failure = Some(Failure {
                            node: self.entry().id.clone(),
                            kind: ExecutionFailureKind::InvalidInput,
                            detail: ErrorDetail::coded(
                                "ambiguous-exhausted-outcome",
                                "request frontier exhausted before caller release",
                            ),
                        });
                        return Step::Done(ExecutionStatus::Failed);
                    }
                    state.status = ExecutionStatus::Completed;
                    return Step::Done(ExecutionStatus::Completed);
                }
            }
        }
        if let Some(step @ ReservedStep::Entry { .. }) = self.build_reserved(state) {
            return Step::Reserved(step);
        }
        {
            let a = state.current.as_ref().expect("current set above");
            if now_ms < a.retry_until_ms {
                return Step::Wait {
                    node: a.node.clone(),
                    until_ms: a.retry_until_ms,
                    attempt: a.attempt,
                    throttle: a.throttle.clone(),
                };
            }
        }
        if state.dispatched >= self.dispatch_budget {
            // Deliberately NOT error_or_fail: an error path could itself be
            // part of the loop, so the budget verdict is unconditionally
            // terminal.
            let node = state.current.take().expect("current set above").node;
            state.status = ExecutionStatus::Failed;
            state.failure = Some(Failure {
                node,
                kind: ExecutionFailureKind::RunawayBudget,
                detail: ErrorDetail::coded(
                    "runaway-budget",
                    format!(
                        "node-execution budget of {} spent without terminating",
                        self.dispatch_budget
                    ),
                ),
            });
            return Step::Done(ExecutionStatus::Failed);
        }
        state.dispatched += 1;
        let a = state.current.as_ref().expect("current set above");
        Step::Dispatch(self.build_dispatch(state, a))
    }

    fn build_dispatch(&self, state: &ExecutionState, a: &Active) -> Dispatch {
        // The node is guaranteed present: the entry token and every enqueued
        // edge target resolve against the validated flow.
        let node = self.node(&a.node).expect("active node in flow");
        let occurrence = state.visits.get(&a.node).copied().unwrap_or(0);
        Dispatch {
            node: a.node.clone(),
            node_type: node.node_type.clone(),
            config: node.config.clone(),
            connection: node.connection.clone(),
            credential: None,
            payload: a.payload.clone(),
            context: state.context.clone(),
            attempt: a.attempt,
            occurrence,
            deadline_ms: node.config.get("deadline-ms").and_then(Value::as_u64),
        }
    }

    fn build_reserved(&self, state: &ExecutionState) -> Option<ReservedStep> {
        let active = state
            .current
            .as_ref()
            .expect("current set before reserved check");
        let node = self
            .node(&active.node)
            .expect("active node in validated flow");
        let occurrence = state.visits.get(&active.node).copied().unwrap_or(0);
        match node.node_type.as_str() {
            "fail" => {
                let config: FailConfig =
                    serde_json::from_value(node.config.clone()).expect("validated fail config");
                Some(ReservedStep::Fail {
                    node: node.id.clone(),
                    payload: active.payload.clone(),
                    occurrence,
                    code: config.code,
                    message: config.message,
                    status: config.status,
                })
            }
            _ => None,
        }
    }

    /// Fold a node's outcome into the run: advance on success, schedule a retry,
    /// route to the error path, or fail — all decided mechanically from the
    /// outcome variant. `dispatch` is the [`Dispatch`] whose node just ran.
    pub fn apply(
        &self,
        state: &mut ExecutionState,
        dispatch: &Dispatch,
        outcome: NodeOutcome,
        now_ms: u64,
    ) -> Result<(), ApplyError> {
        if state.status.is_terminal() {
            return Err(ApplyError::Terminal(state.status));
        }
        let active = state.current.as_ref().ok_or(ApplyError::NoActiveNode)?;
        if active.node != dispatch.node {
            return Err(ApplyError::MismatchedNode {
                expected: active.node.clone(),
                actual: dispatch.node.clone(),
            });
        }
        if matches!(self.build_reserved(state), Some(ReservedStep::Entry { .. })) {
            return Err(ApplyError::NotReserved(dispatch.node.clone()));
        }
        validate_request_outcome(dispatch, &outcome)?;
        validate_event_outcome(dispatch, &outcome)?;
        validate_fail_outcome(dispatch, &outcome)?;
        if dispatch.node_type == "fail" {
            let NodeOutcome::Error(NodeError::Terminal(detail)) = outcome else {
                unreachable!("validated fail outcome is terminal")
            };
            if state.caller == CallerState::Attached {
                state.caller = CallerState::Released;
            }
            state.current = None;
            state.frontier.clear();
            state.step_seq += 1;
            *state.visits.entry(dispatch.node.clone()).or_default() += 1;
            state.status = ExecutionStatus::Failed;
            state.failure = Some(Failure {
                node: dispatch.node.clone(),
                kind: ExecutionFailureKind::Terminal,
                detail,
            });
            return Ok(());
        }
        let attempt = state
            .current
            .as_ref()
            .filter(|a| a.node == dispatch.node)
            .map(|a| a.attempt)
            .unwrap_or(dispatch.attempt);

        match outcome {
            NodeOutcome::Success {
                payload,
                port,
                context,
            } => {
                let releases_caller = dispatch.node_type == "respond";
                let completes_run =
                    releases_caller && self.successors(&dispatch.node, &port).is_empty();
                if releases_caller {
                    match state.caller {
                        CallerState::None => return Err(ApplyError::RespondWithoutCaller),
                        CallerState::Released => return Err(ApplyError::CallerAlreadyReleased),
                        CallerState::Attached => {}
                    }
                }
                if let Some(replacement) = context.as_ref()
                    && !replacement.is_object()
                {
                    return Err(ApplyError::InvalidContext(replacement.clone()));
                }
                state.current = None;
                state.step_seq += 1;
                *state.visits.entry(dispatch.node.clone()).or_default() += 1;
                state.result = payload.clone();
                if let Some(replacement) = context {
                    state.context = replacement;
                }
                self.enqueue_successors(state, &dispatch.node, &port, payload);
                if releases_caller {
                    state.caller = CallerState::Released;
                    if completes_run {
                        state.status = ExecutionStatus::Completed;
                    }
                }
            }
            NodeOutcome::Error(NodeError::Retryable(detail)) => {
                let policy = RetryPolicy::from_config(&dispatch.config);
                if policy.may_retry(attempt) {
                    self.schedule_retry(state, now_ms + policy.backoff_ms(attempt), None);
                } else {
                    self.error_or_fail(
                        state,
                        &dispatch.node,
                        detail,
                        ExecutionFailureKind::RetryExhausted,
                    );
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
                    self.error_or_fail(
                        state,
                        &dispatch.node,
                        rl.detail,
                        ExecutionFailureKind::RetryExhausted,
                    );
                }
            }
            NodeOutcome::Error(NodeError::Terminal(detail)) => {
                self.error_or_fail(
                    state,
                    &dispatch.node,
                    detail,
                    ExecutionFailureKind::Terminal,
                );
            }
            NodeOutcome::Error(NodeError::InvalidInput(detail)) => {
                // Never retried, regardless of budget.
                self.error_or_fail(
                    state,
                    &dispatch.node,
                    detail,
                    ExecutionFailureKind::InvalidInput,
                );
            }
        }
        Ok(())
    }

    /// Acknowledge an engine-reserved boundary.
    pub fn apply_reserved(
        &self,
        state: &mut ExecutionState,
        step: &ReservedStep,
    ) -> Result<(), ApplyError> {
        if state.status.is_terminal() {
            return Err(ApplyError::Terminal(state.status));
        }
        let active = state.current.as_ref().ok_or(ApplyError::NoActiveNode)?;
        if active.node != step.node() {
            return Err(ApplyError::MismatchedNode {
                expected: active.node.clone(),
                actual: step.node().to_string(),
            });
        }
        let expected = self
            .build_reserved(state)
            .ok_or_else(|| ApplyError::NotReserved(active.node.clone()))?;
        if expected != *step {
            return Err(ApplyError::MismatchedNode {
                expected: expected.node().to_string(),
                actual: step.node().to_string(),
            });
        }

        match step {
            ReservedStep::Entry { node, payload, .. } => {
                state.current = None;
                state.step_seq += 1;
                *state.visits.entry(node.clone()).or_default() += 1;
                state.result = payload.clone();
                self.enqueue_successors(state, node, crate::MAIN_PORT, payload.clone());
            }
            ReservedStep::Fail {
                node,
                code,
                message,
                ..
            } => {
                if state.caller == CallerState::Attached {
                    state.caller = CallerState::Released;
                }
                state.current = None;
                state.frontier.clear();
                state.step_seq += 1;
                *state.visits.entry(node.clone()).or_default() += 1;
                state.status = ExecutionStatus::Failed;
                state.failure = Some(Failure {
                    node: node.clone(),
                    kind: ExecutionFailureKind::Terminal,
                    detail: ErrorDetail::coded(
                        code.clone(),
                        message.clone().unwrap_or_else(|| code.clone()),
                    ),
                });
            }
        }
        Ok(())
    }

    /// Enqueue the edges leaving `node` on `port`, each carrying `payload`.
    fn enqueue_successors(
        &self,
        state: &mut ExecutionState,
        node: &str,
        port: &str,
        payload: Value,
    ) {
        let edges = self.successors(node, port);
        for edge in edges {
            state.frontier.push_back(Token {
                node: edge.to.clone(),
                payload: payload.clone(),
            });
        }
    }

    /// The error-ROUTE transition shared by all routed failures: clear the active
    /// slot, count the COMPLETED visit (an error-routed
    /// emission advances the node's occurrence exactly as a success does — R24),
    /// and enqueue the [`ERROR_PORT`](crate::outcome::ERROR_PORT) successors carrying
    /// `payload`. Deliberately leaves `step_seq` and `result` untouched: an error
    /// route is not a success completion (R26).
    fn route_error(&self, state: &mut ExecutionState, node: &str, payload: Value) {
        state.current = None;
        *state.visits.entry(node.to_string()).or_default() += 1;
        self.enqueue_successors(state, node, crate::outcome::ERROR_PORT, payload);
    }

    /// Mark the active node for a retry at `until_ms` (keeping the same input
    /// payload), coordinating on `throttle` if set.
    fn schedule_retry(
        &self,
        state: &mut ExecutionState,
        until_ms: u64,
        throttle: Option<ThrottleKey>,
    ) {
        if let Some(a) = state.current.as_mut() {
            a.attempt += 1;
            a.retry_until_ms = until_ms;
            a.throttle = throttle;
        }
    }

    /// Route the failed node to its error path if one exists; otherwise end the
    /// run as failed with the given `kind`.
    fn error_or_fail(
        &self,
        state: &mut ExecutionState,
        node: &str,
        detail: ErrorDetail,
        kind: ExecutionFailureKind,
    ) {
        state.current = None;
        let error_edges = self.successors(node, crate::outcome::ERROR_PORT);
        if error_edges.is_empty() {
            state.status = ExecutionStatus::Failed;
            state.failure = Some(Failure {
                node: node.to_string(),
                kind,
                detail,
            });
        } else {
            // An error-ROUTED emission is a COMPLETED visit (the driver persists
            // its `node_runs` row), so it advances the node's occurrence exactly
            // as a success does; a run-ending failure above does not (R26).
            self.route_error(state, node, detail.to_error_payload());
        }
    }

    /// Drive a run to a terminal status synchronously, for callers that own a
    /// clock and a node dispatcher. `sleep_until` is invoked for a not-yet-due
    /// retry (a driver enforcing the shared throttle would gate the returned key
    /// here); a single-run helper may simply advance its clock. `dispatch` runs
    /// one node and returns its outcome.
    pub fn drive(
        &self,
        state: &mut ExecutionState,
        mut now: impl FnMut() -> u64,
        mut sleep_until: impl FnMut(u64, Option<&ThrottleKey>),
        mut dispatch: impl FnMut(&Dispatch) -> NodeOutcome,
    ) -> ExecutionStatus {
        loop {
            match self.next(state, now()) {
                Step::Done(status) => return status,
                Step::Wait {
                    until_ms, throttle, ..
                } => sleep_until(until_ms, throttle.as_ref()),
                Step::Reserved(step) => {
                    self.apply_reserved(state, &step)
                        .expect("reserved step came from this state");
                }
                Step::Dispatch(d) => {
                    let outcome = dispatch(&d);
                    self.apply(state, &d, outcome, now())
                        .expect("dispatch came from this state");
                }
            }
        }
    }
}

/// Refuse any request emission that does not preserve the admitted input.
///
/// Drivers call this before recording a successful attempt; [`Plan::apply`]
/// repeats the check so all engine users share the invariant.
pub fn validate_request_outcome(
    dispatch: &Dispatch,
    outcome: &NodeOutcome,
) -> Result<(), ApplyError> {
    if dispatch.node_type != "request" {
        return Ok(());
    }
    match outcome {
        NodeOutcome::Success {
            payload,
            port,
            context,
        } if port == crate::MAIN_PORT && payload == &dispatch.payload && context.is_none() => {
            Ok(())
        }
        NodeOutcome::Error(_) => Ok(()),
        NodeOutcome::Success { .. } => Err(ApplyError::InvalidRequestEmission),
    }
}

/// Refuse any event emission that does not preserve the externally admitted input.
///
/// Drivers call this before recording a successful attempt; [`Plan::apply`]
/// repeats the check so all engine users share the invariant.
pub fn validate_event_outcome(
    dispatch: &Dispatch,
    outcome: &NodeOutcome,
) -> Result<(), ApplyError> {
    if dispatch.node_type != "event" {
        return Ok(());
    }
    match outcome {
        NodeOutcome::Success {
            payload,
            port,
            context,
        } if port == crate::MAIN_PORT && payload == &dispatch.payload && context.is_none() => {
            Ok(())
        }
        NodeOutcome::Error(_) => Ok(()),
        NodeOutcome::Success { .. } => Err(ApplyError::InvalidEventEmission),
    }
}

/// Refuse any fail result other than the exact authored terminal detail.
///
/// Drivers call this before entering the privileged transaction that
/// completes the attempt, releases an attached caller, and terminalizes the run.
/// [`Plan::apply`] repeats the check for direct engine users.
pub fn validate_fail_outcome(dispatch: &Dispatch, outcome: &NodeOutcome) -> Result<(), ApplyError> {
    if dispatch.node_type != "fail" {
        return Ok(());
    }
    let config: FailConfig =
        serde_json::from_value(dispatch.config.clone()).expect("validated fail config");
    let expected = ErrorDetail::coded(
        &config.code,
        config.message.as_deref().unwrap_or(&config.code),
    );
    match outcome {
        NodeOutcome::Error(NodeError::Terminal(detail)) if detail == &expected => Ok(()),
        _ => Err(ApplyError::InvalidFailOutcome),
    }
}

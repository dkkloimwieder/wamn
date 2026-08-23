//! The walk: a pure, synchronous state machine that carries one delivery
//! through a [`Wiring`]. Every effect — invoking a node, sleeping for a backoff —
//! belongs to the caller; the walk only decides the next [`Step`] and folds a
//! [`NodeOutcome`] into a [`Walk`].
//!
//! ## Walk model
//! A single-token BFS frontier. A node emits on a **port**; the walk enqueues the
//! edges leaving that port. Branch = several ports; merge = several edges into
//! one node (no join *barrier* — a merged node runs once per arriving token).
//! Fan-out (several edges from one port) runs **sequentially** in frontier
//! order. The walk is single-shot and in-memory; it exposes no durable recovery
//! or frontier-restoration contract.

use std::collections::{HashMap, VecDeque};

use serde_json::Value;

use crate::outcome::{ERROR_PORT, ErrorDetail, NodeError, NodeOutcome};
use crate::retry::{RetryPolicy, ThrottleKey};
use crate::terminal::{DEDUP_ID_FIELD, Terminal, Verdict};
use crate::wiring::Wiring;

/// One delivery entering the router: the unit of work a walk carries.
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    /// Caller-assigned id, carried through for correlation only.
    pub id: String,
    /// The payload admitted at the entry node.
    pub payload: Value,
    /// Whether a synchronous caller is waiting on this delivery: true for
    /// ingress paths 1 (hot HTTP) and 3 (`begin`/`wait`), false for path 2
    /// (streams). It is what makes [`Terminal::Respond`] answerable and what
    /// separates a legitimate [`Verdict::Discard`] from a stranded caller.
    pub caller_attached: bool,
}

/// Terminal + in-progress walk status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkStatus {
    Running,
    Completed,
    Failed,
    /// The active component stopped cooperatively without a failure or verdict.
    Cancelled,
}

impl WalkStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, WalkStatus::Running)
    }
}

/// Why a walk ended in [`WalkStatus::Failed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// A node returned `terminal` and no error edge caught it.
    Terminal,
    /// A node kept returning `retryable`/`rate-limited` past its budget.
    RetryExhausted,
    /// A node returned `invalid-input` (never retried) with no error edge.
    InvalidInput,
    /// The delivery spent its hop limit ([`Wiring::set_hop_limit`]) — a
    /// permitted loop that never terminated. Unconditionally terminal: never
    /// routed to an error edge, which could itself be part of the loop.
    HopLimit,
    /// The frontier emptied with a synchronous caller still waiting and no
    /// [`Terminal::Respond`] reached. Terminal rather than a
    /// [`Verdict::Discard`]: discarding here would hang the caller until its
    /// own timeout, reporting nothing about why.
    UnreleasedCaller,
    /// A [`Terminal::Emit`] node emitted an event with no
    /// [`DEDUP_ID_FIELD`] string in it, so there is no key the boundary could
    /// dedup the publish on. Routed to the node's error edge if it has one.
    MissingDedupId,
    /// A [`Terminal::Respond`] node was reached by a delivery with no caller
    /// attached ([`ApplyErrorKind::RespondWithoutCaller`]) — an authored wiring
    /// meeting an ingress path it does not fit.
    RespondWithoutCaller,
    /// A second terminal node emitted after this delivery already had a verdict
    /// ([`ApplyErrorKind::SecondVerdict`]). The first verdict stands and the
    /// second node is the failure.
    SecondVerdict,
}

/// The recorded failure of a walk.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub node: String,
    pub kind: FailureKind,
    pub detail: ErrorDetail,
}

/// One pending unit of work: a node to run with the payload that entered it.
#[derive(Debug, Clone, PartialEq)]
struct Token {
    node: String,
    /// `None` only for the wiring entry; routed tokens carry the edge's resolved
    /// target operation input.
    input_port: Option<String>,
    payload: Value,
}

/// The node currently being invoked (or awaiting a scheduled retry).
#[derive(Debug, Clone, PartialEq)]
struct Active {
    node: String,
    input_port: Option<String>,
    payload: Value,
    /// 0 on first execution, incremented per retry.
    attempt: u32,
    /// Monotonic-ms deadline before which this node must not be re-invoked
    /// (0 = ready now). Set when a retry is scheduled.
    retry_until_ms: u64,
    /// The shared-throttle key to coordinate on before the next invocation (set
    /// on a `rate-limited` retry).
    throttle: Option<ThrottleKey>,
}

/// One delivery's mutable walk state. Opaque; inspect via the accessors.
#[derive(Debug, Clone, PartialEq)]
pub struct Walk {
    delivery_id: String,
    status: WalkStatus,
    frontier: VecDeque<Token>,
    current: Option<Active>,
    step_seq: u64,
    /// Invocations [`Wiring::next`] has handed out for this walk — the counter
    /// the hop limit is checked against.
    hops: u64,
    /// COMPLETED visits per node (a success or an error-ROUTED emission; retries
    /// of one visit do not count) — the source of [`NodeCall::occurrence`], so a
    /// merge/loop node's Nth visit is distinguishable from its first.
    visits: HashMap<String, u32>,
    result: Value,
    failure: Option<Failure>,
    /// Whether this delivery arrived with a synchronous caller waiting
    /// ([`Delivery::caller_attached`]).
    caller_attached: bool,
    /// The verdict a terminal node recorded, or [`Verdict::Discard`] settled at
    /// frontier exhaustion. `None` until then, and on a failed walk.
    verdict: Option<Verdict>,
}

impl Walk {
    /// The id of the delivery being walked.
    pub fn delivery_id(&self) -> &str {
        &self.delivery_id
    }
    pub fn status(&self) -> WalkStatus {
        self.status
    }
    /// Count of successfully-completed node steps.
    pub fn step_seq(&self) -> u64 {
        self.step_seq
    }
    /// Invocations handed out for this walk (the counter the hop limit is
    /// checked against).
    pub fn hops(&self) -> u64 {
        self.hops
    }
    /// The payload of the last node that completed successfully (the delivery's
    /// result on completion).
    pub fn result(&self) -> &Value {
        &self.result
    }
    /// The recorded failure, if the walk failed.
    pub fn failure(&self) -> Option<&Failure> {
        self.failure.as_ref()
    }
    /// What this delivery ended up doing: `Some` once a terminal node has been
    /// reached or the frontier has emptied cleanly, `None` while running and on
    /// a failed or cancelled walk.
    pub fn verdict(&self) -> Option<&Verdict> {
        self.verdict.as_ref()
    }
}

/// What the caller should do next.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Invoke this node, then feed its outcome back via [`Wiring::apply`].
    Invoke(NodeCall),
    /// The next node is a scheduled retry not yet due: coordinate on `throttle`
    /// (if any) and wait until `until_ms`, then call [`Wiring::next`] again.
    Wait {
        node: String,
        until_ms: u64,
        /// The attempt the pending retry will run as.
        attempt: u32,
        throttle: Option<ThrottleKey>,
    },
    /// The walk reached a terminal status.
    Done(WalkStatus),
}

/// A single node invocation the caller must perform.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeCall {
    pub node: String,
    /// The target operation input selected by the incoming edge. `None` only
    /// for the wiring entry, which has no incoming edge.
    pub input_port: Option<String>,
    /// The component to acquire a pooled instance of.
    pub component: String,
    pub config: Value,
    pub connection: Option<String>,
    pub credential: Option<String>,
    /// The payload entering this node — the delivery payload at the entry,
    /// otherwise the upstream node's output (unchanged across retries).
    pub payload: Value,
    /// 0 on first execution, incremented per retry.
    pub attempt: u32,
    /// Which VISIT of this node in this delivery (0 = first): a merge runs once
    /// per arriving token and a loop revisits its nodes, each visit a distinct
    /// occurrence. Stable across retries of one visit; only a completed visit
    /// advances it.
    pub occurrence: u32,
    /// Remaining time budget for this node, if the wiring set one.
    pub deadline_ms: Option<u64>,
}

/// A walk transition was applied against the wrong state boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct ApplyError {
    kind: ApplyErrorKind,
}

/// Which boundary the transition broke.
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyErrorKind {
    /// The walk already reached this terminal status.
    Terminal(WalkStatus),
    /// Nothing is active to apply an outcome to.
    NoActiveNode,
    /// The outcome names a node the walk is not on.
    MismatchedNode { expected: String, actual: String },
    /// The named [`Terminal::Respond`] node emitted, but no caller is attached
    /// to this delivery — a wiring answering nobody.
    RespondWithoutCaller(String),
    /// The named terminal node emitted after this delivery already had a
    /// verdict. One delivery ends once.
    SecondVerdict(String),
}

impl ApplyError {
    /// Which boundary the transition broke.
    pub fn kind(&self) -> &ApplyErrorKind {
        &self.kind
    }

    /// The walk failure this refusal folds into, with its stable code, or `None`
    /// when it cannot be folded at all.
    ///
    /// The split is the whole point of the two groups.
    /// [`ApplyErrorKind::RespondWithoutCaller`] and
    /// [`ApplyErrorKind::SecondVerdict`] are an authored wiring meeting a
    /// delivery it does not fit. Both are DATA, so they end ONE delivery.
    /// The other three describe a driver feeding back an outcome the walk never
    /// handed out — a defect in the driver, which no wiring can recover from
    /// and which [`route`](crate::route) therefore still panics on.
    fn node_data_failure(&self) -> Option<(FailureKind, &'static str)> {
        match &self.kind {
            ApplyErrorKind::RespondWithoutCaller(_) => {
                Some((FailureKind::RespondWithoutCaller, "respond-without-caller"))
            }
            ApplyErrorKind::SecondVerdict(_) => {
                Some((FailureKind::SecondVerdict, "second-verdict"))
            }
            ApplyErrorKind::Terminal(_)
            | ApplyErrorKind::NoActiveNode
            | ApplyErrorKind::MismatchedNode { .. } => None,
        }
    }
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            ApplyErrorKind::Terminal(status) => write!(f, "walk is already terminal: {status:?}"),
            ApplyErrorKind::NoActiveNode => write!(f, "walk has no active node"),
            ApplyErrorKind::MismatchedNode { expected, actual } => {
                write!(f, "active node is {expected:?}, not {actual:?}")
            }
            ApplyErrorKind::RespondWithoutCaller(node) => {
                write!(f, "node {node:?} responds, but no caller is attached")
            }
            ApplyErrorKind::SecondVerdict(node) => {
                write!(f, "node {node:?} is a second terminal for this delivery")
            }
        }
    }
}

impl std::error::Error for ApplyError {}

impl Wiring {
    /// Start a walk: the entry node holds the delivery payload.
    pub fn start(&self, delivery: Delivery) -> Walk {
        let mut frontier = VecDeque::new();
        frontier.push_back(Token {
            node: self.entry().to_string(),
            input_port: None,
            payload: delivery.payload,
        });
        Walk {
            delivery_id: delivery.id,
            status: WalkStatus::Running,
            frontier,
            current: None,
            step_seq: 0,
            hops: 0,
            visits: HashMap::new(),
            result: Value::Null,
            failure: None,
            caller_attached: delivery.caller_attached,
            verdict: None,
        }
    }

    /// Decide the next [`Step`]. Promotes the next frontier token to the active
    /// slot when idle; a due active node is invoked, a not-yet-due retry waits,
    /// an empty frontier completes the walk. Each invocation handed out counts
    /// against the hop limit ([`Wiring::set_hop_limit`]); once spent, the walk
    /// fails [`FailureKind::HopLimit`] — the runtime bound that keeps a
    /// permitted cycle from running forever.
    pub fn next(&self, walk: &mut Walk, now_ms: u64) -> Step {
        if walk.status.is_terminal() {
            return Step::Done(walk.status);
        }
        if walk.current.is_none() {
            match walk.frontier.pop_front() {
                Some(tok) => {
                    walk.current = Some(Active {
                        node: tok.node,
                        input_port: tok.input_port,
                        payload: tok.payload,
                        attempt: 0,
                        retry_until_ms: 0,
                        throttle: None,
                    });
                }
                None => return Step::Done(self.settle(walk)),
            }
        }
        {
            let a = walk.current.as_ref().expect("current set above");
            if now_ms < a.retry_until_ms {
                return Step::Wait {
                    node: a.node.clone(),
                    until_ms: a.retry_until_ms,
                    attempt: a.attempt,
                    throttle: a.throttle.clone(),
                };
            }
        }
        if walk.hops >= self.hop_limit {
            // Deliberately NOT error_or_fail: an error edge could itself be part
            // of the loop, so the hop-limit verdict is unconditionally terminal.
            let node = walk.current.take().expect("current set above").node;
            walk.status = WalkStatus::Failed;
            walk.failure = Some(Failure {
                node,
                kind: FailureKind::HopLimit,
                detail: ErrorDetail::coded(
                    "hop-limit",
                    format!("hop limit of {} spent without terminating", self.hop_limit),
                ),
            });
            return Step::Done(WalkStatus::Failed);
        }
        walk.hops += 1;
        let a = walk.current.as_ref().expect("current set above");
        Step::Invoke(self.build_call(walk, a))
    }

    /// End a walk whose frontier emptied. A verdict already recorded by a
    /// terminal node stands; otherwise this is [`Verdict::Discard`] — unless a
    /// caller is still waiting, which is [`FailureKind::UnreleasedCaller`]. A
    /// cancelled walk never reaches settlement.
    fn settle(&self, walk: &mut Walk) -> WalkStatus {
        if walk.verdict.is_none() {
            if walk.caller_attached {
                walk.status = WalkStatus::Failed;
                walk.failure = Some(Failure {
                    node: self.entry().to_string(),
                    kind: FailureKind::UnreleasedCaller,
                    detail: ErrorDetail::coded(
                        "unreleased-caller",
                        "frontier emptied with a caller attached and no respond",
                    ),
                });
                return WalkStatus::Failed;
            }
            walk.verdict = Some(Verdict::Discard);
        }
        walk.status = WalkStatus::Completed;
        WalkStatus::Completed
    }

    fn build_call(&self, walk: &Walk, a: &Active) -> NodeCall {
        // The node is guaranteed present: the entry token and every enqueued
        // edge target resolve against the compiled wiring.
        let node = self.node(&a.node).expect("active node in wiring");
        let occurrence = walk.visits.get(&a.node).copied().unwrap_or(0);
        NodeCall {
            node: a.node.clone(),
            input_port: a.input_port.clone(),
            component: node.component.clone(),
            config: node.config.clone(),
            connection: node.connection.clone(),
            credential: None,
            payload: a.payload.clone(),
            attempt: a.attempt,
            occurrence,
            deadline_ms: node.config.get("deadline-ms").and_then(Value::as_u64),
        }
    }

    /// Fold a node's outcome into the walk: advance on success, schedule a
    /// retry, route to the error edge, or fail — all decided mechanically from
    /// the outcome variant. `call` is the [`NodeCall`] whose node just ran.
    pub fn apply(
        &self,
        walk: &mut Walk,
        call: &NodeCall,
        outcome: NodeOutcome,
        now_ms: u64,
    ) -> Result<(), ApplyError> {
        if walk.status.is_terminal() {
            return Err(ApplyError {
                kind: ApplyErrorKind::Terminal(walk.status),
            });
        }
        let active = walk.current.as_ref().ok_or(ApplyError {
            kind: ApplyErrorKind::NoActiveNode,
        })?;
        if active.node != call.node {
            return Err(ApplyError {
                kind: ApplyErrorKind::MismatchedNode {
                    expected: active.node.clone(),
                    actual: call.node.clone(),
                },
            });
        }
        let attempt = active.attempt;

        match outcome {
            NodeOutcome::Success { payload, port } => {
                // Decided before anything mutates, so a rejected verdict leaves
                // the walk exactly as it found it.
                let verdict = match self
                    .node(&call.node)
                    .and_then(|node| node.terminal.as_ref())
                {
                    None => None,
                    Some(terminal) => {
                        match terminal_verdict(walk, &call.node, terminal, &payload)? {
                            recorded @ Some(_) => recorded,
                            // An emit node that produced no dedup id published
                            // nothing, so it is a node data failure taking the
                            // ordinary error-edge route, not a verdict.
                            None => {
                                self.error_or_fail(
                                    walk,
                                    &call.node,
                                    ErrorDetail::coded(
                                        "missing-dedup-id",
                                        format!("emit node emitted no {DEDUP_ID_FIELD} string"),
                                    ),
                                    FailureKind::MissingDedupId,
                                );
                                return Ok(());
                            }
                        }
                    }
                };
                walk.current = None;
                walk.step_seq += 1;
                *walk.visits.entry(call.node.clone()).or_default() += 1;
                walk.result = payload.clone();
                if verdict.is_some() {
                    walk.verdict = verdict;
                }
                self.enqueue_successors(walk, &call.node, &port, payload);
            }
            NodeOutcome::Error(NodeError::Retryable(detail)) => {
                let policy = RetryPolicy::from_config(&call.config);
                if policy.may_retry(attempt) {
                    schedule_retry(walk, now_ms + policy.backoff_ms(attempt), None);
                } else {
                    self.error_or_fail(walk, &call.node, detail, FailureKind::RetryExhausted);
                }
            }
            NodeOutcome::Error(NodeError::RateLimited(rl)) => {
                let policy = RetryPolicy::from_config(&call.config);
                if policy.may_retry(attempt) {
                    let delay = rl
                        .retry_after_ms
                        .unwrap_or_else(|| policy.backoff_ms(attempt));
                    let key = ThrottleKey::new(
                        call.component.clone(),
                        call.credential.clone(),
                        rl.target_host.clone(),
                    );
                    schedule_retry(walk, now_ms + delay, Some(key));
                } else {
                    self.error_or_fail(walk, &call.node, rl.detail, FailureKind::RetryExhausted);
                }
            }
            NodeOutcome::Error(NodeError::Terminal(detail)) => {
                self.error_or_fail(walk, &call.node, detail, FailureKind::Terminal);
            }
            NodeOutcome::Error(NodeError::InvalidInput(detail)) => {
                // Never retried, regardless of budget.
                self.error_or_fail(walk, &call.node, detail, FailureKind::InvalidInput);
            }
            NodeOutcome::Cancelled => {
                walk.current = None;
                walk.status = WalkStatus::Cancelled;
            }
        }
        Ok(())
    }

    /// Fold an [`ApplyError`] that DATA caused into the walk: `node`'s error
    /// edge if it has one, otherwise a failed walk carrying the matching
    /// [`FailureKind`]. Gives the error back unchanged when it instead describes
    /// a driver feeding back an outcome the walk never handed out, which no
    /// wiring can recover from.
    ///
    /// [`Wiring::apply`] deliberately does not do this itself: its refusal is
    /// atomic, so a driver that can re-ask its component sees the walk exactly
    /// as it found it. Folding is the driver's decision, and
    /// [`route`](crate::route) — which hands out one call and feeds back its
    /// answer, so cannot re-ask anything — makes it.
    pub fn fail_on_node_data(
        &self,
        walk: &mut Walk,
        node: &str,
        error: ApplyError,
    ) -> Result<(), ApplyError> {
        let Some((kind, code)) = error.node_data_failure() else {
            return Err(error);
        };
        self.error_or_fail(
            walk,
            node,
            ErrorDetail::coded(code, error.to_string()),
            kind,
        );
        Ok(())
    }

    /// Enqueue the edges leaving `node` on `port`, each carrying `payload`.
    fn enqueue_successors(&self, walk: &mut Walk, node: &str, port: &str, payload: Value) {
        for edge in self.successors(node, port) {
            walk.frontier.push_back(Token {
                node: edge.to.clone(),
                input_port: Some(edge.to_port.clone()),
                payload: payload.clone(),
            });
        }
    }

    /// The error-ROUTE transition shared by all routed failures: clear the
    /// active slot, count the COMPLETED visit (an error-routed emission advances
    /// the node's occurrence exactly as a success does), and enqueue the
    /// [`ERROR_PORT`](crate::ERROR_PORT) successors carrying `payload`.
    /// Deliberately leaves `step_seq` and `result` untouched: an error route is
    /// not a success completion.
    fn route_error(&self, walk: &mut Walk, node: &str, payload: Value) {
        walk.current = None;
        *walk.visits.entry(node.to_string()).or_default() += 1;
        self.enqueue_successors(walk, node, ERROR_PORT, payload);
    }

    /// Route the failed node to its error edge if one exists; otherwise end the
    /// walk as failed with the given `kind`.
    fn error_or_fail(&self, walk: &mut Walk, node: &str, detail: ErrorDetail, kind: FailureKind) {
        walk.current = None;
        if self.successors(node, ERROR_PORT).next().is_none() {
            walk.status = WalkStatus::Failed;
            walk.failure = Some(Failure {
                node: node.to_string(),
                kind,
                detail,
            });
        } else {
            // An error-ROUTED emission is a COMPLETED visit, so it advances the
            // node's occurrence exactly as a success does; a walk-ending failure
            // above does not.
            self.route_error(walk, node, detail.to_error_payload());
        }
    }
}

/// The [`Verdict`] a terminal node's success records. `Ok(None)` means a
/// [`Terminal::Emit`] node emitted no [`DEDUP_ID_FIELD`] string — the one case
/// the caller turns into a routed failure instead. Pure: it decides, and the
/// caller applies.
fn terminal_verdict(
    walk: &Walk,
    node: &str,
    terminal: &Terminal,
    payload: &Value,
) -> Result<Option<Verdict>, ApplyError> {
    if walk.verdict.is_some() {
        return Err(ApplyError {
            kind: ApplyErrorKind::SecondVerdict(node.to_string()),
        });
    }
    match terminal {
        Terminal::Respond if !walk.caller_attached => Err(ApplyError {
            kind: ApplyErrorKind::RespondWithoutCaller(node.to_string()),
        }),
        Terminal::Respond => Ok(Some(Verdict::Respond {
            payload: payload.clone(),
            node_id: node.to_string(),
        })),
        Terminal::Emit { entity, operation } => Ok(payload
            .get(DEDUP_ID_FIELD)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(|id| Verdict::Emit {
                event: payload.clone(),
                dedup_id: id.to_string(),
                entity: entity.clone(),
                operation: *operation,
            })),
    }
}

/// Mark the active node for a retry at `until_ms` (keeping the same input
/// payload), coordinating on `throttle` if set.
fn schedule_retry(walk: &mut Walk, until_ms: u64, throttle: Option<ThrottleKey>) {
    if let Some(a) = walk.current.as_mut() {
        a.attempt += 1;
        a.retry_until_ms = until_ms;
        a.throttle = throttle;
    }
}

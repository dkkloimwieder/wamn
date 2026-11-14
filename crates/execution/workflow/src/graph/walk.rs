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

use super::outcome::{ErrorDetail, NodeError, NodeOutcome};
use super::retry::RetryPolicy;
use super::terminal::{DEDUP_ID_FIELD, Verdict};
use super::wiring::{DEFAULT_HOP_LIMIT, Wiring};
use std::sync::Arc;
use wamn_catalog::ERROR_PORT;
use wamn_catalog::{WiringNode, WiringTerminal as Terminal};

/// One delivery entering the router: the unit of work a walk carries.
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    /// Caller-assigned id, carried through for correlation only.
    /// The payload admitted at the entry node.
    pub payload: Value,
    /// Whether a synchronous caller waits for a terminal response.
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
pub enum FailureType {
    /// A node returned `terminal` and no error edge caught it.
    Terminal,
    /// A node kept returning `retryable`/`rate-limited` past its budget.
    RetryExhausted,
    /// A node returned `invalid-input` (never retried) with no error edge.
    InvalidInput,
    /// The delivery spent its hop limit (the fixed hop limit) — a
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
    /// attached (the respond-without-caller refusal) — an authored wiring
    /// meeting an ingress path it does not fit.
    RespondWithoutCaller,
    /// A second terminal node emitted after this delivery already had a verdict
    /// (the second-verdict refusal). The first verdict stands and the
    /// second node is the failure.
    SecondVerdict,
}

impl FailureType {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::RetryExhausted => "retry-exhausted",
            Self::InvalidInput => "invalid-input",
            Self::HopLimit => "hop-limit",
            Self::UnreleasedCaller => "unreleased-caller",
            Self::MissingDedupId => "missing-dedup-id",
            Self::RespondWithoutCaller => "respond-without-caller",
            Self::SecondVerdict => "second-verdict",
        }
    }
}

/// The recorded failure of a walk.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub node: String,
    pub type_: FailureType,
    pub detail: ErrorDetail,
}

/// One pending unit of work: a node to run with the payload that entered it.
#[derive(Debug, Clone, PartialEq)]
struct Token {
    node: String,
    /// `None` only for the wiring entry; routed tokens carry the edge's resolved
    /// target operation input.
    input_port: Option<String>,
    payload: Arc<Value>,
}

/// The node currently being invoked (or awaiting a scheduled retry).
#[derive(Debug, Clone, PartialEq)]
struct Active {
    node: String,
    input_port: Option<String>,
    payload: Arc<Value>,
    /// 0 on first execution, incremented per retry.
    attempt: u32,
    /// Monotonic-ms deadline before which this node must not be re-invoked
    /// (0 = ready now). Set when a retry is scheduled.
    retry_until_ms: u64,
}

/// One delivery's mutable walk state. Opaque; inspect via the accessors.
#[derive(Debug, Clone, PartialEq)]
pub struct Walk {
    status: WalkStatus,
    frontier: VecDeque<Token>,
    current: Option<Active>,
    /// Invocations [`Wiring::next`] has handed out for this walk — the counter
    /// the hop limit is validated against.
    hops: u64,
    /// COMPLETED visits per node (a success or an error-ROUTED emission; retries
    /// of one visit do not count) — the source of [`NodeCall::occurrence`], so a
    /// merge/loop node's Nth visit is distinguishable from its first.
    visits: HashMap<String, u32>,
    result: Arc<Value>,
    failure: Option<Failure>,
    /// Whether this delivery arrived with a synchronous caller waiting
    /// ([`Delivery::caller_attached`]).
    caller_attached: bool,
    /// The verdict a terminal node recorded, or [`Verdict::Discard`] settled at
    /// frontier exhaustion. `None` until then, and on a failed walk.
    verdict: Option<Verdict>,
}

impl Walk {
    pub(crate) fn into_outcome(self) -> super::Outcome {
        drop(self.frontier);
        drop(self.current);
        super::Outcome {
            status: self.status,
            result: Arc::unwrap_or_clone(self.result),
            failure: self.failure,
            hops: self.hops,
            verdict: self.verdict,
        }
    }
    /// Invocations handed out for this walk (the counter the hop limit is
    /// validated against).
    #[cfg(test)]
    pub fn hops(&self) -> u64 {
        self.hops
    }
    /// The payload of the last node that completed successfully (the delivery's
    /// result on completion).
    #[cfg(test)]
    pub fn result(&self) -> &Value {
        &self.result
    }
    /// The recorded failure, if the walk failed.
    #[cfg(test)]
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
#[derive(Debug, PartialEq)]
pub enum Step<'a> {
    /// Invoke this node, then feed its outcome back via [`Wiring::apply`].
    Invoke(NodeCall<'a>),
    /// Wait for the pending retry.
    Wait { until_ms: u64 },
    /// The walk reached a terminal status.
    Done(WalkStatus),
}

/// A single node invocation the caller must perform.
#[derive(Debug, PartialEq)]
pub struct NodeCall<'a> {
    pub node: String,
    /// The target operation input selected by the incoming edge. `None` only
    /// for the wiring entry, which has no incoming edge.
    pub input_port: Option<String>,
    pub definition: &'a WiringNode,
    /// The payload entering this node — the delivery payload at the entry,
    /// otherwise the upstream node's output (unchanged across retries).
    pub payload: Arc<Value>,
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
    type_: ApplyErrorType,
}

/// Which boundary the transition broke.
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyErrorType {
    /// The walk already reached this terminal status.
    Terminal(WalkStatus),
    /// Nothing is active to apply an outcome to.
    NoActiveNode,
    /// The outcome names a node the walk is not on.
    MismatchedNode { expected: String, actual: String },
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.type_ {
            ApplyErrorType::Terminal(status) => write!(f, "walk is already terminal: {status:?}"),
            ApplyErrorType::NoActiveNode => write!(f, "walk has no active node"),
            ApplyErrorType::MismatchedNode { expected, actual } => {
                write!(f, "active node is {expected:?}, not {actual:?}")
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
            payload: Arc::new(delivery.payload),
        });
        Walk {
            status: WalkStatus::Running,
            frontier,
            current: None,
            hops: 0,
            visits: HashMap::new(),
            result: Arc::new(Value::Null),
            failure: None,
            caller_attached: delivery.caller_attached,
            verdict: None,
        }
    }

    /// Decide the next [`Step`]. Promotes the next frontier token to the active
    /// slot when idle; a due active node is invoked, a not-yet-due retry waits,
    /// an empty frontier completes the walk. Each invocation handed out counts
    /// against the hop limit (the fixed hop limit); once spent, the walk
    /// fails [`FailureType::HopLimit`] — the runtime bound that keeps a
    /// permitted cycle from running forever.
    pub fn next(&self, walk: &mut Walk, now_ms: u64) -> Step<'_> {
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
                    });
                }
                None => return Step::Done(self.settle(walk)),
            }
        }
        {
            let a = walk.current.as_ref().expect("current set above");
            if now_ms < a.retry_until_ms {
                return Step::Wait {
                    until_ms: a.retry_until_ms,
                };
            }
        }
        if walk.hops >= DEFAULT_HOP_LIMIT {
            // Deliberately NOT error_or_fail: an error edge could itself be part
            // of the loop, so the hop-limit verdict is unconditionally terminal.
            let node = walk.current.take().expect("current set above").node;
            walk.status = WalkStatus::Failed;
            walk.failure = Some(Failure {
                node,
                type_: FailureType::HopLimit,
                detail: ErrorDetail::coded(
                    FailureType::HopLimit.code(),
                    format!("hop limit of {DEFAULT_HOP_LIMIT} spent without terminating"),
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
    /// caller is still waiting, which is [`FailureType::UnreleasedCaller`]. A
    /// cancelled walk never reaches settlement.
    fn settle(&self, walk: &mut Walk) -> WalkStatus {
        if walk.verdict.is_none() {
            if walk.caller_attached {
                walk.status = WalkStatus::Failed;
                walk.failure = Some(Failure {
                    node: self.entry().to_string(),
                    type_: FailureType::UnreleasedCaller,
                    detail: ErrorDetail::coded(
                        FailureType::UnreleasedCaller.code(),
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

    fn build_call<'a>(&'a self, walk: &Walk, a: &Active) -> NodeCall<'a> {
        // The node is guaranteed present: the entry token and every enqueued
        // edge target resolve against the compiled wiring.
        let node = self.node(&a.node).expect("active node in wiring");
        let occurrence = walk.visits.get(&a.node).copied().unwrap_or(0);
        NodeCall {
            node: a.node.clone(),
            input_port: a.input_port.clone(),
            definition: node,
            payload: Arc::clone(&a.payload),
            attempt: a.attempt,
            occurrence,
            deadline_ms: node.params.get("deadline-ms").and_then(Value::as_u64),
        }
    }

    /// Fold a node's outcome into the walk: advance on success, schedule a
    /// retry, route to the error edge, or fail — all decided mechanically from
    /// the outcome variant. `call` is the [`NodeCall`] whose node just ran.
    pub fn apply(
        &self,
        walk: &mut Walk,
        call: &NodeCall<'_>,
        outcome: NodeOutcome,
        now_ms: u64,
    ) -> Result<(), ApplyError> {
        if walk.status.is_terminal() {
            return Err(ApplyError {
                type_: ApplyErrorType::Terminal(walk.status),
            });
        }
        let active = walk.current.as_ref().ok_or(ApplyError {
            type_: ApplyErrorType::NoActiveNode,
        })?;
        if active.node != call.node {
            return Err(ApplyError {
                type_: ApplyErrorType::MismatchedNode {
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
                        // An emit node that produced no dedup id published
                        // nothing, so it is a node data failure taking the
                        // ordinary error-edge route, not a verdict.
                        let recorded = match terminal_verdict(walk, &call.node, terminal, &payload)
                        {
                            Ok(Some(verdict)) => verdict,
                            outcome => {
                                let type_ = match outcome {
                                    Ok(None) => FailureType::MissingDedupId,
                                    Err(type_) => type_,
                                    Ok(Some(_)) => unreachable!(),
                                };
                                self.error_or_fail(
                                    walk,
                                    &call.node,
                                    ErrorDetail::coded(type_.code(), type_.code()),
                                    type_,
                                );
                                return Ok(());
                            }
                        };
                        Some(recorded)
                    }
                };
                walk.current = None;
                *walk.visits.entry(call.node.clone()).or_default() += 1;
                let payload = Arc::new(payload);
                walk.result = Arc::clone(&payload);
                if verdict.is_some() {
                    walk.verdict = verdict;
                }
                self.enqueue_successors(walk, &call.node, &port, &payload);
            }
            NodeOutcome::Error(NodeError::Retryable(detail)) => {
                let policy = RetryPolicy::from_config(&call.definition.params);
                if policy.may_retry(attempt) {
                    schedule_retry(walk, now_ms.saturating_add(policy.backoff_ms(attempt)));
                } else {
                    self.error_or_fail(walk, &call.node, detail, FailureType::RetryExhausted);
                }
            }
            NodeOutcome::Error(NodeError::RateLimited(rl)) => {
                let policy = RetryPolicy::from_config(&call.definition.params);
                if policy.may_retry(attempt) {
                    let delay = rl
                        .retry_after_ms
                        .unwrap_or_else(|| policy.backoff_ms(attempt));
                    schedule_retry(walk, now_ms.saturating_add(delay));
                } else {
                    self.error_or_fail(walk, &call.node, rl.detail, FailureType::RetryExhausted);
                }
            }
            NodeOutcome::Error(NodeError::Terminal(detail)) => {
                self.error_or_fail(walk, &call.node, detail, FailureType::Terminal);
            }
            NodeOutcome::Error(NodeError::InvalidInput(detail)) => {
                // Never retried, regardless of budget.
                self.error_or_fail(walk, &call.node, detail, FailureType::InvalidInput);
            }
            NodeOutcome::Cancelled => {
                walk.current = None;
                walk.status = WalkStatus::Cancelled;
            }
        }
        Ok(())
    }

    /// Enqueue the edges leaving `node` on `port`, each carrying `payload`.
    fn enqueue_successors(&self, walk: &mut Walk, node: &str, port: &str, payload: &Arc<Value>) {
        for (edge, input_port) in self.successors(node, port) {
            walk.frontier.push_back(Token {
                node: edge.to.clone(),
                input_port: Some(input_port.to_owned()),
                payload: Arc::clone(payload),
            });
        }
    }

    /// The error-ROUTE transition shared by all routed failures: clear the
    /// active slot, count the COMPLETED visit (an error-routed emission advances
    /// the node's occurrence exactly as a success does), and enqueue the
    /// [`ERROR_PORT`](wamn_catalog::ERROR_PORT) successors carrying `payload`.
    /// Deliberately leaves `step_seq` and `result` untouched: an error route is
    /// not a success completion.
    fn route_error(&self, walk: &mut Walk, node: &str, payload: &Arc<Value>) {
        walk.current = None;
        *walk.visits.entry(node.to_string()).or_default() += 1;
        self.enqueue_successors(walk, node, ERROR_PORT, payload);
    }

    /// Route the failed node to its error edge if one exists; otherwise end the
    /// walk as failed with the given `type`.
    fn error_or_fail(&self, walk: &mut Walk, node: &str, detail: ErrorDetail, type_: FailureType) {
        walk.current = None;
        if self.successors(node, ERROR_PORT).next().is_none() {
            walk.status = WalkStatus::Failed;
            walk.failure = Some(Failure {
                node: node.to_string(),
                type_,
                detail,
            });
        } else {
            // An error-ROUTED emission is a COMPLETED visit, so it advances the
            // node's occurrence exactly as a success does; a walk-ending failure
            // above does not.
            self.route_error(walk, node, &Arc::new(detail.to_error_payload()));
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
) -> Result<Option<Verdict>, FailureType> {
    if walk.verdict.is_some() {
        return Err(FailureType::SecondVerdict);
    }
    match terminal {
        Terminal::Respond if !walk.caller_attached => Err(FailureType::RespondWithoutCaller),
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
                node_id: node.to_string(),
            })),
    }
}

/// Mark the active node for a retry at `until_ms` (keeping the same input
/// payload).
fn schedule_retry(walk: &mut Walk, until_ms: u64) {
    if let Some(a) = walk.current.as_mut() {
        a.attempt += 1;
        a.retry_until_ms = until_ms;
    }
}

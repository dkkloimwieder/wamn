//! WALK-1..6 as plain functions over an observed walk — D8a, Part D rung 4 of
//! `docs/poc/deterministic-testing-spec.md`.
//!
//! No cargo feature and no flag: these are ordinary library functions the walk
//! simulator and any other driver call after every `apply`. Wiring them into the
//! pilot executor as tripwires is D8b, deferred to Phase 3.
//!
//! A [`Walk`] keeps no history — it is one delivery's live state and nothing
//! else — but four of the six invariants are about a TRAJECTORY: a merge running
//! once per arriving token, a retry budget spent across attempts, a wait landing
//! ahead of the clock, a terminal outcome matching the failure it produced. So
//! the driver accumulates a [`WalkTrace`] as it drives, and [`check`] reads the
//! wiring, the live walk and that trace together.

use std::collections::BTreeMap;

use crate::outcome::{ERROR_PORT, NodeError, NodeOutcome};
use crate::retry::RetryPolicy;
use crate::walk::{FailureKind, NodeCall, Step, Walk, WalkStatus};
use crate::wiring::Wiring;

/// An invariant that did not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The invariant id, for traceability: `"WALK-1"` … `"WALK-6"`.
    pub invariant: &'static str,
    /// What was observed instead.
    pub detail: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} violated: {}", self.invariant, self.detail)
    }
}

impl std::error::Error for Violation {}

fn violated(invariant: &'static str, detail: String) -> Violation {
    Violation { invariant, detail }
}

/// Which outcome a driver fed back for one call — the routing-relevant variant
/// with the payload dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeKind {
    Success,
    Retryable,
    RateLimited,
    Terminal,
    InvalidInput,
    Cancelled,
}

fn outcome_kind(outcome: &NodeOutcome) -> OutcomeKind {
    match outcome {
        NodeOutcome::Success { .. } => OutcomeKind::Success,
        NodeOutcome::Cancelled => OutcomeKind::Cancelled,
        NodeOutcome::Error(NodeError::Retryable(_)) => OutcomeKind::Retryable,
        NodeOutcome::Error(NodeError::RateLimited(_)) => OutcomeKind::RateLimited,
        NodeOutcome::Error(NodeError::Terminal(_)) => OutcomeKind::Terminal,
        NodeOutcome::Error(NodeError::InvalidInput(_)) => OutcomeKind::InvalidInput,
    }
}

/// One completed call: the coordinate [`Wiring::next`] handed out and the
/// outcome the driver fed back for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub node: String,
    pub attempt: u32,
    pub occurrence: u32,
    pub outcome: OutcomeKind,
}

/// What a driver observed while walking one delivery.
///
/// `arrivals` is the driver's own model of the frontier — one count per token
/// enqueued into a node — kept independently of the walk so WALK-3 compares two
/// answers instead of restating one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WalkTrace {
    calls: Vec<Call>,
    arrivals: BTreeMap<String, u32>,
    /// Every [`Step::Wait`] as `(clock when asked, deadline returned)`.
    waits: Vec<(u64, u64)>,
    /// Invocations handed out after the walk had already reached a terminal
    /// status.
    invokes_after_done: u32,
}

impl WalkTrace {
    /// A trace of a walk about to start: the entry node holds its one arriving
    /// token.
    pub fn new(wiring: &Wiring) -> WalkTrace {
        WalkTrace {
            arrivals: BTreeMap::from([(wiring.entry().to_string(), 1)]),
            ..WalkTrace::default()
        }
    }

    /// Record the [`Step`] [`Wiring::next`] returned, with the walk's status
    /// before it was asked and the clock reading it was asked at.
    pub fn stepped(&mut self, status_before: WalkStatus, step: &Step, now_ms: u64) {
        match step {
            Step::Invoke(_) if status_before.is_terminal() => self.invokes_after_done += 1,
            Step::Wait { until_ms, .. } => self.waits.push((now_ms, *until_ms)),
            Step::Invoke(_) | Step::Done(_) => {}
        }
    }

    /// Record the outcome fed back for `call`, and the tokens that outcome puts
    /// on the frontier.
    pub fn applied(&mut self, wiring: &Wiring, call: &NodeCall, outcome: &NodeOutcome) {
        self.calls.push(Call {
            node: call.node.clone(),
            attempt: call.attempt,
            occurrence: call.occurrence,
            outcome: outcome_kind(outcome),
        });
        let port = match outcome {
            NodeOutcome::Success { port, .. } => port.as_str(),
            NodeOutcome::Cancelled => return,
            // A retry within budget re-runs the same token: nothing arrives
            // anywhere. Past budget the failure takes the error route below.
            NodeOutcome::Error(NodeError::Retryable(_) | NodeError::RateLimited(_))
                if RetryPolicy::from_config(&call.config).may_retry(call.attempt) =>
            {
                return;
            }
            NodeOutcome::Error(_) => ERROR_PORT,
        };
        for edge in wiring.successors(&call.node, port) {
            *self.arrivals.entry(edge.to.clone()).or_default() += 1;
        }
    }
}

/// The walk under check: the graph, the live walk, and what the driver observed.
#[derive(Debug, Clone, Copy)]
pub struct WalkState<'a> {
    pub wiring: &'a Wiring,
    pub walk: &'a Walk,
    pub trace: &'a WalkTrace,
}

/// Check WALK-1..6 against one observed walk. Cheap enough to call after every
/// `apply`.
pub fn check(state: &WalkState<'_>) -> Result<(), Violation> {
    walk_1_hop_limit(state)?;
    walk_2_no_invoke_after_done(state)?;
    walk_3_once_per_arriving_token(state)?;
    walk_4_retry_budget(state)?;
    walk_5_wait_never_in_the_past(state)?;
    walk_6_terminal_outcome_matches(state)
}

/// WALK-1: the walk ends — the invocations handed out never pass the hop limit.
fn walk_1_hop_limit(state: &WalkState<'_>) -> Result<(), Violation> {
    let limit = state.wiring.hop_limit();
    if state.walk.hops() > limit {
        return Err(violated(
            "WALK-1",
            format!(
                "the walk counted {} hops against a limit of {limit}",
                state.walk.hops()
            ),
        ));
    }
    let handed = u64::try_from(state.trace.calls.len()).unwrap_or(u64::MAX);
    if handed > limit {
        return Err(violated(
            "WALK-1",
            format!("{handed} invocations were handed out against a limit of {limit}"),
        ));
    }
    Ok(())
}

/// WALK-2: after `Done`, `next` never returns `Invoke`.
fn walk_2_no_invoke_after_done(state: &WalkState<'_>) -> Result<(), Violation> {
    if state.trace.invokes_after_done > 0 {
        return Err(violated(
            "WALK-2",
            format!(
                "{} invocations were handed out after the walk was terminal",
                state.trace.invokes_after_done
            ),
        ));
    }
    Ok(())
}

/// WALK-3: a merged node runs once per arriving token, never more.
///
/// A node's Nth visit carries `occurrence == N - 1`, so the highest occurrence
/// seen plus one is how many visits it started. That may lag the tokens that
/// arrived — a walk can fail with work still on the frontier — but never lead.
fn walk_3_once_per_arriving_token(state: &WalkState<'_>) -> Result<(), Violation> {
    let mut started: BTreeMap<&str, u32> = BTreeMap::new();
    for call in &state.trace.calls {
        let visits = started.entry(call.node.as_str()).or_default();
        *visits = (*visits).max(call.occurrence + 1);
    }
    for (node, visits) in started {
        let arrived = state.trace.arrivals.get(node).copied().unwrap_or(0);
        if visits > arrived {
            return Err(violated(
                "WALK-3",
                format!("node {node:?} started {visits} visits for {arrived} arriving tokens"),
            ));
        }
    }
    Ok(())
}

/// WALK-4: retries per node never pass the retry budget — and only a retryable
/// or rate-limited outcome ever produces one.
fn walk_4_retry_budget(state: &WalkState<'_>) -> Result<(), Violation> {
    for call in &state.trace.calls {
        let budget = node_budget(state.wiring, &call.node);
        if call.attempt + 1 > budget {
            return Err(violated(
                "WALK-4",
                format!(
                    "node {:?} ran attempt {} against a budget of {budget} attempts",
                    call.node, call.attempt
                ),
            ));
        }
    }
    for pair in state.trace.calls.windows(2) {
        let (previous, next) = (&pair[0], &pair[1]);
        let retried = previous.node == next.node
            && previous.occurrence == next.occurrence
            && next.attempt == previous.attempt + 1;
        if retried && !matches!(previous.outcome, OutcomeKind::Retryable | OutcomeKind::RateLimited)
        {
            return Err(violated(
                "WALK-4",
                format!(
                    "node {:?} was retried after a {:?} outcome",
                    previous.node, previous.outcome
                ),
            ));
        }
    }
    Ok(())
}

/// The total attempts a node's own config allows, defaulting as
/// [`RetryPolicy::from_config`] does.
fn node_budget(wiring: &Wiring, node: &str) -> u32 {
    wiring.node(node).map_or(1, |wired| {
        RetryPolicy::from_config(&wired.config).max_attempts.max(1)
    })
}

/// WALK-5: a `Wait` is never earlier than the clock it was asked at.
fn walk_5_wait_never_in_the_past(state: &WalkState<'_>) -> Result<(), Violation> {
    for (asked_at, until_ms) in &state.trace.waits {
        if until_ms < asked_at {
            return Err(violated(
                "WALK-5",
                format!("a wait until {until_ms} was returned at clock {asked_at}"),
            ));
        }
    }
    Ok(())
}

/// WALK-6: a `Terminal` outcome ends the walk with the matching verdict —
/// `FailureKind::Terminal` naming that node when nothing caught it, and a walk
/// that carries on down the error edge when something did.
///
/// [`FailureKind::HopLimit`] is the one failure this invariant ignores in the
/// caught case: `apply` never produces it, `Wiring::next` does, so a walk that
/// routed the terminal outcome down an error edge and then spent its last hop
/// is exactly the behaviour this invariant expects.
fn walk_6_terminal_outcome_matches(state: &WalkState<'_>) -> Result<(), Violation> {
    let Some(last) = state.trace.calls.last() else {
        return Ok(());
    };
    if last.outcome != OutcomeKind::Terminal {
        return Ok(());
    }
    let caught = state
        .wiring
        .successors(&last.node, ERROR_PORT)
        .next()
        .is_some();
    match (caught, state.walk.failure()) {
        (false, Some(failure))
            if state.walk.status() == WalkStatus::Failed
                && failure.kind == FailureKind::Terminal
                && failure.node == last.node =>
        {
            Ok(())
        }
        (false, recorded) => Err(violated(
            "WALK-6",
            format!(
                "node {:?} returned terminal with no error edge, but the walk is {:?} with {recorded:?}",
                last.node,
                state.walk.status()
            ),
        )),
        (true, None) => Ok(()),
        (true, Some(failure)) if failure.kind == FailureKind::HopLimit => Ok(()),
        (true, Some(failure)) => Err(violated(
            "WALK-6",
            format!(
                "node {:?} has an error edge, but its terminal outcome failed the walk {:?}",
                last.node, failure.kind
            ),
        )),
    }
}

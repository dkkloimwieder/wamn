//! RUN-* as plain functions over the pure queue decision — D8a, Part D rung 4
//! of `docs/poc/deterministic-testing-spec.md`.
//!
//! No cargo feature and no flag: ordinary library functions the pure decision
//! tests call after every step. Wiring them into the pilot executor as tripwires
//! is D8b, deferred to Phase 3.
//!
//! ## What Phase 0 can hold, and what it cannot
//!
//! Spec B2 names seven invariants. Four of them are properties of the *decision*
//! this crate owns, so they are here:
//!
//! - RUN-2: at most one runner holds a live lease on one run.
//! - RUN-3: a claimable run is claimed unless the batch limit ran out.
//! - RUN-4: a claim never writes `attempts` past `max_attempts`, and a row the
//!   janitor orphans is never one the claim path would take.
//! - RUN-7: a run is claimable again only once its lease has expired.
//!
//! Three are not, and are deliberately absent rather than approximated. RUN-1 (a
//! run admitted is in `runs` or the queue), RUN-5 (a terminal row never changes)
//! and RUN-6 (one producer key admits once) are all statements about rows in a
//! database across a sequence of writes. The pure layer holds one
//! [`QueueEntry`] view and no `runs` row, no producer key and no write history,
//! so nothing here could evaluate them. They belong to the seeded event
//! scheduler over a throwaway PostgreSQL — spec B2's D2, which Phase 2 opens
//! once D2b puts `$now` on every time-dependent statement.

use std::collections::BTreeSet;

use crate::queue::{
    ClaimPlan, ClaimState, JanitorVerdict, Millis, QueueEntry, claim_state, janitor_verdict,
    lease_live,
};

/// An invariant that did not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The invariant id, for traceability: `"RUN-2"`, `"RUN-3"`, `"RUN-4"`,
    /// `"RUN-7"`.
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

/// One decision's worth of queue state: the candidate rows, the plan taken over
/// them (when a claim was planned), and the clock and grace window in force.
#[derive(Debug, Clone, Copy)]
pub struct QueueState<'a> {
    /// The candidate rows the decision was taken over.
    pub rows: &'a [QueueEntry],
    /// The plan `plan_claim` returned for `rows`, when the step took one.
    pub plan: Option<&'a ClaimPlan>,
    /// The batch limit `plan` was taken with. Ignored when `plan` is `None`.
    pub limit: usize,
    /// The janitor grace window in force.
    pub grace: Millis,
    /// The instant the step was taken at.
    pub now: Millis,
}

/// Check the RUN-* invariants the pure decision layer can evaluate. Cheap enough
/// to call after every step.
pub fn check(state: &QueueState<'_>) -> Result<(), Violation> {
    run_2_one_live_lease(state)?;
    run_3_claimable_is_claimed(state)?;
    run_4_budget_and_janitor(state)?;
    run_7_claimable_only_past_expiry(state)
}

/// RUN-2: at any time, at most one runner holds a live lease on one run — and a
/// claim never takes a run another runner still holds, nor the same run twice.
fn run_2_one_live_lease(state: &QueueState<'_>) -> Result<(), Violation> {
    let mut leased: BTreeSet<&str> = BTreeSet::new();
    for row in state.rows {
        if lease_live(state.now, row.lease_expires_at) && !leased.insert(row.run_id.as_str()) {
            return Err(violated(
                "RUN-2",
                format!("run {:?} carries two live leases at {}", row.run_id, state.now),
            ));
        }
    }
    let Some(plan) = state.plan else {
        return Ok(());
    };
    let mut claimed: BTreeSet<&str> = BTreeSet::new();
    for claim in &plan.claimed {
        if !claimed.insert(claim.run_id.as_str()) {
            return Err(violated(
                "RUN-2",
                format!("run {:?} was claimed twice by one plan", claim.run_id),
            ));
        }
        if leased.contains(claim.run_id.as_str()) {
            return Err(violated(
                "RUN-2",
                format!(
                    "run {:?} was claimed while another runner's lease was live",
                    claim.run_id
                ),
            ));
        }
    }
    Ok(())
}

/// RUN-3: a run is delivered at least once unless its attempts are exhausted —
/// so a plan that did not fill its batch left no `Ready` row behind.
fn run_3_claimable_is_claimed(state: &QueueState<'_>) -> Result<(), Violation> {
    let Some(plan) = state.plan else {
        return Ok(());
    };
    if plan.claimed.len() >= state.limit {
        return Ok(());
    }
    let claimed: BTreeSet<&str> = plan
        .claimed
        .iter()
        .map(|claim| claim.run_id.as_str())
        .collect();
    for row in state.rows {
        if claim_state(row, state.now) == ClaimState::Ready
            && !claimed.contains(row.run_id.as_str())
        {
            return Err(violated(
                "RUN-3",
                format!(
                    "run {:?} was ready and left unclaimed with {} of {} slots taken",
                    row.run_id,
                    plan.claimed.len(),
                    state.limit
                ),
            ));
        }
    }
    Ok(())
}

/// RUN-4: attempts never pass `max_attempts`, and the janitor orphans exactly
/// the exhausted rows — never one the claim path would still take.
fn run_4_budget_and_janitor(state: &QueueState<'_>) -> Result<(), Violation> {
    if let Some(plan) = state.plan {
        for claim in &plan.claimed {
            let Some(row) = state.rows.iter().find(|row| row.run_id == claim.run_id) else {
                continue;
            };
            if claim.attempts > row.max_attempts {
                return Err(violated(
                    "RUN-4",
                    format!(
                        "run {:?} was claimed at attempt {} against a budget of {}",
                        claim.run_id, claim.attempts, row.max_attempts
                    ),
                ));
            }
        }
    }
    for row in state.rows {
        if janitor_verdict(row, state.now, state.grace) != JanitorVerdict::Orphaned {
            continue;
        }
        if row.attempts < row.max_attempts {
            return Err(violated(
                "RUN-4",
                format!(
                    "the janitor orphaned run {:?} with {} of {} attempts spent",
                    row.run_id, row.attempts, row.max_attempts
                ),
            ));
        }
        if claim_state(row, state.now) == ClaimState::Ready {
            return Err(violated(
                "RUN-4",
                format!(
                    "run {:?} is orphaned by the janitor and claimable by a runner at once",
                    row.run_id
                ),
            ));
        }
    }
    Ok(())
}

/// RUN-7: after a crash the run is claimable again only once the lease expires —
/// the dead owner's visibility timeout is what holds it, and nothing else may
/// shorten it.
fn run_7_claimable_only_past_expiry(state: &QueueState<'_>) -> Result<(), Violation> {
    for row in state.rows {
        if claim_state(row, state.now) == ClaimState::Ready
            && lease_live(state.now, row.lease_expires_at)
        {
            return Err(violated(
                "RUN-7",
                format!(
                    "run {:?} is claimable at {} while its lease runs to {:?}",
                    row.run_id, state.now, row.lease_expires_at
                ),
            ));
        }
    }
    Ok(())
}

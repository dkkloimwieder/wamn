//! Claim eligibility and reclaim classification for the global FIFO queue.

use super::model::{Millis, QueueEntry};

/// A queue row's claimability at a given instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimState {
    /// Visible, not live-leased, and within its redelivery budget — a claim takes it.
    Ready,
    /// A runner holds a live lease — a claim skips it (until the lease expires).
    Leased,
    /// `available_at` is in the future — queue-parked/backed-off, not yet visible.
    Parked,
    /// Visible, holding an **expired** lease, and the redelivery budget is spent
    /// (`attempts >= max_attempts`) — the claim path leaves it for the janitor to
    /// retire to `infrastructure-failure`. A budget-spent row with a *released*
    /// (NULL) lease is NOT `Exhausted`: a NULL lease is proof the last owner
    /// intentionally queue-parked the run (was alive), never crash evidence, so
    /// it is `Ready` and wakes (wamn-fqg.7).
    Exhausted,
}

/// The production action selected before any catalog or plan read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductionClaimClass {
    /// A never-leased or intentionally released (queue-parked) queue row.
    /// Neither carries a release record for the claim to reset: a never-leased
    /// run was never claimed, and the park that released the lease cleared the
    /// pair itself, because releasing a lease is what reopens claimability
    /// (wamn-0h0g.15.82).
    Ordinary,
    /// An expired lease with no immutable effect attempt.
    ExpiredPreEffect,
    /// An expired lease with at least one immutable effect attempt.
    ExpiredWithAttempt,
}

impl ProductionClaimClass {
    /// A stable name for structured logs and tests.
    pub fn as_str(self) -> &'static str {
        match self {
            ProductionClaimClass::Ordinary => "ordinary",
            ProductionClaimClass::ExpiredPreEffect => "expired-pre-effect",
            ProductionClaimClass::ExpiredWithAttempt => "expired-with-attempt",
        }
    }
}

impl ClaimState {
    /// A stable name for logs/metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            ClaimState::Ready => "ready",
            ClaimState::Leased => "leased",
            ClaimState::Parked => "parked",
            ClaimState::Exhausted => "exhausted",
        }
    }
}

/// Classify a row's claimability at `now`. A future `available_at` is `Parked`; a
/// live lease is `Leased`; a budget-spent row still holding an **expired** lease is
/// `Exhausted` (left for the janitor); anything else — including a budget-spent row
/// whose lease was *released* (NULL) by a queue park — is `Ready`. This is the
/// no-effect view of the production predicate (`available_at <= now AND (lease_expires_at IS NULL OR
/// lease_expires_at <= now) AND (attempts < max_attempts OR lease_expires_at IS
/// NULL)`). The `lease_expires_at IS NULL` disjunct is what wakes a queue-parked
/// budget-spent run (wamn-fqg.7): reaching the `Exhausted` branch means the lease is
/// expired (crash evidence), while a NULL lease is a released one (an intentional queue wait) and stays
/// `Ready`. Use [`production_claim_state`] when immutable effect evidence is
/// available; that evidence makes an expired row eligible for non-executing
/// terminalization regardless of crash budget.
pub fn claim_state(entry: &QueueEntry, now: Millis) -> ClaimState {
    production_claim_state(entry, false, now)
}

/// Classify eligibility with immutable effect-attempt evidence.
///
/// An effect attempt makes an expired row eligible even after its crash budget
/// is spent so production can remove it as `effect-uncertain`. It does not make
/// a live lease or a future queue wait claimable.
pub fn production_claim_state(
    entry: &QueueEntry,
    has_effect_attempt: bool,
    now: Millis,
) -> ClaimState {
    if entry.available_at > now {
        ClaimState::Parked
    } else if entry.lease_expires_at.is_some_and(|t| t > now) {
        ClaimState::Leased
    } else if entry.lease_expires_at.is_some()
        && entry.attempts >= entry.max_attempts
        && !has_effect_attempt
    {
        ClaimState::Exhausted
    } else {
        ClaimState::Ready
    }
}

/// Classify a row already selected by the production eligibility predicate.
pub fn classify_production_claim(
    had_prior_lease: bool,
    has_effect_attempt: bool,
) -> ProductionClaimClass {
    if has_effect_attempt {
        ProductionClaimClass::ExpiredWithAttempt
    } else if had_prior_lease {
        ProductionClaimClass::ExpiredPreEffect
    } else {
        ProductionClaimClass::Ordinary
    }
}

/// Whether a claim would take this row right now.
pub fn is_claimable(entry: &QueueEntry, now: Millis) -> bool {
    claim_state(entry, now) == ClaimState::Ready
}

/// The result of claiming one row: the new lease deadline and the attempt count
/// the production lease grant writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimed {
    pub tenant_id: String,
    pub run_id: String,
    /// `attempts` after the claim. Counts **crash evidence only**: bumped iff the
    /// claim reclaimed an *expired* lease (the prior owner died holding the run);
    /// a first claim and a queue-park→wake re-claim (the queue park releases the
    /// lease) are free.
    pub attempts: i32,
    /// `now + lease_ttl` — the new visibility timeout.
    pub lease_expires_at: Millis,
}

/// What a batch claim of up to `limit` rows would take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimPlan {
    pub claimed: Vec<Claimed>,
}

/// Model a global FIFO lease grant over a candidate row set: take claimable rows
/// in `(available_at, stream_seq, run_id)` order, up to `limit`, each with a fresh
/// lease deadline `now + lease_ttl`. Only reclaiming an expired lease bumps its
/// attempt count; first claims and released queue waits remain free.
/// The real SQL additionally `SKIP LOCKED`s rows another transaction already
/// holds; this pure function models eligibility, ordering, and lease arithmetic.
///
/// The sort mirrors the SQL's `ORDER BY (available_at, stream_seq, run_id)` (E4):
/// the numeric `stream_seq` tiebreak rides AHEAD of `run_id`, so evt runs
/// (`<flow>:evt:<seq>`, real stream positions minted by the materializer,
/// l5i9.17) dispatch by numeric stream position; every non-CDC enqueue writes
/// `stream_seq = 0`, keeping the tiebreak inert there.
pub fn plan_claim(
    candidates: &[QueueEntry],
    now: Millis,
    limit: usize,
    lease_ttl: Millis,
) -> ClaimPlan {
    let mut eligible: Vec<&QueueEntry> =
        candidates.iter().filter(|e| is_claimable(e, now)).collect();
    eligible.sort_by(|a, b| {
        a.available_at
            .cmp(&b.available_at)
            .then_with(|| a.stream_seq.cmp(&b.stream_seq))
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
    let claimed = eligible
        .into_iter()
        .take(limit)
        .map(|e| Claimed {
            tenant_id: e.tenant_id.clone(),
            run_id: e.run_id.clone(),
            // Crash evidence only: a claimable row's lease is NULL or expired, so a
            // present lease IS an expired one — the prior owner died holding the run.
            attempts: e.attempts + i32::from(e.lease_expires_at.is_some()),
            lease_expires_at: now + lease_ttl,
        })
        .collect();
    ClaimPlan { claimed }
}

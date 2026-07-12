//! Claim eligibility — the pure decision behind the `FOR UPDATE SKIP LOCKED`
//! claim. [`claim_state`] classifies one row; [`plan_claim`] models which rows a
//! batch claim would take, in the same order the SQL uses ([`crate::claim_batch_sql`]),
//! so the SQL's behaviour is unit-testable without a database.

use crate::model::{Millis, QueueEntry};

/// A queue row's claimability at a given instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimState {
    /// Visible, not live-leased, and within its redelivery budget — a claim takes it.
    Ready,
    /// A runner holds a live lease — a claim skips it (until the lease expires).
    Leased,
    /// `available_at` is in the future — delayed/parked/backed-off, not yet visible.
    Parked,
    /// Visible and lease-expired but the redelivery budget is spent
    /// (`attempts >= max_attempts`) — the claim path leaves it for the janitor to
    /// retire to `infrastructure-failure`.
    Exhausted,
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
/// live lease is `Leased`; a lease-expired row whose budget is spent is `Exhausted`
/// (left for the janitor); anything else is `Ready`. Matches the `claim_batch_sql`
/// predicate (`available_at <= now AND (lease_expires_at IS NULL OR
/// lease_expires_at <= now) AND attempts < max_attempts`).
pub fn claim_state(entry: &QueueEntry, now: Millis) -> ClaimState {
    if entry.available_at > now {
        ClaimState::Parked
    } else if entry.lease_expires_at.is_some_and(|t| t > now) {
        ClaimState::Leased
    } else if entry.attempts >= entry.max_attempts {
        ClaimState::Exhausted
    } else {
        ClaimState::Ready
    }
}

/// Whether a claim would take this row right now.
pub fn is_claimable(entry: &QueueEntry, now: Millis) -> bool {
    claim_state(entry, now) == ClaimState::Ready
}

/// The result of claiming one row: the new lease deadline and the attempt count
/// the `claim_batch_sql` UPDATE writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimed {
    pub tenant_id: String,
    pub run_id: String,
    /// `attempts` after the claim. Counts **crash evidence only**: bumped iff the
    /// claim reclaimed an *expired* lease (the prior owner died holding the run);
    /// a first claim and a park→wake re-claim (park releases the lease) are free.
    pub attempts: i32,
    /// `now + lease_ttl` — the new visibility timeout.
    pub lease_expires_at: Millis,
}

/// What a batch claim of up to `limit` rows would take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimPlan {
    pub claimed: Vec<Claimed>,
}

/// Model a `claim_batch_sql(limit)` over a candidate row set: take the claimable
/// **unpartitioned** rows in `(available_at, run_id)` order (the SQL's `ORDER BY`),
/// up to `limit`, each with a fresh lease deadline `now + lease_ttl` and its attempt
/// bumped. Partitioned rows (`partition_key` set) are skipped — the global claim's
/// `partition_key IS NULL` guard leaves them to the per-partition ownership path
/// ([`crate::plan_partition_claim`]). The real SQL additionally `SKIP LOCKED`s rows
/// another transaction already holds — a concurrency property only the live queue
/// (queuebench) exercises; this models the eligibility + ordering + limit a single
/// claimer sees.
pub fn plan_claim(
    candidates: &[QueueEntry],
    now: Millis,
    limit: usize,
    lease_ttl: Millis,
) -> ClaimPlan {
    let mut eligible: Vec<&QueueEntry> = candidates
        .iter()
        .filter(|e| e.partition_key.is_none() && is_claimable(e, now))
        .collect();
    eligible.sort_by(|a, b| {
        a.available_at
            .cmp(&b.available_at)
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

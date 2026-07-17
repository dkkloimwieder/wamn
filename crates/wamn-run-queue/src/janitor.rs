//! The janitor — the pure orphan-detection behind the `janitor_sweep_sql` sweep.
//! A run whose runner died mid-dispatch leaves a `dispatched`/`running` run with
//! an expired lease. If retries remain it is simply reclaimable; once the
//! redelivery budget is spent and a grace period has elapsed, the janitor gives
//! up and the run becomes `infrastructure-failure` (its queue row removed).

use crate::model::{Millis, PartitionPolicy, QueueEntry};

/// What the janitor should do with a queue row at `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JanitorVerdict {
    /// A runner still holds a live lease — leave it.
    Live,
    /// The lease has expired (or was never taken) but retries remain — another
    /// replica can reclaim it. Not the janitor's business.
    Reclaimable,
    /// The lease expired more than `grace` ago and the redelivery budget is spent
    /// — give up: the run is `infrastructure-failure`, the queue row removed.
    Orphaned,
    /// Orphan-shaped, but the row belongs to a `blocking`-policy partition (D20):
    /// the janitor must NOT reap it — the row is the only record that later runs
    /// of the key must wait, so it stays and **wedges** the key until an operator
    /// intervenes. The run's status is left untouched.
    Wedged,
}

/// Classify a queue row for the janitor. `grace` is how long past lease expiry to
/// wait before declaring an exhausted row orphaned (absorbs clock skew / a late
/// heartbeat). Mirrors the `janitor_sweep_sql` predicate, including its D20
/// exemption: an orphan-shaped row of a `blocking`-policy partition is
/// [`JanitorVerdict::Wedged`], never reaped.
pub fn janitor_verdict(entry: &QueueEntry, now: Millis, grace: Millis) -> JanitorVerdict {
    match entry.lease_expires_at {
        Some(t) if t > now => JanitorVerdict::Live,
        Some(t) if t + grace <= now && entry.attempts >= entry.max_attempts => {
            if entry.partition_key.is_some() && entry.partition_policy == PartitionPolicy::Blocking
            {
                JanitorVerdict::Wedged
            } else {
                JanitorVerdict::Orphaned
            }
        }
        _ => JanitorVerdict::Reclaimable,
    }
}

/// The orphaned rows in a set — exactly the rows `janitor_sweep_sql` marks
/// `infrastructure-failure` and dequeues.
pub fn orphans(rows: &[QueueEntry], now: Millis, grace: Millis) -> Vec<&QueueEntry> {
    rows.iter()
        .filter(|e| matches!(janitor_verdict(e, now, grace), JanitorVerdict::Orphaned))
        .collect()
}

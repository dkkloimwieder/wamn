//! Per-partition ownership — the mechanism that makes `partitioned(key)` flows
//! dispatch **in-order per key across replicas** (5.14 scaling; the ordering
//! *semantics* are 5.11). A run's `partition_key` groups it into an ordered stream;
//! a replica **leases a partition** (a `partition_owner` row) and, while it holds
//! that lease, is the only replica that dispatches the partition's runs — head-first,
//! one in flight at a time — so ordering within the key is preserved under
//! horizontal scaling. If the owner dies its partition lease expires and another
//! replica reacquires the whole key and continues in order (crash-safe failover).
//!
//! This is the pure decision layer behind the SQL builders
//! ([`crate::acquire_partitions_sql`], [`crate::claim_partition_head_sql`]): the
//! run-level claim only ever takes the *head* of a partition the caller owns, and
//! `partitioned` runs are claimed **only** through this path — the global
//! [`crate::claim_batch_sql`] skips them (`partition_key IS NULL`), so a partitioned
//! run can never be dispatched out of order by the global claim. Like the rest of
//! the crate these are functions of `(rows, now)`; the concurrent arbitration of a
//! contested partition (the `INSERT … ON CONFLICT` in the acquire SQL, the
//! `FOR UPDATE SKIP LOCKED` in the claim SQL) is a property only the live queue
//! (`queuebench`) exercises.

use std::collections::HashSet;

use crate::claim::{ClaimPlan, Claimed, is_claimable};
use crate::lease::lease_live;
use crate::model::{Millis, PartitionOwner, QueueEntry};

/// Whether a partition lease is still held at `now` (its deadline is in the future).
pub fn partition_lease_live(owner: &PartitionOwner, now: Millis) -> bool {
    owner.lease_expires_at > now
}

/// Which partitions a replica would acquire: the distinct keys that have at least
/// one claimable run right now **and** are not currently held by a live partition
/// lease (unowned, or the owner's lease expired = failover), in key order, up to
/// `limit`. Models the candidate selection of [`crate::acquire_partitions_sql`];
/// which replica *wins* a partition two of them race for is decided by the
/// `INSERT … ON CONFLICT … WHERE lease_expires_at <= now()` arbitration, a live-queue
/// property (`queuebench`).
pub fn plan_acquire(
    rows: &[QueueEntry],
    owners: &[PartitionOwner],
    now: Millis,
    limit: usize,
) -> Vec<String> {
    let live_owned: HashSet<&str> = owners
        .iter()
        .filter(|o| partition_lease_live(o, now))
        .map(|o| o.partition_key.as_str())
        .collect();

    // A key is a candidate iff it has a claimable run and is not live-owned.
    let mut keys: Vec<&str> = rows
        .iter()
        .filter(|e| is_claimable(e, now))
        .filter_map(|e| e.partition_key.as_deref())
        .filter(|key| !live_owned.contains(key))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys.into_iter().take(limit).map(String::from).collect()
}

/// The head runs a replica would claim across the partitions it owns: for each
/// owned key with no run currently in flight, the earliest-`(available_at, run_id)`
/// run that is ready now (its head) — then the globally-earliest such heads across
/// owned partitions, up to `limit`, each with a fresh run lease. Models
/// [`crate::claim_partition_head_sql`]. `owned` is the set of partition keys the
/// replica holds a live lease on.
///
/// The two guarantees this encodes — **one in flight per partition** (a partition
/// with any live-leased run yields no head) and **head-first** (only the earliest
/// ready run of a partition is taken) — are exactly what preserve per-key order:
/// the next run of a key is claimable only once the current one has completed and
/// dequeued.
pub fn plan_partition_claim(
    rows: &[QueueEntry],
    owned: &HashSet<&str>,
    now: Millis,
    limit: usize,
    lease_ttl: Millis,
) -> ClaimPlan {
    let mut heads: Vec<&QueueEntry> = Vec::new();
    for &key in owned {
        let part: Vec<&QueueEntry> = rows
            .iter()
            .filter(|e| e.partition_key.as_deref() == Some(key))
            .collect();
        // One-in-flight: any live-leased run in this partition blocks the whole key.
        if part.iter().any(|e| lease_live(now, e.lease_expires_at)) {
            continue;
        }
        // Head = the earliest ready run (by available_at, then run_id).
        if let Some(head) = part
            .iter()
            .copied()
            .filter(|e| is_claimable(e, now))
            .min_by(|a, b| ord_key(a).cmp(&ord_key(b)))
        {
            heads.push(head);
        }
    }
    heads.sort_by(|a, b| ord_key(a).cmp(&ord_key(b)));
    let claimed = heads
        .into_iter()
        .take(limit)
        .map(|e| Claimed {
            tenant_id: e.tenant_id.clone(),
            run_id: e.run_id.clone(),
            attempts: e.attempts + 1,
            lease_expires_at: now + lease_ttl,
        })
        .collect();
    ClaimPlan { claimed }
}

/// The dispatch-order key: `(available_at, run_id)`, matching every SQL `ORDER BY`.
fn ord_key(e: &QueueEntry) -> (Millis, &str) {
    (e.available_at, e.run_id.as_str())
}

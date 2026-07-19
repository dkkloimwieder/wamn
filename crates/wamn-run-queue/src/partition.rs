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
use crate::model::{Millis, PartitionOwner, PartitionPolicy, QueueEntry};

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
/// owned key with no run currently in flight, the ready run no sibling blocks
/// under the row's policy (its head) — then the globally-earliest such heads
/// across owned partitions, up to `limit`, each with a fresh run lease. Models
/// [`crate::claim_partition_head_sql`]. `owned` is the set of partition keys the
/// replica holds a live lease on.
///
/// The guarantees this encodes — **one in flight per partition** (a partition
/// with any live-leased run yields no head) and **head-first** under the row's
/// [`PartitionPolicy`] (D20) — are what preserve per-key order. Under
/// `blocking` (the default) the head is the earliest run in the key's *stream
/// order* `(enqueued_at, run_id)` — a backed-off/parked/exhausted earlier run
/// still blocks, so the key waits (or wedges) rather than reorder. Under
/// `leapfrog` only an earlier *currently-ready* sibling blocks, in
/// `(available_at, run_id)` order — an unavailable head yields the key.
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
        // Head = the claimable run no sibling blocks under its policy.
        if let Some(head) = part
            .iter()
            .copied()
            .filter(|c| is_claimable(c, now))
            .filter(|c| !part.iter().any(|b| blocks(b, c, now)))
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
            // Crash evidence only, mirroring `claim_batch_sql`: bump iff this claim
            // reclaims an expired lease. A parked head is re-claimed on every wake,
            // so this path would burn the redelivery budget fastest otherwise.
            attempts: e.attempts + i32::from(e.lease_expires_at.is_some()),
            lease_expires_at: now + lease_ttl,
        })
        .collect();
    ClaimPlan { claimed }
}

/// Whether sibling `b` blocks head candidate `c` under `c`'s policy — the pure
/// mirror of `claim_partition_head_sql`'s `NOT EXISTS` (minus the in-flight arm,
/// which [`plan_partition_claim`] applies key-wide).
fn blocks(b: &QueueEntry, c: &QueueEntry, now: Millis) -> bool {
    if b.run_id == c.run_id {
        return false;
    }
    match c.partition_policy {
        // Blocking: ANY earlier sibling in stream order blocks — ready or not,
        // budget spent or not (an exhausted earlier sibling is the wedge).
        PartitionPolicy::Blocking => stream_key(b) < stream_key(c),
        // Leapfrog: only an earlier currently-ready sibling blocks.
        PartitionPolicy::Leapfrog => is_claimable(b, now) && ord_key(b) < ord_key(c),
    }
}

/// The dispatch-order key: `(available_at, run_id)`. The SQL carries a numeric
/// `stream_seq` tiebreak between the two (E4), inert while every enqueue writes
/// `stream_seq = 0`; see [`crate::plan_claim`]'s note for when the model adopts it.
fn ord_key(e: &QueueEntry) -> (Millis, &str) {
    (e.available_at, e.run_id.as_str())
}

/// The `blocking` policy's stream order: `(enqueued_at, run_id)` — stamped at
/// enqueue, never moved by a park/backoff (unlike `available_at`). The SQL carries
/// the same `stream_seq` tiebreak here, between `enqueued_at` and `run_id` (E4).
fn stream_key(e: &QueueEntry) -> (Millis, &str) {
    (e.enqueued_at, e.run_id.as_str())
}

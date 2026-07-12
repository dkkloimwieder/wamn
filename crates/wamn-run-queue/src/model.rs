//! The run-queue record model — the durable, claimable unit of work. A `runs`
//! row (5.7) is the immutable run-state history; a `run_queue` row is the
//! high-churn claim/lease machinery that co-transacts with it and is deleted when
//! the run is done. This is the *decision view* the pure claim/lease/janitor
//! logic reasons over, not every column of `deploy/run-queue.sql` (the DB row
//! also carries `enqueued_at`).

use serde::{Deserialize, Serialize};

/// Epoch milliseconds — the pure layer's time unit. Every time-dependent decision
/// takes a `now: Millis` argument (the crate reads no clock); the DB expresses the
/// same instants as `timestamptz` and compares with server-side `now()`.
pub type Millis = i64;

/// One row of `run_queue`: a run waiting to be (or being) dispatched. `available_at`
/// is when the row becomes claimable — future for a delayed/parked/backed-off run;
/// a live lease (`lease_expires_at` in the future) marks a row a runner currently
/// owns. `attempts` counts crash evidence — it bumps only when a claim reclaims an
/// expired lease (redelivery budget vs `max_attempts`); parks/wakes are free.
/// `partition_key` is reserved for the per-partition ownership follow-up (5.14
/// scaling); the walking skeleton leaves it null.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct QueueEntry {
    pub tenant_id: String,
    pub run_id: String,
    /// Per-partition ownership key (`partitioned(key)`, 5.11 semantics / 5.14
    /// ownership). Null = unpartitioned (claimed by the global claim). A non-null
    /// key is dispatched only through the per-partition ownership path
    /// ([`crate::plan_partition_claim`]) so the key's runs stay in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_key: Option<String>,
    /// Dispatch priority tiebreaker (claim orders by `available_at` first).
    #[serde(default)]
    pub priority: i32,
    /// When this row becomes claimable. Future = parked (delay node / backoff).
    pub available_at: Millis,
    /// The runner replica currently holding a lease, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_owner: Option<String>,
    /// The lease visibility timeout. A row with `lease_expires_at > now` is owned;
    /// past that it is reclaimable by another replica (crash-safe failover).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_expires_at: Option<Millis>,
    /// Crash-evidence count: how many times a claim has reclaimed this row's
    /// *expired* lease (the prior owner died holding it). First claims and
    /// park→wake re-claims do not count — parking is proof of life.
    #[serde(default)]
    pub attempts: i32,
    /// The redelivery budget: once `attempts >= max_attempts` and the lease is
    /// long expired, the janitor gives up (the run is `infrastructure-failure`).
    pub max_attempts: i32,
}

impl QueueEntry {
    /// A fresh, immediately-claimable queue entry (no lease, first attempt).
    pub fn ready(
        tenant_id: impl Into<String>,
        run_id: impl Into<String>,
        available_at: Millis,
        max_attempts: i32,
    ) -> QueueEntry {
        QueueEntry {
            tenant_id: tenant_id.into(),
            run_id: run_id.into(),
            partition_key: None,
            priority: 0,
            available_at,
            lease_owner: None,
            lease_expires_at: None,
            attempts: 0,
            max_attempts,
        }
    }

    /// A fresh, immediately-claimable entry bound to a partition (`partitioned(key)`
    /// dispatch — claimed only through the per-partition ownership path, never the
    /// global claim).
    pub fn ready_partition(
        tenant_id: impl Into<String>,
        run_id: impl Into<String>,
        partition_key: impl Into<String>,
        available_at: Millis,
        max_attempts: i32,
    ) -> QueueEntry {
        QueueEntry {
            partition_key: Some(partition_key.into()),
            ..QueueEntry::ready(tenant_id, run_id, available_at, max_attempts)
        }
    }
}

/// A partition lease: the row of `partition_owner` that grants a replica exclusive
/// dispatch rights over one `(tenant_id, partition_key)` stream. While a replica
/// holds a live lease it is the *only* replica that claims the partition's runs, so
/// ordering within the key is preserved; when the lease expires (the owner died or
/// stepped down) another replica reacquires the whole key and continues in order.
/// This is the *decision view* the pure acquire/claim logic reasons over — the DB
/// row (`deploy/run-queue.sql`) also carries `acquired_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PartitionOwner {
    pub tenant_id: String,
    pub partition_key: String,
    /// The replica holding the partition lease.
    pub lease_owner: String,
    /// The partition-lease visibility timeout. Past it, the partition is reacquirable.
    pub lease_expires_at: Millis,
}

impl PartitionOwner {
    /// A partition leased to `owner` until `lease_expires_at`.
    pub fn new(
        tenant_id: impl Into<String>,
        partition_key: impl Into<String>,
        lease_owner: impl Into<String>,
        lease_expires_at: Millis,
    ) -> PartitionOwner {
        PartitionOwner {
            tenant_id: tenant_id.into(),
            partition_key: partition_key.into(),
            lease_owner: lease_owner.into(),
            lease_expires_at,
        }
    }
}

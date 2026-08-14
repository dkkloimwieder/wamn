//! The run-queue record model — the durable, claimable unit of work. A `runs`
//! row (5.7) is the immutable run-state history; a `run_queue` row is the
//! high-churn claim/lease machinery that co-transacts with it and is deleted when
//! the run is done. This is the *decision view* the pure claim/lease/janitor
//! logic reasons over, not every column of `deploy/sql/run-queue.sql` (the DB row
//! also carries `enqueued_at`).

use serde::{Deserialize, Serialize};

/// Epoch milliseconds — the pure layer's time unit. Every time-dependent decision
/// takes a `now: Millis` argument (the crate reads no clock); the DB expresses the
/// same instants as `timestamptz` and compares with server-side `now()`.
pub type Millis = i64;

/// One row of `run_queue`: a run waiting to be (or being) dispatched. `available_at`
/// is when the row becomes claimable — future for a queue-parked/backed-off run;
/// a live lease (`lease_expires_at` in the future) marks a row a runner currently
/// owns. `attempts` counts crash evidence — it bumps only when a claim reclaims an
/// expired lease (redelivery budget vs `max_attempts`); queue parks/wakes are free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct QueueEntry {
    pub tenant_id: String,
    pub run_id: String,
    /// Persisted compatibility field; the global FIFO does not order by it.
    #[serde(default)]
    pub priority: i32,
    /// When this row becomes claimable. Future = queue-parked (for example,
    /// bounded-retry backoff).
    pub available_at: Millis,
    /// When this row was enqueued — stamped **once** and never updated, unlike
    /// `available_at` which a queue park/backoff pushes into the future. This is the
    /// stable admission timestamp retained independently of a later queue wait.
    #[serde(default)]
    pub enqueued_at: Millis,
    /// The per-flow monotone CDC stream position (D19 §5 / E4): the JetStream
    /// `stream_seq` a materializer-minted evt run (`<flow>:evt:<seq>`) is keyed
    /// by, carried as a numeric tiebreak AHEAD of `run_id` in every dispatch
    /// order so evt runs claim by NUMERIC stream position, never lexical run-id
    /// order (`f1:evt:10` must not precede `f1:evt:9` — the R6/D20 corruption
    /// class arriving through a string comparison). `0` for every non-CDC
    /// enqueue (the column default), which keeps the tiebreak inert there.
    #[serde(default)]
    pub stream_seq: i64,
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
            priority: 0,
            available_at,
            // An immediately-claimable row was enqueued when it became
            // available; a delayed enqueue's `enqueued_at` precedes it (the DB
            // stamps `now()` while `available_at` = `now() + delay`).
            enqueued_at: available_at,
            stream_seq: 0,
            lease_owner: None,
            lease_expires_at: None,
            attempts: 0,
            max_attempts,
        }
    }

    /// The same entry carrying a real CDC stream position (E4) — what the
    /// materializer's evt enqueue stamps; every other writer leaves the 0 default.
    pub fn with_stream_seq(mut self, stream_seq: i64) -> QueueEntry {
        self.stream_seq = stream_seq;
        self
    }
}

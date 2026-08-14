//! Durable queue, lease, partition, timer, and dead-letter state.
//!
//! The dispatch half of the flow runner: a `FOR UPDATE SKIP LOCKED` run queue in
//! Postgres (durability), NATS-core fire-and-forget doorbells (latency), and
//! run-claim leases that reclaim a dead replica's work (scaling). Where the run
//! history records persist *what happened* while this module governs *what
//! runs next and who runs it*: the write-ahead enqueue, the batch claim, lease
//! renewal, the janitor that gives up on an abandoned run, and the reconciliation
//! sweep that backstops a lost doorbell hint.
//!
//! Like the rest of `wamn-run-state`, this
//! crate is **pure**: no DB, no NATS, no clock. Every decision is a function of
//! `(rows, now, config)` with `now` a passed-in [`crate::queue::Millis`]; the SQL is emitted as
//! parameterized `String`s. The driver (`tests/integration/src/queuebench.rs` /
//! the dispatcher) supplies the `wamn:postgres` effects against the schema in
//! `deploy/sql/run-queue.sql`, the NATS-core doorbell, the real clock, and the replica
//! identity.
//!
//! ```
//! use wamn_run_state::queue::{claim_state, ClaimState, QueueEntry};
//!
//! // `now = 100`, a row visible since 50 with no lease -> a claim would take it.
//! let e = QueueEntry::ready("t1", "run-1", 50, 20);
//! assert_eq!(claim_state(&e, 100), ClaimState::Ready);
//!
//! // The same row leased until 500 is skipped until the lease expires.
//! let leased = QueueEntry { lease_owner: Some("A".into()), lease_expires_at: Some(500), ..e };
//! assert_eq!(claim_state(&leased, 100), ClaimState::Leased);
//! assert_eq!(claim_state(&leased, 600), ClaimState::Ready); // lease expired -> reclaimable
//! ```
//!
//! ## Scope (5.14) vs siblings
//! Owns: the `run_queue` (+ `partition_owner`) tables + DDL
//! (`deploy/sql/run-queue.sql`), the
//! `SKIP LOCKED` claim + batch claims, the D15 write-ahead / reduced-audit fast
//! path, run-claim leases + reclaim, the janitor (orphan →
//! `infrastructure-failure`), the reconciliation cadence, **per-partition
//! ownership** — the `partition_owner` lease + head-first claim
//! ([`crate::queue::plan_partition_claim`])
//! that dispatches `partitioned(key)` runs in order per key across replicas —
//! ownership. Trigger schedule and cadence decisions live in `wamn-scheduler`;
//! this module owns only their durable SQL boundary. (Row events are no longer a
//! dispatcher concern: the D19 v3 event plane — CDC reader → JetStream →
//! materializer — delivers them; the outbox path was torn down at l5i9.19.)
//! The **walking skeleton** deferred these to follow-ups; all are now
//! delivered, including the **guest-self-claim** (fqg.4): the flowrunner guest
//! links the pure claim-path builders and claims its own work via `run-next`.
//! Does **not** own: the engine walk / retry / reconstruction (5.2 + 5.7 — the
//! claimed run drives them); the `runs`/`node_runs` schema (5.7 — 5.14 co-transacts
//! and reuses the reserved `dispatched`/`infrastructure-failure` statuses via
//! [`crate::RunStatus`]); per-node ordering *semantics* (5.11 — 5.14 provides the
//! per-partition claim *mechanism*); the payload byte store (5.10).
//!
//! ## SR12 — what the pure tests cover, and what they cannot
//!
//! This crate's tests exercise the **decision** (which statement, what shape,
//! which binds); they cannot exercise the **statement** — the pure model has no
//! planner, isolation level, lock manager, or RLS. A statement can be modelled
//! correctly here and still misbehave live: this module's `claim_batch_sql`
//! passed every pure test while the real statement over-claimed on a
//! plan-dependent `SKIP LOCKED` re-scan — the `AS MATERIALIZED` fix is a
//! property of the emitted SQL no pure test can observe. Convention (SR12a):
//! every composed or plan-sensitive statement carries a comment naming what the
//! pure tests do NOT cover; the live half is the throwaway-PG gates over the
//! real prepared-statement path (SR12b).

mod claim;
// Evt-run identity (D19 §5 / E4): dep-free, always-on — the materializer guest
// links it through the same `default-features = false` core.
mod evt;
mod janitor;
mod lease;
mod model;
mod partition;
mod sql;

pub use claim::{
    ClaimPlan, ClaimState, Claimed, claim_state, dead_letters_on_terminal, is_claimable, plan_claim,
};
pub use evt::mint_evt_run_id;
pub use janitor::{JanitorVerdict, janitor_verdict, orphans};
pub use lease::{lease_deadline, lease_live, should_renew};
pub use model::{Millis, PartitionOwner, PartitionPolicy, QueueEntry};
pub use partition::{partition_lease_live, plan_acquire, plan_partition_claim};
pub use sql::{
    acquire_partitions_sql, active_flows_sql, claim_batch_sql, claim_dispatch_sql,
    claim_partition_head_sql, complete_dequeue_sql, dead_letter_dequeue_sql, dequeue_sql,
    enqueue_evt_sql, enqueue_evt_with_policy_sql, enqueue_sql, enqueue_with_policy_sql,
    gc_orphan_partitions_sql, janitor_sweep_sql, mark_running_sql, park_sql, parked_due_sql,
    record_error_and_renew_sql, record_success_and_renew_sql, release_partition_sql,
    renew_lease_sql, renew_partition_sql, write_ahead_run_sql, write_ahead_triggered_run_sql,
};

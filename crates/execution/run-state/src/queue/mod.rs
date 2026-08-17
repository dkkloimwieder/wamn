//! Durable global FIFO queue, lease, timer, and reclaim state.
//!
//! The dispatch half of the flow runner: a `FOR UPDATE SKIP LOCKED` run queue in
//! Postgres (durability), NATS-core fire-and-forget doorbells (latency), and
//! run-claim leases that reclaim a dead replica's work (scaling). Where the run
//! history records persist *what happened* while this module governs *what
//! runs next and who runs it*: the write-ahead enqueue, the one-row production claim, lease
//! renewal, the janitor that gives up on an abandoned run, and the reconciliation
//! sweep that backstops a lost doorbell hint.
//!
//! Like the rest of `wamn-run-state`, this
//! crate is **pure**: no DB, no NATS, no clock. Every decision is a function of
//! `(rows, now, config)` with `now` a passed-in [`crate::queue::Millis`]; the SQL is emitted as
//! parameterized `String`s. The host-owned production composer and dispatcher
//! supply the Postgres effects against the schema in
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
//! ## Scope vs siblings
//! Owns: the global `run_queue`, exact FIFO claim decision, lease/reclaim
//! classifier, and janitor. Trigger schedule and cadence decisions live in `wamn-scheduler`;
//! this module owns only their durable SQL boundary. (Row events are no longer a
//! dispatcher concern: the D19 v3 event plane — CDC reader → JetStream →
//! materializer — delivers them; the outbox path was torn down at l5i9.19.)
//! The host-only Postgres adapter composes the transaction; the flowrunner guest
//! receives only the already-claimed `(run-id, payload)` pair.
//! Does **not** own: the engine walk / retry (5.2 — the claimed run drives it);
//! the `runs`/`node_runs` schema (5.7 — 5.14 co-transacts
//! and reuses the reserved `dispatched`/`infrastructure-failure` statuses via
//! [`crate::RunStatus`]); the payload byte store (5.10).
//!
//! ## SR12 — what the pure tests cover, and what they cannot
//!
//! This crate's tests exercise the **decision** (which statement, what shape,
//! which binds); they cannot exercise the **statement** — the pure model has no
//! planner, isolation level, lock manager, or RLS. A statement can be modelled
//! correctly here and still misbehave live: a prior batch claim passed every
//! pure test while the real statement over-claimed on a
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
mod sql;

pub use claim::{
    ClaimPlan, ClaimState, Claimed, ProductionClaimClass, claim_state, classify_production_claim,
    is_claimable, plan_claim, production_claim_state,
};
pub use evt::mint_evt_run_id;
pub use janitor::{JanitorVerdict, janitor_verdict, janitor_verdict_with_attempt, orphans};
pub use lease::{lease_deadline, lease_live, should_renew};
pub use model::{Millis, QueueEntry};
pub use sql::{
    active_flows_sql, advance_claim_attempts_sql, clear_pre_effect_state_sql, complete_dequeue_sql,
    dequeue_sql, enqueue_evt_sql, enqueue_sql, grant_production_claim_sql, mark_running_sql,
    park_sql, parked_due_sql, renew_lease_sql, select_claim_effect_attempt_sql,
    select_exhausted_production_sql, select_pre_effect_projection_sql, select_production_claim_sql,
    serialize_effect_intent_sql, terminalize_effect_uncertain_claim_sql,
    terminalize_exhausted_production_sql, write_ahead_run_sql, write_ahead_triggered_run_sql,
};

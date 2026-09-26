//! Durable global FIFO queue, lease, timer, and reclaim state.
//!
//! Postgres stores queued work. The host claims due rows with
//! `FOR UPDATE SKIP LOCKED`; leases allow another replica to recover abandoned work.
//! This module supplies claim, lease renewal, and exhausted-run decisions.
//!
//! This crate has no database, broker, or clock. Decisions consume explicit
//! rows and time, and SQL builders return parameterized statements.
//! The host supplies database effects, the clock, and the replica identity.
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
//! Owns the global `run_queue`, FIFO claim decision, lease recovery, and janitor.
//! The host polls due work. CDC and the materializer deliver row events.
//! The host-only Postgres adapter composes the transaction and hands the exact
//! frozen wiring identity plus payload to the router driver.
//! Does **not** own: the router walk / retry (the claimed run drives it);
//! the `runs` schema (5.7 — 5.14 co-transacts
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
/// The workflow contract's statements: admit, park, release, and list.
mod workflow;

pub use claim::{
    ClaimPlan, ClaimState, Claimed, ProductionClaimClass, claim_state, classify_production_claim,
    is_claimable, plan_claim, production_claim_state,
};
pub use evt::mint_evt_run_id;
pub use janitor::{JanitorVerdict, janitor_verdict, janitor_verdict_with_attempt, orphans};
pub use lease::{lease_deadline, lease_live, should_renew};
pub use model::{Millis, QueueEntry};
pub use sql::{
    advance_claim_attempts_sql, clear_pre_effect_state_sql, grant_production_claim_sql,
    renew_production_lease_sql, select_claim_effect_attempt_sql, select_exhausted_production_sql,
    select_production_claim_sql, serialize_effect_intent_sql,
    terminalize_effect_uncertain_claim_sql, terminalize_exhausted_production_sql,
};
pub use workflow::{
    insert_automation_run_sql, insert_event_run_sql, insert_run_queue_sql, list_workflow_runs_sql,
    park_queued_run_sql, release_parked_run_sql, select_automation_run_sql, select_event_run_sql,
    select_run_queue_state_sql,
};

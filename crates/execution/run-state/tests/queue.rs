//! Pure tests of queue eligibility, FIFO order, leases, and exhausted-run reaping.

use serde_json::json;
use wamn_run_state::invariants::{self, QueueState};
use wamn_run_state::queue::{
    JanitorVerdict, LeasePlan, LeaseState, Millis, ProductionLeaseClass, QueueEntry,
    classify_production_lease, janitor_verdict, lease_deadline, lease_live, lease_state,
    should_renew,
};

// ---- D8a: the RUN-* invariant sweep (wamn-54b0.2) --------------------------
//
// `wamn_run_state::invariants::check` runs after every step that leaves a queue
// state behind, so a decision that passes its own assertion while breaking
// RUN-2, RUN-3, RUN-4 or RUN-7 still fails here. The invariants module doc
// records which RUN-* need D2's seeded scheduler instead.

/// The janitor grace window every sweep in this file uses.
const GRACE: Millis = 50;

/// Assert the RUN-* invariants over the rows a step left behind. `plan` is the
/// lease taken at that step, if any, with the batch limit it was taken with.
fn run_invariants_hold(rows: &[QueueEntry], plan: Option<(&LeasePlan, usize)>, now: Millis) {
    let (plan, limit) = plan.map_or((None, 0), |(plan, limit)| (Some(plan), limit));
    let state = QueueState {
        rows,
        plan,
        limit,
        grace: GRACE,
        now,
    };
    if let Err(violation) = invariants::check(&state) {
        panic!("{violation}");
    }
}

#[test]
fn lease_state_preserves_budget() {
    let ready = QueueEntry::ready("t1", "ready", 50, 2);
    assert_eq!(lease_state(&ready, 100), LeaseState::Ready);
    run_invariants_hold(std::slice::from_ref(&ready), None, 100);

    let parked = QueueEntry {
        available_at: 101,
        ..ready.clone()
    };
    assert_eq!(lease_state(&parked, 100), LeaseState::Parked);
    run_invariants_hold(std::slice::from_ref(&parked), None, 100);

    let leased = QueueEntry {
        lease_owner: Some("runner-a".into()),
        lease_expires_at: Some(200),
        ..ready.clone()
    };
    assert_eq!(lease_state(&leased, 100), LeaseState::Leased);
    run_invariants_hold(std::slice::from_ref(&leased), None, 100);

    let exhausted = QueueEntry {
        lease_expires_at: Some(90),
        attempts: 2,
        ..ready
    };
    assert_eq!(lease_state(&exhausted, 100), LeaseState::Exhausted);
    run_invariants_hold(std::slice::from_ref(&exhausted), None, 100);
}

#[test]
fn recover_classifier_has_exact_two_actions() {
    assert_eq!(
        classify_production_lease(false),
        ProductionLeaseClass::Ordinary
    );
    assert_eq!(
        classify_production_lease(true),
        ProductionLeaseClass::ExpiredPreEffect
    );
}

#[test]
fn janitor_reaps_exhausted_runs() {
    let exhausted = QueueEntry {
        lease_expires_at: Some(100),
        attempts: 3,
        ..QueueEntry::ready("t1", "run-1", 0, 3)
    };
    assert_eq!(
        janitor_verdict(&exhausted, 200, 50),
        JanitorVerdict::Orphaned
    );
    run_invariants_hold(std::slice::from_ref(&exhausted), None, 200);
}

#[test]
fn lease_arithmetic_remains_stable() {
    assert_eq!(lease_deadline(1_000, 250), 1_250);
    assert!(lease_live(1_249, Some(1_250)));
    assert!(!lease_live(1_250, Some(1_250)));
    assert!(should_renew(1_200, 1_250, 100));
    // The same deadline as a queue row, swept either side of expiry: RUN-7 is
    // exactly the lease of a crashed owner's row waiting out its lease.
    let held = QueueEntry {
        lease_owner: Some("runner-a".into()),
        lease_expires_at: Some(lease_deadline(1_000, 250)),
        ..QueueEntry::ready("t1", "held", 1_000, 3)
    };
    run_invariants_hold(std::slice::from_ref(&held), None, 1_249);
    run_invariants_hold(std::slice::from_ref(&held), None, 1_250);
}

#[test]
fn queue_entry_round_trips_without_partition_plane() {
    let entry = QueueEntry::ready("tenant", "run", 42, 5);
    let value = serde_json::to_value(&entry).unwrap();
    assert_eq!(value["env_key"], json!("tenant"));
    assert!(value.get("partition-key").is_none());
    assert!(value.get("partition-policy").is_none());
    assert_eq!(serde_json::from_value::<QueueEntry>(value).unwrap(), entry);
}

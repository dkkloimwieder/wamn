//! Pure tests of queue eligibility, FIFO order, leases, and exhausted-run reaping.

use serde_json::json;
use wamn_run_state::invariants::{self, QueueState};
use wamn_run_state::queue::{
    ClaimPlan, ClaimState, JanitorVerdict, Millis, ProductionClaimClass, QueueEntry, claim_state,
    classify_production_claim, janitor_verdict, lease_deadline, lease_live, mint_evt_run_id,
    plan_claim, should_renew,
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
/// claim taken at that step, if any, with the batch limit it was taken with.
fn run_invariants_hold(rows: &[QueueEntry], plan: Option<(&ClaimPlan, usize)>, now: Millis) {
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
fn claim_state_preserves_budget() {
    let ready = QueueEntry::ready("t1", "ready", 50, 2);
    assert_eq!(claim_state(&ready, 100), ClaimState::Ready);
    run_invariants_hold(std::slice::from_ref(&ready), None, 100);

    let parked = QueueEntry {
        available_at: 101,
        ..ready.clone()
    };
    assert_eq!(claim_state(&parked, 100), ClaimState::Parked);
    run_invariants_hold(std::slice::from_ref(&parked), None, 100);

    let leased = QueueEntry {
        lease_owner: Some("runner-a".into()),
        lease_expires_at: Some(200),
        ..ready.clone()
    };
    assert_eq!(claim_state(&leased, 100), ClaimState::Leased);
    run_invariants_hold(std::slice::from_ref(&leased), None, 100);

    let exhausted = QueueEntry {
        lease_expires_at: Some(90),
        attempts: 2,
        ..ready
    };
    assert_eq!(claim_state(&exhausted, 100), ClaimState::Exhausted);
    run_invariants_hold(std::slice::from_ref(&exhausted), None, 100);
}

#[test]
fn reclaim_classifier_has_exact_two_actions() {
    assert_eq!(
        classify_production_claim(false),
        ProductionClaimClass::Ordinary
    );
    assert_eq!(
        classify_production_claim(true),
        ProductionClaimClass::ExpiredPreEffect
    );
}

#[test]
fn global_fifo_uses_available_stream_run_tie_break() {
    let rows = [
        QueueEntry::ready("t1", "run-z", 10, 3).with_stream_seq(8),
        QueueEntry::ready("t1", "run-b", 10, 3).with_stream_seq(7),
        QueueEntry::ready("t1", "run-a", 10, 3).with_stream_seq(7),
        QueueEntry::ready("t1", "earliest", 9, 3).with_stream_seq(99),
    ];
    let plan = plan_claim(&rows, 10, 10, 1_000);
    assert_eq!(
        plan.claimed
            .iter()
            .map(|claim| claim.run_id.as_str())
            .collect::<Vec<_>>(),
        ["earliest", "run-a", "run-b", "run-z"]
    );
    run_invariants_hold(&rows, Some((&plan, 10)), 10);
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
    // exactly the claim of a crashed owner's row waiting out its lease.
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
    let entry = QueueEntry::ready("tenant", "run", 42, 5).with_stream_seq(17);
    let value = serde_json::to_value(&entry).unwrap();
    assert_eq!(value["env_key"], json!("tenant"));
    assert_eq!(value["stream-seq"], json!(17));
    assert!(value.get("partition-key").is_none());
    assert!(value.get("partition-policy").is_none());
    assert_eq!(serde_json::from_value::<QueueEntry>(value).unwrap(), entry);
}

#[test]
fn event_run_id_preserves_numeric_stream_order() {
    assert!(mint_evt_run_id("flow", 9) < mint_evt_run_id("flow", 10));
}

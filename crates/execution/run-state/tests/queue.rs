//! Pure tests of queue eligibility, FIFO order, leases, and exhausted-run reaping.

use serde_json::json;
use wamn_run_state::invariants::{self, QueueState};
use wamn_run_state::queue::{
    ClaimPlan, ClaimState, JanitorVerdict, Millis, ProductionClaimClass, QueueEntry,
    advance_claim_attempts_sql, claim_state, classify_production_claim, clear_pre_effect_state_sql,
    grant_production_claim_sql, janitor_verdict, lease_deadline, lease_live, mint_evt_run_id,
    plan_claim, renew_production_lease_sql, select_exhausted_production_sql,
    select_production_claim_sql, should_renew, terminalize_exhausted_production_sql,
};

// ---- D8a: the RUN-* invariant sweep (wamn-54b0.2) --------------------------
//
// `wamn_run_state::invariants::check` runs after every step that leaves a queue
// state behind, so a decision that passes its own assertion while breaking
// RUN-2, RUN-3, RUN-4 or RUN-7 still fails here. The SQL-literal cases hold no
// rows and no clock, so there is no state for the sweep to read; the invariants
// module doc records which RUN-* need D2's seeded scheduler instead.

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

    let sql = select_production_claim_sql();
    assert!(sql.contains("ORDER BY q.available_at, q.stream_seq, q.run_id"));
    assert!(sql.contains("AS MATERIALIZED"));
    assert!(sql.contains("FOR UPDATE OF selected_run, q SKIP LOCKED"));
    assert!(sql.contains("LIMIT 1"));
    assert!(sql.contains("selected_run.package_id = ANY($1::text[])"));
    assert!(sql.contains("selected_run.environment = $2"));
    // R20 (1): FIFO is per release while two releases coexist.
    assert!(sql.contains("AND selected_run.manifest_digest IS NOT DISTINCT FROM $3::text"));
    assert!(sql.contains("AS router_caller_attached"));
    assert!(sql.contains("AS durable_caller_attached"));
    assert!(sql.contains("r.flow_id IS NULL AND r.flow_version IS NULL"));
}

#[test]
fn production_lease_uses_a_fresh_post_fence_clock() {
    let grant = grant_production_claim_sql();
    assert!(grant.contains("lease_expires_at = statement_timestamp()"));
    assert!(!grant.contains("lease_expires_at = now()"));
}

#[test]
fn app_pre_effect_step_clears_only_abandoned_run_state() {
    let sql = clear_pre_effect_state_sql();
    assert!(sql.contains("SET state_json = NULL"));
    assert!(!sql.contains("release_version"));
    for preserved in [
        "input_json =",
        "invocation_context =",
        "package_id =",
        "manifest_digest =",
        "updated_at =",
        "DELETE FROM effect_attempts",
    ] {
        assert!(!sql.contains(preserved), "reset must preserve {preserved}");
    }
}

#[test]
fn lease_grant_is_a_separate_final_statement() {
    let select = select_production_claim_sql();
    let grant = grant_production_claim_sql();
    assert!(!select.contains("SET lease_owner"));
    assert!(!select.contains("lease_generation ="));
    assert!(grant.contains("SET lease_owner = $2"));
    assert!(grant.contains("lease_generation = q.lease_generation + 1"));
    assert!(grant.contains("status = 'running'"));
}

#[test]
fn crash_evidence_advances_in_a_statement_the_grant_cannot_roll_back() {
    // The grant can abort at a database guard on the run row. An `attempts`
    // increment fused into it rolled back with it, so the run never reached
    // `max_attempts` and the janitor could never reap it (wamn-0h0g.15.69).
    let grant = grant_production_claim_sql();
    assert!(!grant.contains("attempts"));

    let advance = advance_claim_attempts_sql();
    assert!(advance.contains("UPDATE run_queue AS q"));
    assert!(advance.contains("SET attempts = q.attempts"));
    // Crash evidence only: a released (queue-parked) or never-leased row is free.
    assert!(advance.contains("CASE WHEN q.lease_expires_at IS NOT NULL THEN 1 ELSE 0 END"));
    assert!(!advance.contains("lease_owner"));
    assert!(!advance.contains("lease_expires_at ="));
    assert!(!advance.contains("UPDATE runs"));
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
    let select = select_exhausted_production_sql();
    assert!(select.contains("q.attempts >= q.max_attempts"));
    assert!(select.contains("FOR UPDATE OF selected_run, q SKIP LOCKED"));
    assert!(select.contains("selected_run.package_id = ANY($2::text[])"));
    assert!(select.contains("selected_run.environment = $3"));
    assert!(!select.contains("effect_attempts"));
    let terminalize = terminalize_exhausted_production_sql();
    assert!(terminalize.contains("THEN $2::text::jsonb"));
    assert!(terminalize.contains("THEN $3"));
    assert!(terminalize.contains("THEN 500"));
    assert!(terminalize.contains("DELETE FROM run_queue"));
    assert!(terminalize.contains("result_json = CASE"));
    assert!(terminalize.contains("r.binding_world_json IS NOT NULL"));
    assert!(!terminalize.contains("sha256("));
    assert!(!terminalize.contains("jsonb::text"));
    assert!(!select.contains("partition_key"));
    assert!(!select.contains("partition_policy"));
}

/// D2b, and the line the owner drew through it: a STAMP takes the instant from
/// the caller, a COMPARISON keeps the server clock.
///
/// The split is authority, not taste. A caller-supplied instant inside a lease
/// or visibility test would let a worker decide whether its own lease had
/// expired, which moves that decision out of the database. Queue
/// terminalization takes the caller instant, and the claim,
/// renewal, and wake predicates keep it.
#[test]
fn queue_stamps_take_the_caller_instant_and_comparisons_keep_the_server_clock() {
    let sql = terminalize_exhausted_production_sql();
    assert!(sql.contains("caller_released_at = CASE"), "{sql}");
    assert!(sql.contains("THEN $4::timestamptz"), "{sql}");
    assert!(sql.contains("updated_at = $4::timestamptz"), "{sql}");
    assert!(!sql.contains("now()"), "{sql}");
    assert!(!sql.contains("statement_timestamp()"), "{sql}");

    // The comparisons the ruling protects. Each is a read, not a write.
    let claim = select_production_claim_sql();
    assert!(claim.contains("q.available_at <= now()"), "{claim}");
    assert!(claim.contains("q.lease_expires_at <= now()"), "{claim}");
    let grant = grant_production_claim_sql();
    assert!(
        grant.contains("lease_expires_at = statement_timestamp()"),
        "{grant}"
    );
    let renew = renew_production_lease_sql();
    assert!(
        renew.contains("q.lease_expires_at > statement_timestamp()"),
        "{renew}"
    );
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
    let renew = renew_production_lease_sql();
    assert!(renew.contains("lease_owner = $2"));
    assert!(renew.contains("lease_generation = $3"));
    assert!(renew.contains("lease_expires_at > statement_timestamp()"));
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

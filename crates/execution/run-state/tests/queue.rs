//! Pure proofs of the global run queue: the SURVIVING SPINE and, beside it, the
//! SHELVED CRASH FLOOR.
//!
//! wamn-0h0g.20.4 partitions the live claim proofs into two suites, one per
//! durability class. Nothing here needs that treatment — every test in this
//! file is pure, hermetic and free, so there is no fixture to drop and no tier
//! to gate. The distinction is still worth naming, because it decides what a
//! future deletion of the crash floor may take with it:
//!
//! * SPINE — the queue as it runs on the DEFAULT `standard` class: FIFO order,
//!   SKIP LOCKED, lease arithmetic and renewal, crash-evidence accounting, the
//!   park's release-record reset, the exhausted-run janitor, the enqueue
//!   builders, and the dispatcher's wake hint. None of it may change.
//! * SHELF — reachable only when `DurabilityClass::admits_effect_evidence()`
//!   is true (wamn-0h0g.20.2): `ProductionClaimClass::ExpiredWithAttempt`,
//!   `JanitorVerdict::EffectAttempt`, `terminalize_effect_uncertain_claim_sql`,
//!   `select_claim_effect_attempt_sql`, `serialize_effect_intent_sql`, and the
//!   effect disjunct inside `select_production_claim_sql`'s eligibility
//!   predicate. Unreachable by default, NOT deleted.
//! * THE GATE ITSELF — the `the_default_class_*` and `the_class_gate_*` tests,
//!   which prove exactly that the shelf is closed on the default class and
//!   that closing it changed nothing else.
//!
//! A test asserting on both sides is not a filing error: the gate is a
//! conjunct inside statements the spine also owns, so proving the spine
//! unchanged and proving the shelf closed is often one assertion apart. The
//! live counterparts are `crates/platform/runtime/tests/production_claim_live.rs`
//! (spine) and `production_claim_durable_live.rs` (shelf).

use serde_json::json;
use wamn_run_state::DurabilityClass;
use wamn_run_state::queue::{
    ClaimState, JanitorVerdict, ProductionClaimClass, QueueEntry, advance_claim_attempts_sql,
    claim_state, classify_production_claim, clear_pre_effect_state_sql, grant_production_claim_sql,
    janitor_verdict_with_attempt, lease_deadline, lease_live, mint_evt_run_id, parked_due_sql,
    plan_claim, production_claim_state, renew_production_lease_sql,
    select_claim_effect_attempt_sql, select_exhausted_production_sql, select_production_claim_sql,
    serialize_effect_intent_sql, should_renew, terminalize_effect_uncertain_claim_sql,
    terminalize_exhausted_production_sql,
};

#[test]
fn claim_state_preserves_budget_and_effect_attempt_escape() {
    let ready = QueueEntry::ready("t1", "ready", 50, 2);
    assert_eq!(claim_state(&ready, 100), ClaimState::Ready);

    let parked = QueueEntry {
        available_at: 101,
        ..ready.clone()
    };
    assert_eq!(claim_state(&parked, 100), ClaimState::Parked);

    let leased = QueueEntry {
        lease_owner: Some("runner-a".into()),
        lease_expires_at: Some(200),
        ..ready.clone()
    };
    assert_eq!(claim_state(&leased, 100), ClaimState::Leased);

    let exhausted = QueueEntry {
        lease_expires_at: Some(90),
        attempts: 2,
        ..ready
    };
    assert_eq!(claim_state(&exhausted, 100), ClaimState::Exhausted);
    assert_eq!(
        production_claim_state(&exhausted, DurabilityClass::Durable, true, 100),
        ClaimState::Ready
    );
}

#[test]
fn reclaim_classifier_has_exact_three_actions() {
    assert_eq!(
        classify_production_claim(DurabilityClass::Durable, false, false),
        ProductionClaimClass::Ordinary
    );
    assert_eq!(
        classify_production_claim(DurabilityClass::Durable, false, true),
        ProductionClaimClass::ExpiredWithAttempt
    );
    assert_eq!(
        classify_production_claim(DurabilityClass::Durable, true, false),
        ProductionClaimClass::ExpiredPreEffect
    );
    assert_eq!(
        classify_production_claim(DurabilityClass::Durable, true, true),
        ProductionClaimClass::ExpiredWithAttempt
    );
}

#[test]
fn the_default_class_takes_plain_lock_then_lease() {
    // wamn-0h0g.20.2 (a): the eligibility predicate's effect-evidence disjunct
    // opens ONLY for the premium class. A budget-spent expired lease is the
    // janitor's on the default tier no matter what the effect ledger holds.
    let exhausted = QueueEntry {
        lease_expires_at: Some(90),
        attempts: 2,
        ..QueueEntry::ready("t1", "spent", 50, 2)
    };
    assert_eq!(
        production_claim_state(&exhausted, DurabilityClass::Standard, true, 100),
        ClaimState::Exhausted,
        "the default class was let into the shelved crash floor"
    );
    assert_eq!(
        production_claim_state(&exhausted, DurabilityClass::Durable, true, 100),
        ClaimState::Ready,
        "the premium class no longer reaches the floor it pays for"
    );

    // With the gate closed, the class-carrying predicate is EXACTLY the
    // no-effect view — the shelf changes nothing else about eligibility.
    for entry in [
        QueueEntry::ready("t1", "ready", 50, 2),
        QueueEntry {
            available_at: 101,
            ..QueueEntry::ready("t1", "parked", 50, 2)
        },
        QueueEntry {
            lease_owner: Some("runner-a".into()),
            lease_expires_at: Some(200),
            ..QueueEntry::ready("t1", "leased", 50, 2)
        },
        exhausted.clone(),
    ] {
        assert_eq!(
            production_claim_state(&entry, DurabilityClass::Standard, true, 100),
            claim_state(&entry, 100),
            "the default class diverged from the no-effect predicate on {}",
            entry.run_id
        );
    }
}

#[test]
fn the_default_class_never_classifies_expired_with_attempt() {
    // (b), (c) and (d) of wamn-0h0g.20.2 are UNREACHABLE, not deleted: this is
    // the only producer of the variant that reaches them.
    for had_prior_lease in [false, true] {
        assert_ne!(
            classify_production_claim(DurabilityClass::Standard, had_prior_lease, true),
            ProductionClaimClass::ExpiredWithAttempt,
            "the default class reached the shelved floor \
             (had_prior_lease={had_prior_lease})"
        );
    }
    assert_eq!(
        classify_production_claim(DurabilityClass::Standard, true, true),
        ProductionClaimClass::ExpiredPreEffect
    );
    assert_eq!(
        classify_production_claim(DurabilityClass::Standard, false, true),
        ProductionClaimClass::Ordinary
    );
}

#[test]
fn the_class_gate_carries_one_sql_literal_at_every_gated_statement() {
    // One spelling, one place to audit. `select_production_claim_sql` gates its
    // eligibility disjunct.
    let predicate = "durability_class = 'durable'";
    let candidate = select_production_claim_sql();
    assert_eq!(
        candidate.matches(predicate).count(),
        1,
        "the claim's class gate is not exactly one predicate"
    );
    assert!(candidate.contains(&format!("AND selected_run.{predicate}")));
    // The gate rides the row the statement already joins and locks: no second
    // relation, no second lookup.
    assert_eq!(candidate.matches("JOIN runs").count(), 2);

    // The class rides both host-decoded projections.
    assert!(candidate.contains("r.durability_class"));
    let exhausted = select_exhausted_production_sql();
    assert!(exhausted.contains("selected_run.durability_class"));
    for claim in [&candidate, &exhausted] {
        assert!(
            !claim.contains("environment_policies") && !claim.contains("registry."),
            "claim reads the frozen run carrier only: {claim}"
        );
    }
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

    let sql = select_production_claim_sql();
    assert!(sql.contains("ORDER BY q.available_at, q.stream_seq, q.run_id"));
    assert!(sql.contains("AS MATERIALIZED"));
    assert!(sql.contains("FOR UPDATE OF selected_run, q SKIP LOCKED"));
    assert!(sql.contains("LIMIT 1"));
    assert!(sql.contains("selected_run.package_id = ANY($1::text[])"));
    assert!(sql.contains("selected_run.environment = $2"));
    assert!(sql.contains("AS router_caller_attached"));
    assert!(sql.contains("AS durable_caller_attached"));
    assert!(sql.contains("r.flow_id IS NULL AND r.flow_version IS NULL"));
}

#[test]
fn effect_attempt_escape_is_classified_before_any_plan_read() {
    let candidate = select_production_claim_sql();
    assert!(candidate.contains("OR EXISTS ("));
    assert!(candidate.contains("FROM effect_attempts AS effect"));
    assert!(!candidate.contains("AS has_effect_attempt"));
    assert!(!candidate.contains("catalog."));
    assert!(!candidate.contains("execution_bundles"));

    let classification = select_claim_effect_attempt_sql();
    assert!(classification.contains("SELECT EXISTS"));
    assert!(classification.contains("effect.run_id = $1"));
    assert!(!classification.contains("run_queue"));

    let fence = serialize_effect_intent_sql();
    assert!(fence.contains("pg_advisory_xact_lock"));
    assert!(fence.contains("hashtextextended"));
    assert!(fence.contains("current_setting('app.tenant', true)"));
    assert!(fence.contains("$1::text"));
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
    // The abandoned attempt's manifest record is projection of that attempt and
    // joins the replacement set, so the next claim records afresh
    // (wamn-0h0g.15.55).
    assert!(sql.contains("manifest_digest = NULL"));
    assert!(!sql.contains("release_version"));
    for preserved in [
        "input_json =",
        "invocation_context =",
        "package_id =",
        "effective_release_id =",
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
fn effect_uncertain_terminalization_dequeues_and_persists_exact_attached_shape() {
    let sql = terminalize_effect_uncertain_claim_sql();
    assert!(sql.contains("status = 'effect-uncertain'"));
    assert!(sql.contains("fail_kind = 'effect-uncertain'"));
    assert!(sql.contains("THEN $2::text::jsonb"));
    assert!(sql.contains("THEN 500"));
    assert!(sql.contains("THEN NULL"));
    assert!(sql.contains("THEN $3"));
    assert!(sql.contains("DELETE FROM run_queue"));
    assert!(sql.contains("caller_released_at IS NULL"));
}

#[test]
fn janitor_excludes_effect_attempts() {
    let exhausted = QueueEntry {
        lease_expires_at: Some(100),
        attempts: 3,
        ..QueueEntry::ready("t1", "run-1", 0, 3)
    };
    assert_eq!(
        janitor_verdict_with_attempt(&exhausted, false, 200, 50),
        JanitorVerdict::Orphaned
    );
    assert_eq!(
        janitor_verdict_with_attempt(&exhausted, true, 200, 50),
        JanitorVerdict::EffectAttempt
    );
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

#[test]
fn dispatcher_reconciliation_mirrors_claim_eligibility_and_order() {
    let wake = parked_due_sql(100);
    assert!(wake.contains("available_at <= now() + interval '250 milliseconds'"));
    assert!(wake.contains("attempts < q.max_attempts"));
    assert!(wake.contains("OR q.lease_expires_at IS NULL"));
    assert!(wake.contains("FROM effect_attempts AS effect"));
    assert!(wake.contains("ORDER BY q.available_at, q.stream_seq, q.run_id"));
    assert!(!wake.contains("partition_key"));

    // THE CLASS GATE STOPS HERE, AND MUST (wamn-0h0g.20.2). The dispatcher runs
    // as a scoped reader holding SELECT on `run_queue` and `effect_attempts` and
    // explicitly NOT on `runs` (services/dispatcher/tests/read_authority.rs), so
    // correlating this to `runs.durability_class` would make every sweep a
    // permission failure. It produces a WAKE HINT; the executor's own claim
    // re-decides under the gated predicate one statement later, and this one can
    // only ever over-select relative to it.
    assert!(
        !wake.contains("runs"),
        "the reconciliation hint must not read a relation its role cannot see"
    );
    assert!(!wake.contains("durability_class"));
}

#[test]
fn lease_arithmetic_remains_stable() {
    assert_eq!(lease_deadline(1_000, 250), 1_250);
    assert!(lease_live(1_249, Some(1_250)));
    assert!(!lease_live(1_250, Some(1_250)));
    assert!(should_renew(1_200, 1_250, 100));
    let renew = renew_production_lease_sql();
    assert!(renew.contains("lease_owner = $2"));
    assert!(renew.contains("lease_generation = $3"));
    assert!(renew.contains("lease_expires_at > statement_timestamp()"));
}

#[test]
fn queue_entry_round_trips_without_partition_plane() {
    let entry = QueueEntry::ready("tenant", "run", 42, 5).with_stream_seq(17);
    let value = serde_json::to_value(&entry).unwrap();
    assert_eq!(value["tenant-id"], json!("tenant"));
    assert_eq!(value["stream-seq"], json!(17));
    assert!(value.get("partition-key").is_none());
    assert!(value.get("partition-policy").is_none());
    assert_eq!(serde_json::from_value::<QueueEntry>(value).unwrap(), entry);
}

#[test]
fn event_run_id_preserves_numeric_stream_order() {
    assert!(mint_evt_run_id("flow", 9) < mint_evt_run_id("flow", 10));
}

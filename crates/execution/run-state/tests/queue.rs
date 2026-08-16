use serde_json::json;
use wamn_run_state::queue::{
    ClaimState, JanitorVerdict, ProductionClaimClass, QueueEntry, claim_state,
    classify_production_claim, clear_pre_effect_state_sql, enqueue_evt_sql, enqueue_sql,
    grant_production_claim_sql, janitor_verdict_with_attempt, lease_deadline, lease_live,
    mint_evt_run_id, park_sql, parked_due_sql, plan_claim, production_claim_state,
    select_claim_effect_attempt_sql, select_exhausted_production_sql,
    select_pre_effect_projection_sql, select_production_claim_sql, serialize_effect_intent_sql,
    should_renew, terminalize_effect_uncertain_claim_sql, terminalize_exhausted_production_sql,
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
        production_claim_state(&exhausted, true, 100),
        ClaimState::Ready
    );
}

#[test]
fn reclaim_classifier_has_exact_three_actions() {
    assert_eq!(
        classify_production_claim(false, false),
        ProductionClaimClass::Ordinary
    );
    assert_eq!(
        classify_production_claim(false, true),
        ProductionClaimClass::ExpiredWithAttempt
    );
    assert_eq!(
        classify_production_claim(true, false),
        ProductionClaimClass::ExpiredPreEffect
    );
    assert_eq!(
        classify_production_claim(true, true),
        ProductionClaimClass::ExpiredWithAttempt
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

    let sql = select_production_claim_sql();
    assert!(sql.contains("ORDER BY q.available_at, q.stream_seq, q.run_id"));
    assert!(sql.contains("AS MATERIALIZED"));
    assert!(sql.contains("FOR UPDATE OF selected_run, q SKIP LOCKED"));
    assert!(sql.contains("LIMIT 1"));
    for fence in [
        "candidate.lease_owner",
        "candidate.lease_expires_at::text",
        "candidate.lease_generation",
    ] {
        assert!(sql.contains(fence), "missing reset fence {fence}");
    }
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
fn app_pre_effect_step_clears_only_state_after_private_projection_reset() {
    let projection = select_pre_effect_projection_sql();
    assert!(projection.contains("SELECT EXISTS"));
    assert!(projection.contains("FROM node_runs"));
    let sql = clear_pre_effect_state_sql();
    assert!(sql.contains("UPDATE runs SET state_json = NULL"));
    assert!(sql.contains("NOT EXISTS"));
    assert!(!sql.contains("DELETE FROM node_runs"));
    for preserved in [
        "input_json =",
        "invocation_context =",
        "catalog_id =",
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
    assert!(grant.contains("CASE WHEN q.lease_expires_at IS NOT NULL THEN 1 ELSE 0 END"));
    assert!(grant.contains("status = 'running'"));
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
    assert!(!select.contains("effect_attempts"));
    let terminalize = terminalize_exhausted_production_sql();
    assert!(terminalize.contains("THEN $2::text::jsonb"));
    assert!(terminalize.contains("THEN $3"));
    assert!(terminalize.contains("THEN 500"));
    assert!(terminalize.contains("DELETE FROM run_queue"));
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
}

#[test]
fn enqueue_builders_have_global_argument_shapes() {
    let enqueue = enqueue_sql();
    assert!(enqueue.contains("(tenant_id, run_id, priority, available_at)"));
    assert!(enqueue.contains("$3::bigint"));
    assert!(!enqueue.contains("$4"));
    assert!(!enqueue.contains("partition_"));

    let event = enqueue_evt_sql();
    assert!(event.contains("priority, available_at, stream_seq"));
    assert!(event.contains("$4"));
    assert!(!event.contains("$5"));
    assert!(!event.contains("partition_"));
}

#[test]
fn lease_and_park_arithmetic_remains_stable() {
    assert_eq!(lease_deadline(1_000, 250), 1_250);
    assert!(lease_live(1_249, Some(1_250)));
    assert!(!lease_live(1_250, Some(1_250)));
    assert!(should_renew(1_200, 1_250, 100));
    let park = park_sql();
    assert!(park.contains("lease_owner = NULL, lease_expires_at = NULL"));
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

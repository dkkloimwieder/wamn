//! Conformance proofs for the run-state-owned stand-in schema guard.

use wamn_run_state::schema_drift::{Need, assert_run_state_stand_in, assert_stand_in};

const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");

fn all_required() -> [(&'static str, Need); 1] {
    [("run_queue", Need::Required)]
}

fn uncommented(sql: &str) -> String {
    sql.lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn uncommented_schema_of_record() -> String {
    uncommented(RUN_QUEUE_SQL)
}

#[test]
fn schema_of_record_satisfies_the_stand_in_contract() {
    assert_stand_in(
        "conformance",
        &uncommented_schema_of_record(),
        &all_required(),
    );
}

#[test]
#[should_panic(expected = "run_queue` stand-in missing column `stream_seq`")]
fn stand_in_guard_rejects_column_named_only_by_an_index() {
    let column = "    stream_seq       bigint NOT NULL DEFAULT 0,\n";
    let stand_in = uncommented_schema_of_record();
    assert_eq!(stand_in.matches(column).count(), 1);
    let mutant = stand_in.replacen(column, "", 1);

    assert_stand_in("missing-stream-seq-mutant", &mutant, &all_required());
}

// ---------------------------------------------------------------------------
// wamn-y8wd: the same guard over `deploy/sql/run-state.sql`. The runs table is
// the one a gate's ephemeral DDL drifts from (wamn-thvs), and it is also the
// shape the run-queue-only parser could not read: multi-line CHECK constraints
// whose `),` lines close a constraint, not the table.
// ---------------------------------------------------------------------------

/// Every table `run-state.sql` ships, all Required.
fn all_run_state_required() -> [(&'static str, Need); 7] {
    [
        ("runs", Need::Required),
        ("invocation_admissions", Need::Required),
        ("node_runs", Need::Required),
        ("effect_attempts", Need::Required),
        ("effect_attempt_dispatches", Need::Required),
        ("effect_attempt_outcomes", Need::Required),
        ("operator_run_actions", Need::Required),
    ]
}

/// `runs` Required, every other shipped table absent — the shape of a gate that
/// materializes only the run row.
fn runs_only_spec() -> [(&'static str, Need); 7] {
    let mut spec = all_run_state_required();
    for entry in spec.iter_mut().skip(1) {
        entry.1 = Need::AbsentByDesign;
    }
    spec
}

/// A gate-style `runs` stand-in: one flat statement, in the DDL dialect the
/// harnesses actually `format!`, carrying every shipped column. Written out (not
/// derived from the schema of record) so it proves the guard reads real column
/// names out of `run-state.sql` rather than fragments of its CHECK bodies.
fn runs_stand_in() -> String {
    "CREATE TABLE wamn_run.runs (\
        tenant_id text NOT NULL, run_id text NOT NULL, flow_id text NOT NULL, \
        flow_version int NOT NULL, catalog_id text NOT NULL, catalog_version int NOT NULL, \
        environment text NOT NULL, execution_bundle_hash text NOT NULL, \
        attachment_id text, registration_id text, \
        event_source_run_id text, event_root_run_id text, event_depth int, \
        status text NOT NULL DEFAULT 'running' \
          CHECK (status IN ('dispatched', 'running', 'completed', 'failed', \
                            'infrastructure-failure', 'effect-uncertain')), \
        trigger_source text, capture_mode text NOT NULL DEFAULT 'off', \
        durability_class text NOT NULL DEFAULT 'standard' \
          CHECK (durability_class IN ('standard', 'durable')), \
        release_version int, manifest_digest text, \
        input_json jsonb, result_json jsonb, state_json jsonb, \
        invocation_context jsonb NOT NULL DEFAULT '{}'::jsonb, \
        admission_context_version text NOT NULL DEFAULT '0.1', \
        platform_revision text NOT NULL DEFAULT 'legacy', \
        idempotency_key text, \
        caller_outcome_kind text, caller_outcome_json jsonb, \
        caller_http_status int, caller_release_node_id text, caller_outcome_hash text, \
        caller_released_at timestamptz, response_deadline_at timestamptz, \
        run_deadline_at timestamptz, terminal_reason text, \
        fail_kind text, fail_node text, fail_reason text, \
        created_at timestamptz NOT NULL DEFAULT now(), \
        updated_at timestamptz NOT NULL DEFAULT now(), \
        CHECK (capture_mode <> 'full' OR trigger_source IS NOT DISTINCT FROM 'scenario-draft'), \
        PRIMARY KEY (tenant_id, run_id));"
        .to_string()
}

#[test]
fn run_state_schema_of_record_satisfies_the_stand_in_contract() {
    assert_run_state_stand_in(
        "conformance",
        &uncommented(RUN_STATE_SQL),
        &all_run_state_required(),
    );
}

#[test]
fn run_state_guard_accepts_a_gate_style_stand_in_at_full_column_parity() {
    assert_run_state_stand_in("runs-stand-in-fixture", &runs_stand_in(), &runs_only_spec());
}

#[test]
#[should_panic(expected = "`wamn_run.runs` stand-in missing column `run_id`")]
fn run_state_guard_rejects_a_dropped_column_a_longer_name_would_mask() {
    let column = "run_id text NOT NULL, ";
    let stand_in = runs_stand_in();
    assert_eq!(stand_in.matches(column).count(), 1);
    // `event_root_run_id` and the PRIMARY KEY still name `run_id`, so
    // only a by-NAME column comparison catches the drop.
    let mutant = stand_in.replacen(column, "", 1);
    assert!(
        mutant.contains("event_root_run_id") && mutant.contains("PRIMARY KEY (tenant_id, run_id)")
    );

    assert_run_state_stand_in("missing-run-id-mutant", &mutant, &runs_only_spec());
}

#[test]
#[should_panic(expected = "runs stand-in status CHECK missing literal `infrastructure-failure`")]
fn run_state_guard_rejects_a_status_check_the_run_writers_outgrew() {
    let mutant = runs_stand_in().replacen(", 'infrastructure-failure'", "", 1);

    assert_run_state_stand_in("narrow-status-check-mutant", &mutant, &runs_only_spec());
}

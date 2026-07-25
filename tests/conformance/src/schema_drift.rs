//! Conformance proofs for the run-state-owned stand-in schema guard.

use wamn_run_state::schema_drift::{Need, assert_stand_in};

const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");

fn all_required() -> [(&'static str, Need); 3] {
    [
        ("run_queue", Need::Required),
        ("partition_owner", Need::Required),
        ("run_dead_letters", Need::Required),
    ]
}

fn uncommented_schema_of_record() -> String {
    RUN_QUEUE_SQL
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
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

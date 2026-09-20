//! Guards the run-state SQL surface.

use std::fs;
use std::path::{Path, PathBuf};

const RUN_STATE_SQL_SOURCE: &str = "crates/execution/run-state/src/sql.rs";
const RUN_STATE_QUEUE_SQL_SOURCE: &str = "crates/execution/run-state/src/queue/sql.rs";
const RUN_STATE_TRANSITIONS_SOURCE: &str = "crates/execution/run-state/src/transitions.rs";
const FRESH_RUN_STATE_CARRIERS: &[&str] = &[
    "deploy/sql/run-state.sql",
    "deploy/sql/postgres-init.sql",
    "tests/conformance/src/schema_drift.rs",
    "crates/platform/runtime/tests/common/mod.rs",
];

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn read(relative_path: &str) -> String {
    let path = repository_root().join(relative_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

#[test]
fn guest_safe_run_state_surface_has_no_raw_projection_mutation() {
    for path in [
        RUN_STATE_SQL_SOURCE,
        RUN_STATE_QUEUE_SQL_SOURCE,
        RUN_STATE_TRANSITIONS_SOURCE,
    ] {
        let source = read(path);
        for forbidden in [
            "INSERT INTO node_runs",
            "UPDATE node_runs",
            "insert_node_run_success",
            "insert_node_run_error",
            "reserved_checkpoint_sql",
            "complete_attempt_success_sql",
            "complete_attempt_error_sql",
        ] {
            assert!(
                !source.contains(forbidden),
                "guest-safe source {path} retained projection mutation {forbidden:?}"
            );
        }
    }
}

#[test]
fn retired_uncalled_run_builders_stay_deleted_while_park_remains() {
    let run_state_sql = read(RUN_STATE_SQL_SOURCE);
    for retired in [
        "pub fn insert_run_sql(",
        "pub fn insert_run_returning_id_sql(",
        "pub fn update_run_running_sql(",
        "pub fn update_run_failed_sql(",
        "pub fn update_run_completed(",
        "pub fn update_run_completed_sql(",
        "pub fn select_run_dispatch_sql(",
    ] {
        assert!(
            !run_state_sql.contains(retired),
            "run-state SQL restored retired builder {retired:?}"
        );
    }
    for retired_column in ["fail_node", "fail_reason"] {
        assert!(
            !run_state_sql.contains(retired_column),
            "run-state SQL restored retired failure surface {retired_column:?}"
        );
    }
}

#[test]
fn fresh_run_state_carriers_omit_retired_failure_detail_columns() {
    for path in FRESH_RUN_STATE_CARRIERS {
        let source = read(path);
        for retired_column in ["fail_node", "fail_reason"] {
            assert!(
                !source.contains(retired_column),
                "fresh run-state carrier {path} restored retired failure detail {retired_column:?}"
            );
        }
    }
}

//! The reconciler may not re-widen `wamn_app` on `wamn_run.runs`
//! (wamn-0h0g.12.40).
//!
//! `repair_run_capture_privilege_sql` used to compute a writable set as "every
//! canonical column that is not `capture_mode`", so a reconcile pass would hand
//! back every column the DDL had just withheld, and the add-column arm granted
//! `INSERT (col), UPDATE (col)` for every column added to `runs`. wamn-0h0g.22.7
//! (b1d42599) removed BOTH: `wamn_app` now holds table-level `SELECT` and
//! `DELETE` on `runs` and nothing else, so the reconciler must restore no column
//! write authority at all, and it must name only columns the live table has.

use std::collections::{BTreeMap, BTreeSet};

use wamn_schema_control::{
    BareSchemaName, RunPlaneActionKind, RunPlaneObservation, plan_run_plane,
};

/// Every column `wamn_run.runs` carries, in `deploy/sql/run-state.sql` order.
const CANONICAL_RUN_COLUMNS: &[&str] = &[
    "tenant_id",
    "run_id",
    "flow_id",
    "flow_version",
    "catalog_id",
    "catalog_version",
    "environment",
    "attachment_id",
    "registration_id",
    "event_source_run_id",
    "event_root_run_id",
    "event_depth",
    "status",
    "trigger_source",
    "capture_mode",
    "durability_class",
    "wiring_id",
    "wiring_version",
    "wiring_hash",
    "binding_world_json",
    "release_version",
    "manifest_digest",
    "input_json",
    "result_json",
    "state_json",
    "invocation_context",
    "admission_context_version",
    "platform_revision",
    "idempotency_key",
    "caller_outcome_kind",
    "caller_outcome_json",
    "caller_http_status",
    "caller_release_node_id",
    "caller_outcome_hash",
    "caller_released_at",
    "response_deadline_at",
    "run_deadline_at",
    "terminal_reason",
    "fail_kind",
    "created_at",
    "updated_at",
];

fn schema() -> BareSchemaName {
    BareSchemaName::new("demo").expect("bare schema name")
}

/// An observation whose `runs` carries `columns` and whose application-role
/// grants are maximally broad, so the capture-privilege repair is planned.
fn broadly_granted_runs(columns: &[&str]) -> RunPlaneObservation {
    RunPlaneObservation {
        tables: BTreeMap::from([(
            "runs".to_string(),
            columns.iter().map(|c| (*c).to_string()).collect(),
        )]),
        app_run_capture_privileges: (true, true, true),
        ..RunPlaneObservation::default()
    }
}

fn capture_repair(obs: &RunPlaneObservation) -> String {
    plan_run_plane(&schema(), obs)
        .actions
        .into_iter()
        .find(|action| action.kind == RunPlaneActionKind::RepairRunCapturePrivilege)
        .expect("a broad application-role grant is repaired")
        .sql
}

fn columns_of(list: &str) -> BTreeSet<String> {
    list.split(',')
        .map(|column| column.trim().trim_matches('"').to_string())
        .filter(|column| !column.is_empty())
        .collect()
}

#[test]
fn run_repair_restores_no_application_write_authority() {
    let sql = capture_repair(&broadly_granted_runs(CANONICAL_RUN_COLUMNS));

    // The only GRANTs the repair may emit. A restored column write of ANY shape
    // is the regression this file exists to catch.
    assert!(
        sql.contains("GRANT SELECT, DELETE ON TABLE \"demo\".runs TO wamn_app"),
        "{sql}"
    );
    assert!(!sql.contains("GRANT INSERT"), "{sql}");
    assert!(!sql.contains("GRANT UPDATE"), "{sql}");
    assert!(!sql.contains("GRANT REFERENCES"), "{sql}");

    // The blanket set was "every canonical column except capture_mode", so the
    // REVOKE must still name every live column — that is what takes the legacy
    // grant away.
    let revoked = sql
        .split_once("REVOKE SELECT (")
        .expect("column REVOKE")
        .1
        .split_once(") ON TABLE")
        .expect("the column REVOKE is table-qualified")
        .0
        .split_once("), INSERT (")
        .expect("the REVOKE carries each privilege's column list")
        .0;
    assert_eq!(
        columns_of(revoked),
        CANONICAL_RUN_COLUMNS
            .iter()
            .map(|column| (*column).to_string())
            .collect::<BTreeSet<_>>(),
        "the REVOKE left a live column reachable"
    );

    // Convergence is verified, not assumed: a surviving column write anywhere
    // must abort the repair rather than report success.
    assert!(
        sql.contains("run-capture-author-sql-write-authority"),
        "{sql}"
    );
}

#[test]
fn a_column_absent_from_the_live_table_is_repaired_without_being_invented() {
    let mut columns = CANONICAL_RUN_COLUMNS.to_vec();
    columns.retain(|column| *column != "response_deadline_at");
    let sql = capture_repair(&broadly_granted_runs(&columns));
    assert!(
        !sql.contains("response_deadline_at"),
        "the repair named a column the live table does not have: {sql}"
    );
}

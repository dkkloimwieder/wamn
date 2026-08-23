//! The reconciler may only restore the RATIFIED `wamn_app` column sets on
//! `wamn_run.runs` (wamn-0h0g.12.40).
//!
//! Two separate paths could re-widen what `deploy/sql/run-state.sql` narrowed,
//! and each has its own test here. `repair_run_capture_privilege_sql` used to
//! compute its writable set as "every canonical column that is not
//! `capture_mode`", so a reconcile pass would hand back every column the DDL
//! had just withheld. The add-column arm used to grant `INSERT (col),
//! UPDATE (col)` for EVERY column added to `runs`, which is how
//! `release_version` and `manifest_digest` became application-writable with no
//! decision behind them.

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
    "execution_bundle_hash",
    "attachment_id",
    "registration_id",
    "event_source_run_id",
    "event_root_run_id",
    "event_depth",
    "status",
    "trigger_source",
    "capture_mode",
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

/// The exact INSERT set: the callable admission's run insert, which subsumes
/// every other insert the application role executes.
const RATIFIED_INSERT: &[&str] = &[
    "admission_context_version",
    "attachment_id",
    "catalog_id",
    "catalog_version",
    "environment",
    "event_depth",
    "event_root_run_id",
    "event_source_run_id",
    "execution_bundle_hash",
    "flow_id",
    "flow_version",
    "idempotency_key",
    "input_json",
    "invocation_context",
    "platform_revision",
    "registration_id",
    "response_deadline_at",
    "run_deadline_at",
    "run_id",
    "status",
    "tenant_id",
    "trigger_source",
];

/// The exact UPDATE set: the union of the claim, park, release, and
/// terminalize statements.
const RATIFIED_UPDATE: &[&str] = &[
    "caller_http_status",
    "caller_outcome_hash",
    "caller_outcome_json",
    "caller_outcome_kind",
    "caller_release_node_id",
    "caller_released_at",
    "fail_kind",
    "manifest_digest",
    "release_version",
    "result_json",
    "state_json",
    "status",
    "terminal_reason",
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

fn columns_of(list: &str) -> BTreeSet<String> {
    list.split(',')
        .map(|column| column.trim().trim_matches('"').to_string())
        .filter(|column| !column.is_empty())
        .collect()
}

fn expected(set: &[&str]) -> BTreeSet<String> {
    set.iter().map(|c| (*c).to_string()).collect()
}

#[test]
fn run_repair_grants_only_the_ratified_app_column_sets() {
    let obs = broadly_granted_runs(CANONICAL_RUN_COLUMNS);
    let plan = plan_run_plane(&schema(), &obs);
    let repair = plan
        .actions
        .iter()
        .find(|action| action.kind == RunPlaneActionKind::RepairRunCapturePrivilege)
        .expect("a broad application-role grant is repaired");

    // The replacement grant is the text between the restoring GRANT and the
    // postcondition block; the REVOKE above it names every column by design.
    let replacement = repair
        .sql
        .split_once("GRANT INSERT (")
        .expect("replacement INSERT grant")
        .1
        .split_once("DO $run_capture_acl$")
        .expect("postcondition follows the replacement grant")
        .0;
    let (insert_list, rest) = replacement
        .split_once("), UPDATE (")
        .expect("the replacement grant carries both column lists");
    let update_list = rest
        .split_once(") ON TABLE")
        .expect("the replacement grant is table-qualified")
        .0;

    assert_eq!(
        columns_of(insert_list),
        expected(RATIFIED_INSERT),
        "the repair restored an INSERT set that is not the ratified one"
    );
    assert_eq!(
        columns_of(update_list),
        expected(RATIFIED_UPDATE),
        "the repair restored an UPDATE set that is not the ratified one"
    );

    // The blanket set was "every canonical column except capture_mode". Naming
    // the columns that separate the two shapes is what keeps a revert visible.
    for withheld in [
        "capture_mode",
        "created_at",
        "result_json",
        "state_json",
        "release_version",
        "manifest_digest",
    ] {
        assert!(
            !columns_of(insert_list).contains(withheld),
            "{withheld} must not be app-insertable"
        );
    }
    for withheld in [
        "capture_mode",
        "tenant_id",
        "run_id",
        "flow_id",
        "catalog_id",
        "environment",
        "execution_bundle_hash",
        "event_depth",
        "trigger_source",
        "input_json",
        "idempotency_key",
        "created_at",
    ] {
        assert!(
            !columns_of(update_list).contains(withheld),
            "{withheld} must not be app-updatable"
        );
    }

    // A row-locking clause needs UPDATE on at least one column, so an empty
    // UPDATE set would be an outage rather than a confinement.
    assert!(!columns_of(update_list).is_empty());
}

#[test]
fn a_column_absent_from_the_live_table_is_repaired_without_being_invented() {
    let mut columns = CANONICAL_RUN_COLUMNS.to_vec();
    columns.retain(|column| *column != "response_deadline_at");
    let obs = broadly_granted_runs(&columns);
    let plan = plan_run_plane(&schema(), &obs);
    let repair = plan
        .actions
        .iter()
        .find(|action| action.kind == RunPlaneActionKind::RepairRunCapturePrivilege)
        .expect("a broad application-role grant is repaired");
    let replacement = repair
        .sql
        .split_once("GRANT INSERT (")
        .expect("replacement INSERT grant")
        .1
        .split_once("DO $run_capture_acl$")
        .expect("postcondition follows the replacement grant")
        .0;
    assert!(
        !replacement.contains("response_deadline_at"),
        "the repair granted a column the live table does not have"
    );
}

#[test]
fn a_new_runs_column_earns_only_the_grants_its_ratified_sets_name() {
    // `terminal_reason` is UPDATE-only and `created_at` belongs to neither set,
    // so restoring them exercises both halves of the add-column arm.
    let mut columns = CANONICAL_RUN_COLUMNS.to_vec();
    columns.retain(|column| *column != "terminal_reason" && *column != "created_at");
    let obs = broadly_granted_runs(&columns);
    let plan = plan_run_plane(&schema(), &obs);

    let added = |target: &str| {
        plan.actions
            .iter()
            .find(|action| action.kind == RunPlaneActionKind::AddColumn && action.target == target)
            .unwrap_or_else(|| panic!("{target} is added back"))
    };

    let unratified = added("runs.created_at");
    assert!(
        !unratified.sql.contains("GRANT"),
        "a column no application statement writes was granted anyway: {}",
        unratified.sql
    );

    let update_only = added("runs.terminal_reason");
    assert!(
        update_only
            .sql
            .contains("GRANT UPDATE (\"terminal_reason\") ON TABLE \"demo\".runs TO wamn_app"),
        "the ratified UPDATE column lost its grant: {}",
        update_only.sql
    );
    assert!(
        !update_only.sql.contains("INSERT (\"terminal_reason\")"),
        "a column only the terminalize statements write became insertable: {}",
        update_only.sql
    );
}

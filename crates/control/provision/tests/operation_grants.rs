//! PostgreSQL 18 proof for exact manifest-derived route-caller grants.
//!
//! Set `WAMN_OPERATION_GRANTS_PG18_URL` to a disposable superuser database.
//! The proof reads the mutation counts and final rows from PostgreSQL itself.

use std::io::Write as _;
use std::process::{Command, Stdio};

use wamn_control_provision::operation_grants::{
    APP_SYSTEM_FLOOR_MISSING, OPERATION_CALLER_ROLE, OPERATION_GRANT_TRANSACTION_PRELUDE_SQL,
    OperationGrantReconcileResult, operation_grant_floor_check_sql, reconcile_operation_grants_sql,
};

const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");
const RECEIVING_MANIFEST: &[u8] = include_bytes!("../../../../packages/receiving/wamn.json");
const ENV_VAR: &str = "WAMN_OPERATION_GRANTS_PG18_URL";

fn psql(url: &str, script: &str) -> (bool, String, String) {
    let mut child = Command::new("psql")
        .arg(url)
        .args([
            "-v",
            "ON_ERROR_STOP=1",
            "-v",
            "VERBOSITY=verbose",
            "-tAq",
            "-f",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql");
    child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes())
        .expect("write psql script");
    let output = child.wait_with_output().expect("wait for psql");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn run(url: &str, what: &str, script: &str) -> String {
    let (ok, stdout, stderr) = psql(url, script);
    assert!(ok, "{what} failed:\nstdout:\n{stdout}\nstderr:\n{stderr}");
    stdout.trim().to_owned()
}

fn query(url: &str, statement: &str) -> String {
    run(url, "query server state", statement)
}

fn result(answer: &str) -> OperationGrantReconcileResult {
    let counts = answer
        .lines()
        .last()
        .expect("reconcile returned its final row")
        .split('|')
        .map(|value| value.parse::<i64>().expect("count is bigint"))
        .collect::<Vec<_>>();
    assert_eq!(counts.len(), 3, "reconcile answer has three counts");
    OperationGrantReconcileResult::new(counts[0], counts[1], counts[2])
}

/// `PREPARE` proves the reconciliation query remains one extended-query
/// statement, matching the production driver's `query_one` boundary.
fn transaction(statement: &str) -> String {
    let floor_check = operation_grant_floor_check_sql();
    format!(
        "BEGIN; {OPERATION_GRANT_TRANSACTION_PRELUDE_SQL}; {floor_check} \
         PREPARE operation_grant_reconcile AS {statement} \
         EXECUTE operation_grant_reconcile; COMMIT;"
    )
}

#[test]
fn route_caller_grants_are_exact_residue_free_and_convergent_live() {
    let Ok(url) = std::env::var(ENV_VAR) else {
        eprintln!(
            "skipping route_caller_grants_are_exact_residue_free_and_convergent_live \
             (set {ENV_VAR} to run)"
        );
        return;
    };
    assert!(
        query(&url, "SHOW server_version_num")
            .parse::<u32>()
            .expect("server version is numeric")
            >= 180_000,
        "operation-grant proof requires PostgreSQL 18"
    );
    run(
        &url,
        "reset application authority floor",
        "DROP SCHEMA IF EXISTS app_system CASCADE; \
         DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
         DO $role$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app') THEN \
             CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $role$;",
    );

    let reconcile_statement =
        reconcile_operation_grants_sql(RECEIVING_MANIFEST, "t1").expect("build exact reconcile");
    let reconcile = transaction(&reconcile_statement);
    let (ok, _, missing_stderr) = psql(&url, &reconcile);
    assert!(!ok, "reconcile installed its own missing app_system floor");
    assert!(
        missing_stderr.contains("55000") && missing_stderr.contains(APP_SYSTEM_FLOOR_MISSING),
        "missing-floor refusal lost its SQLSTATE or literal:\n{missing_stderr}"
    );

    run(&url, "install application authority floor", APP_SCHEMA);
    run(
        &url,
        "seed scoped role and grant residue",
        "INSERT INTO app_system.roles (tenant_id, name, is_system) VALUES \
           ('t1', 'route-caller', false), \
           ('t1', 'sibling-role', false), \
           ('t2', 'route-caller', false); \
         INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES \
           ('t1', 'route-caller', 'wamn_receiving@1.0.0::purchase_order.get'), \
           ('t1', 'route-caller', 'wamn_receiving@1.0.0::obsolete.operation'), \
           ('t1', 'route-caller', 'wamn_receiving@1.1.0::purchase_order.get'), \
           ('t1', 'route-caller', 'client_acme_receiving@3.0.0::receiving.submit_receipt'), \
           ('t1', 'sibling-role', 'residue.must.stay'), \
           ('t2', 'route-caller', 'residue.must.stay');",
    );

    let changed = result(&run(&url, "reconcile operation grants", &reconcile));
    assert_eq!(changed.role_rows_changed(), 1, "role was not hardened");
    assert_eq!(changed.grants_added(), 5, "missing grants were not exact");
    assert_eq!(
        changed.grants_removed(),
        1,
        "same-coordinate residue survived"
    );

    assert_eq!(
        query(
            &url,
            "SELECT string_agg(permission, E'\\n' ORDER BY permission) \
               FROM app_system.permissions \
              WHERE tenant_id = 't1' AND role_name = 'route-caller' \
                AND starts_with(permission, 'wamn_receiving@1.0.0::')"
        ),
        [
            "wamn_receiving@1.0.0::purchase_order.get",
            "wamn_receiving@1.0.0::purchase_order.query",
            "wamn_receiving@1.0.0::purchase_order.update",
            "wamn_receiving@1.0.0::receipt.get",
            "wamn_receiving@1.0.0::receipt.query",
            "wamn_receiving@1.0.0::receiving.record_receipt",
        ]
        .join("\n"),
        "server did not retain exactly the manifest's six operation grants"
    );
    assert_eq!(
        query(
            &url,
            "SELECT string_agg(permission, E'\\n' ORDER BY permission) \
               FROM app_system.permissions \
              WHERE tenant_id = 't1' AND role_name = 'route-caller' \
                AND NOT starts_with(permission, 'wamn_receiving@1.0.0::')"
        ),
        [
            "client_acme_receiving@3.0.0::receiving.submit_receipt",
            "wamn_receiving@1.1.0::purchase_order.get",
        ]
        .join("\n"),
        "package reconciliation changed another coordinate's operation grants"
    );
    assert_eq!(
        query(
            &url,
            "SELECT count(*) FROM app_system.permissions \
              WHERE permission = 'residue.must.stay'"
        ),
        "2",
        "reconcile escaped its tenant+role scope"
    );
    assert_eq!(
        query(
            &url,
            &format!(
                "SELECT is_system FROM app_system.roles \
                  WHERE tenant_id = 't1' AND name = '{OPERATION_CALLER_ROLE}'"
            )
        ),
        "t",
        "the operation role is not system-owned"
    );

    let again = result(&run(&url, "replay operation grants", &reconcile));
    assert!(
        again.is_noop(),
        "server changed rows on converged replay: {again:?}"
    );

    run(
        &url,
        "remove application authority floor",
        "DROP SCHEMA app_system CASCADE; DROP SCHEMA wamn_authority CASCADE;",
    );
}

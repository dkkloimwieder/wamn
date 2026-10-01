//! PostgreSQL 18 test of the built-in `admin` role that `apply-package`
//! writes, and of the permission rows of authored roles
//! (docs/plan/platform-ui.md §2.2 to §2.4).
//!
//! The test uses a test database of the test PostgreSQL server as its superuser,
//! and holds the process lock, because it creates cluster-wide roles.

use std::io::Write as _;
use std::process::{Command, Stdio};

use wamn_control_provision::operation_grants::{
    APP_SYSTEM_FLOOR_MISSING, ENSURE_ADMIN_ROLE_SQL, OPERATION_GRANT_TRANSACTION_PRELUDE_SQL,
    operation_grant_floor_check_sql,
};
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql};

const RECORD_HISTORY: &str = include_str!("../../../../deploy/sql/record-history.sql");
const RECORD_HISTORY_APP_GRANTS: &str =
    include_str!("../../../../deploy/sql/record-history-app-grants.sql");
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");
/// The test principal that the fixture seed writes as.
const FIXTURE_PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f1";

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

/// The transaction binds `wamn:apply-package`, as apply-package does, and
/// runs the admin statement for tenant `t1`.
fn transaction() -> String {
    let floor_check = operation_grant_floor_check_sql();
    let actor = bind_platform_principal_sql(PlatformComponent::ApplyPackage);
    format!(
        "BEGIN; {actor} {OPERATION_GRANT_TRANSACTION_PRELUDE_SQL}; {floor_check} \
         PREPARE ensure_admin AS {ENSURE_ADMIN_ROLE_SQL}; \
         EXECUTE ensure_admin('t1'); COMMIT;"
    )
}

/// Run `statement` as the fixture principal and return psql's error text, or
/// `None` when it succeeded.
fn refusal(url: &str, statement: &str) -> Option<String> {
    let (ok, _, stderr) = psql(
        url,
        &format!(
            "BEGIN; SELECT set_config('app.user_id', '{FIXTURE_PRINCIPAL}', true), \
             set_config('app.operation', 'admin:operation-grant-fixture', true); \
             {statement}; COMMIT;"
        ),
    );
    (!ok).then_some(stderr)
}

#[test]
fn admin_role_has_no_rows_and_authored_rows_follow_their_root_live() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();
    assert!(
        query(&url, "SHOW server_version_num")
            .parse::<u32>()
            .expect("server version is numeric")
            >= 180_000,
        "operation-grant test requires PostgreSQL 18"
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

    let (ok, _, missing_stderr) = psql(&url, &transaction());
    assert!(
        !ok,
        "the admin write installed its own missing app_system floor"
    );
    assert!(
        missing_stderr.contains("55000") && missing_stderr.contains(APP_SYSTEM_FLOOR_MISSING),
        "missing-floor refusal lost its SQLSTATE or literal:\n{missing_stderr}"
    );

    run(
        &url,
        "install application authority floor",
        &format!("{RECORD_HISTORY}\n{RECORD_HISTORY_APP_GRANTS}\n{APP_SCHEMA}"),
    );
    run(
        &url,
        "seed an authored role with one root and its closure",
        &format!(
            "BEGIN; SELECT set_config('app.user_id', '{FIXTURE_PRINCIPAL}', true), \
         set_config('app.operation', 'admin:seed-operation-grant-fixture', true); \
         INSERT INTO app_system.users (tenant_id, id, type, email) VALUES \
           ('t1', '{FIXTURE_PRINCIPAL}', 'user', 'fixture@example.invalid'); \
         INSERT INTO app_system.roles (tenant_id, name) VALUES ('t1', 'clerk'); \
         INSERT INTO app_system.permissions (tenant_id, role_name, permission, required_by) VALUES \
           ('t1', 'clerk', 'platform-fixture:widget/create', 'platform-fixture:widget/create'), \
           ('t1', 'clerk', 'platform-fixture:widget/get', 'platform-fixture:widget/create'), \
           ('t1', 'clerk', 'platform-fixture:widget/get', 'platform-fixture:widget/get'); COMMIT;"
        ),
    );

    run(&url, "write the admin role", &transaction());
    run(&url, "replay the admin role", &transaction());
    assert_eq!(
        query(
            &url,
            "SELECT string_agg(name, ',' ORDER BY name) FROM app_system.roles WHERE tenant_id = 't1'"
        ),
        "admin,clerk",
        "apply-package did not leave admin beside the authored role"
    );
    assert_eq!(
        query(&url, "SELECT count(*) FROM app_system.permissions"),
        "3",
        "the admin write changed an authored permission row"
    );

    let admin_row = refusal(
        &url,
        "INSERT INTO app_system.permissions (tenant_id, role_name, permission, required_by) \
         VALUES ('t1', 'admin', 'platform-fixture:widget/get', 'platform-fixture:widget/get')",
    )
    .expect("admin took a permission row");
    assert!(admin_row.contains("permissions_admin_check"), "{admin_row}");
    let sealed = refusal(
        &url,
        "INSERT INTO app_system.permissions (tenant_id, role_name, permission, required_by) \
         VALUES ('t1', 'clerk', 'platform-fixture:widget/list@2.0.0', \
                 'platform-fixture:widget/list@2.0.0')",
    )
    .expect("a sealed operation id was stored as a reference");
    assert!(sealed.contains("permissions_reference_check"), "{sealed}");
    let orphan = refusal(
        &url,
        "INSERT INTO app_system.permissions (tenant_id, role_name, permission, required_by) \
         VALUES ('t1', 'clerk', 'platform-fixture:widget/list', 'platform-fixture:widget/query')",
    )
    .expect("a closure row without its selected root was stored");
    assert!(orphan.contains("permissions_required_by_fkey"), "{orphan}");
    for name in ["Clerk", "-clerk", "clerk role", &"a".repeat(65)] {
        let refused = refusal(
            &url,
            &format!("INSERT INTO app_system.roles (tenant_id, name) VALUES ('t1', '{name}')"),
        )
        .unwrap_or_else(|| panic!("role name {name:?} was stored"));
        assert!(refused.contains("roles_name_check"), "{refused}");
    }

    if let Some(error) = refusal(
        &url,
        "DELETE FROM app_system.permissions WHERE tenant_id = 't1' AND role_name = 'clerk' \
         AND permission = 'platform-fixture:widget/create' \
         AND required_by = 'platform-fixture:widget/create'",
    ) {
        panic!("revoke the root widget/create: {error}");
    }
    assert_eq!(
        query(
            &url,
            "SELECT string_agg(permission || ' by ' || required_by, ',') FROM app_system.permissions"
        ),
        "platform-fixture:widget/get by platform-fixture:widget/get",
        "the revoke of a root kept its closure row or removed another root"
    );

    run(
        &url,
        "remove application authority floor",
        "DROP SCHEMA app_system CASCADE; DROP SCHEMA wamn_authority CASCADE;",
    );
}

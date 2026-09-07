//! Optional live-apply gate for the pure provisioning builders, against a
//! throwaway Postgres when `WAMN_PROVISION_PG_URL` is set (a **superuser** URL —
//! `CREATE DATABASE` / `CREATE ROLE` need it, exactly as the CNPG cluster
//! superuser does in production). Skips cleanly when unset. Shells out to `psql`
//! so the crate retains no database dependency.
//!
//! It drives the **real** builders and asserts their effects on the live
//! cluster: a legacy password-bearing app LOGIN converges to the passwordless
//! NOLOGIN ACL role, the NOLOGIN title role exists, the project database is
//! created under the title role, and CONNECT is confined to `wamn_app` with
//! `PUBLIC` revoked (the isolation backstop — dropping either half of
//! `grant_connect_sql` fails an assertion).
//! End-to-end routing / resolution / cross-database isolation is the
//! `provisionbench` gate's job (it needs two live app-role connections).

use std::io::Write as _;
use std::process::{Command, Stdio};

use wamn_control_provision::{DB_OWNER_ROLE, sql};

#[test]
fn provisioning_builders_apply_on_postgres() {
    let Ok(url) = std::env::var("WAMN_PROVISION_PG_URL") else {
        eprintln!(
            "skipping provisioning_builders_apply_on_postgres (set WAMN_PROVISION_PG_URL to run)"
        );
        return;
    };

    let project = "provtest-a";
    let db = wamn_control_provision::database_name(project); // wamn-db-provtest-a

    let mut script = String::new();
    // Clean slate (a prior failed run may have left the database).
    script.push_str(&sql::drop_database_sql(project));
    script.push_str(";\n");
    // Seed the retired shared-login posture so this proves convergence, not
    // only the fresh-create arm. The candidate is test-owned and deliberately
    // unrelated to any historical production password.
    script.push_str(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
             CREATE ROLE wamn_app LOGIN PASSWORD 'retired-shared-probe' INHERIT; \
           ELSE \
             ALTER ROLE wamn_app LOGIN PASSWORD 'retired-shared-probe' INHERIT; \
           END IF; \
         END $$;\n",
    );
    // The real builders under test.
    script.push_str(&sql::ensure_app_role_sql("wamn_app"));
    script.push('\n');
    script.push_str(&sql::drain_app_role_sessions_sql());
    script.push('\n');
    script.push_str(sql::ensure_db_owner_role_sql());
    script.push('\n');
    script.push_str(&sql::create_database_sql(project));
    script.push_str(";\n");
    script.push_str(&sql::grant_connect_sql(project));
    script.push('\n');

    // Assertions (RAISE EXCEPTION + ON_ERROR_STOP=1 → psql exits non-zero).
    script.push_str(&format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_database WHERE datname = '{db}') THEN \
             RAISE EXCEPTION 'project database {db} was not created'; \
           END IF; \
           IF (SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname = '{db}') \
                <> '{DB_OWNER_ROLE}' THEN \
             RAISE EXCEPTION 'project database {db} is not owned by {DB_OWNER_ROLE}'; \
           END IF; \
           IF NOT has_database_privilege('wamn_app', '{db}', 'CONNECT') THEN \
             RAISE EXCEPTION 'wamn_app lacks CONNECT on {db} (GRANT missing)'; \
           END IF; \
           IF has_database_privilege('wamn_app', '{db}', 'CREATE') \
              OR has_database_privilege('wamn_app', '{db}', 'TEMPORARY') THEN \
             RAISE EXCEPTION 'wamn_app inherited database-owner authority on {db}'; \
           END IF; \
           IF EXISTS ( \
             SELECT 1 FROM pg_database d, aclexplode(d.datacl) a \
             WHERE d.datname = '{db}' AND a.grantee = 0 AND a.privilege_type = 'CONNECT' \
           ) THEN \
             RAISE EXCEPTION 'PUBLIC still has CONNECT on {db} (REVOKE missing)'; \
           END IF; \
         END $$;\n"
    ));
    // The shared app role is a stable passwordless NOLOGIN ACL carrier.
    script.push_str(
        "DO $$ DECLARE r pg_roles%ROWTYPE; BEGIN \
           SELECT * INTO r FROM pg_roles WHERE rolname = 'wamn_app'; \
           IF r IS NULL THEN RAISE EXCEPTION 'wamn_app role missing'; END IF; \
           IF r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole \
              OR r.rolinherit OR r.rolreplication OR r.rolbypassrls THEN \
             RAISE EXCEPTION 'wamn_app is not a NOLOGIN ACL role'; \
           END IF; \
           IF (SELECT rolpassword IS NOT NULL FROM pg_authid \
                WHERE rolname = 'wamn_app') THEN \
             RAISE EXCEPTION 'wamn_app retained a password verifier'; \
           END IF; \
         END $$;\n",
    );
    // The title role owns the database and nothing authenticates as it.
    script.push_str(&format!(
        "DO $$ DECLARE r pg_roles%ROWTYPE; BEGIN \
           SELECT * INTO r FROM pg_roles WHERE rolname = '{DB_OWNER_ROLE}'; \
           IF r IS NULL THEN RAISE EXCEPTION '{DB_OWNER_ROLE} role missing'; END IF; \
           IF r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole \
              OR r.rolinherit OR r.rolreplication OR r.rolbypassrls THEN \
             RAISE EXCEPTION '{DB_OWNER_ROLE} is not a NOLOGIN title-only role'; \
           END IF; \
           IF (SELECT rolpassword IS NOT NULL FROM pg_authid \
                WHERE rolname = '{DB_OWNER_ROLE}') THEN \
             RAISE EXCEPTION '{DB_OWNER_ROLE} unexpectedly has a password'; \
           END IF; \
         END $$;\n"
    ));
    // Teardown (self-contained; never touches shared databases).
    script.push_str(&sql::drop_database_sql(project));
    script.push_str(";\n");

    let mut child = Command::new("psql")
        .arg(&url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// wamn-yk9l. The platform installs `btree_gist`, so an `EXCLUDE USING gist`
/// overlap constraint is expressible from a package's own `CREATE TABLE`.
///
/// The negative control is the point of this gate. Without the extension the
/// same statement refuses with `data type uuid has no default operator class
/// for access method "gist"`, which is what three pilot agents met, so this
/// asserts BOTH directions in one throwaway database.
/// The exact table three pilot agents needed and could not have.
const OVERLAP_PROBE_TABLE: &str = "CREATE TABLE overlap_probe.appointment (\
     id uuid PRIMARY KEY, dock_id uuid NOT NULL, during tstzrange NOT NULL, \
     CONSTRAINT appointment_no_overlap \
     EXCLUDE USING gist (dock_id WITH =, during WITH &&));";

#[test]
fn the_platform_extensions_make_an_exclusion_constraint_reachable() {
    let Ok(url) = std::env::var("WAMN_PROVISION_PG_URL") else {
        eprintln!(
            "skipping the_platform_extensions_make_an_exclusion_constraint_reachable \
             (set WAMN_PROVISION_PG_URL to run)"
        );
        return;
    };

    // 1. WITHOUT the extension: the constraint refuses for want of an operator
    //    class. Run alone, because ON_ERROR_STOP would abandon the script.
    let refusal = psql_output(
        &url,
        &format!(
            "CREATE SCHEMA overlap_probe;\n{OVERLAP_PROBE_TABLE}\nDROP SCHEMA overlap_probe CASCADE;\n"
        ),
    );
    assert!(
        !refusal.status.success(),
        "the constraint must refuse before the extension is installed"
    );
    let stderr = String::from_utf8_lossy(&refusal.stderr).to_string();
    assert!(
        stderr.contains("no default operator class for access method \"gist\""),
        "unexpected refusal:\n{stderr}"
    );

    // 2. WITH it: the same table is created, the first booking is accepted, and
    //    an overlapping second is refused BY THE DATABASE.
    let mut script = String::new();
    script.push_str(&sql::install_platform_extensions_sql());
    script.push_str("DROP SCHEMA IF EXISTS overlap_probe CASCADE;\n");
    script.push_str("CREATE SCHEMA overlap_probe;\n");
    script.push_str(OVERLAP_PROBE_TABLE);
    script.push_str(
        "\nINSERT INTO overlap_probe.appointment \
         VALUES (gen_random_uuid(), '11111111-1111-1111-1111-111111111111', \
         tstzrange('2026-10-01 09:00Z','2026-10-01 10:00Z'));\n",
    );
    let accepted = psql_output(&url, &script);
    assert!(
        accepted.status.success(),
        "the constraint must be reachable once the platform installs its extensions:\n{}",
        String::from_utf8_lossy(&accepted.stderr)
    );

    let overlap = psql_output(
        &url,
        "INSERT INTO overlap_probe.appointment \
         VALUES (gen_random_uuid(), '11111111-1111-1111-1111-111111111111', \
         tstzrange('2026-10-01 09:30Z','2026-10-01 10:30Z'));\n",
    );
    assert!(
        !overlap.status.success(),
        "an overlapping booking must be refused by the constraint"
    );
    assert!(
        String::from_utf8_lossy(&overlap.stderr)
            .contains("conflicting key value violates exclusion constraint"),
        "unexpected overlap result:\n{}",
        String::from_utf8_lossy(&overlap.stderr)
    );

    let cleanup = psql_output(&url, "DROP SCHEMA IF EXISTS overlap_probe CASCADE;\n");
    assert!(cleanup.status.success(), "probe schema removed");
}

/// Run one script through `psql` and hand back the whole outcome, so a test can
/// assert on a REFUSAL as readily as on success.
fn psql_output(url: &str, script: &str) -> std::process::Output {
    let mut child = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

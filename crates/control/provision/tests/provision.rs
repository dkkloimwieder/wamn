//! Optional live-apply gate for the pure provisioning builders, against a
//! throwaway Postgres when `WAMN_PROVISION_PG_URL` is set (a **superuser** URL —
//! `CREATE DATABASE` / `CREATE ROLE` need it, exactly as the CNPG cluster
//! superuser does in production). Skips cleanly when unset. Shells out to `psql`
//! (no DB dependency in the crate), the wamn-ddl / wamn-rls / wamn-seed pattern.
//!
//! It drives the **real** builders and asserts their effects on the live
//! cluster: the least-privilege role exists, the project database is created,
//! and CONNECT is confined to `wamn_app` with `PUBLIC` revoked (the isolation
//! backstop — dropping either half of `grant_connect_sql` fails an assertion).
//! End-to-end routing / resolution / cross-database isolation is the
//! `provisionbench` gate's job (it needs two live app-role connections).

use std::io::Write as _;
use std::process::{Command, Stdio};

use wamn_provision::sql;

#[test]
fn provisioning_builders_apply_on_postgres() {
    let Ok(url) = std::env::var("WAMN_PROVISION_PG_URL") else {
        eprintln!(
            "skipping provisioning_builders_apply_on_postgres (set WAMN_PROVISION_PG_URL to run)"
        );
        return;
    };

    let project = "provtest-a";
    let db = wamn_provision::database_name(project); // wamn-db-provtest-a

    let mut script = String::new();
    // Clean slate (a prior failed run may have left the database).
    script.push_str(&sql::drop_database_sql(project));
    script.push_str(";\n");
    // The real builders under test.
    script.push_str(&sql::ensure_app_role_sql("wamn_app"));
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
           IF NOT has_database_privilege('wamn_app', '{db}', 'CONNECT') THEN \
             RAISE EXCEPTION 'wamn_app lacks CONNECT on {db} (GRANT missing)'; \
           END IF; \
           IF EXISTS ( \
             SELECT 1 FROM pg_database d, aclexplode(d.datacl) a \
             WHERE d.datname = '{db}' AND a.grantee = 0 AND a.privilege_type = 'CONNECT' \
           ) THEN \
             RAISE EXCEPTION 'PUBLIC still has CONNECT on {db} (REVOKE missing)'; \
           END IF; \
         END $$;\n"
    ));
    // The shared app role is least-privilege.
    script.push_str(
        "DO $$ DECLARE r pg_roles%ROWTYPE; BEGIN \
           SELECT * INTO r FROM pg_roles WHERE rolname = 'wamn_app'; \
           IF r IS NULL THEN RAISE EXCEPTION 'wamn_app role missing'; END IF; \
           IF r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolbypassrls THEN \
             RAISE EXCEPTION 'wamn_app is not least-privilege (super=% createdb=% createrole=% bypassrls=%)', \
               r.rolsuper, r.rolcreatedb, r.rolcreaterole, r.rolbypassrls; \
           END IF; \
         END $$;\n",
    );
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

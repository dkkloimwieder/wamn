//! Live-apply gate for the CDC capture builders (wamn-l5i9.9, D19 v3 §4).
//!
//! Set `WAMN_CDC_PG_URL` to a **superuser** URL of a throwaway Postgres running
//! with `wal_level=logical` (e.g. `docker run … postgres:18 -c
//! wal_level=logical`); skipped cleanly when unset. Applies the REAL builders
//! via psql and asserts the live substrate: the publication covers the schema
//! and auto-includes a table created later (`FOR TABLES IN SCHEMA`), the slot
//! is pgoutput + non-temporary + failover-enabled (the exact shape
//! pg_walstream's `ensure_replication_slot` tolerates), the role is
//! `REPLICATION` and otherwise least-privilege, the grants hold, and a
//! re-apply is a no-op (idempotent). Teardown drops the slot (releasing pinned
//! WAL), the database, and the role.

use std::io::Write as _;
use std::process::{Command, Stdio};

use wamn_provision::{cdc_object_name, project_env_database_name, sql};

/// Swap the database path segment of a libpq URL (the test controls the URL —
/// no query string).
fn swap_db(url: &str, db: &str) -> String {
    let (base, _) = url.rsplit_once('/').expect("url has a path");
    format!("{base}/{db}")
}

fn psql(url: &str, script: &str) -> std::process::Output {
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

fn run_ok(url: &str, script: &str) {
    let out = psql(url, script);
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn cdc_substrate_applies_and_is_idempotent_on_postgres() {
    let Ok(url) = std::env::var("WAMN_CDC_PG_URL") else {
        eprintln!(
            "skipping cdc_substrate_applies_and_is_idempotent_on_postgres (set WAMN_CDC_PG_URL to run)"
        );
        return;
    };

    let (org, project, env) = ("acme", "billing", "dev");
    let db = project_env_database_name(org, project, env);
    let cdc = cdc_object_name(org, project, env);
    let schema = "app";

    // Maintenance DB: fresh database + the cluster-global replication role.
    // The role is dropped first so every run exercises the CURRENT builder —
    // a leftover healthy role would satisfy the IF NOT EXISTS guard and mask a
    // mutated builder (the M2 gate-blind-spot lesson).
    run_ok(
        &url,
        &format!(
            "{drop_db};\nDROP ROLE IF EXISTS \"{cdc}\";\n{create_db};\n{role}\n",
            drop_db = sql::drop_database_named_sql(&db),
            create_db = sql::create_database_named_sql(&db),
            role = sql::ensure_replication_role_sql(&cdc, "wamn_cdc"),
        ),
    );

    // Project-env DB: the CDC bundle (schema guard → publication → failover
    // slot → grants), then a table created AFTER the publication, then the
    // live assertions.
    let db_url = swap_db(&url, &db);
    let cdc_sql = format!(
        "{schema_guard};\n{publication}\n{slot}\n{grants}\n",
        schema_guard = sql::ensure_schema_sql(schema),
        publication = sql::create_publication_sql(&cdc, schema),
        slot = sql::create_failover_slot_sql(&cdc),
        grants = sql::grant_replication_access_sql(&db, &cdc, schema),
    );
    run_ok(&db_url, &cdc_sql);
    // Idempotency: a second apply of the same bundle is a clean no-op.
    run_ok(&db_url, &cdc_sql);

    run_ok(
        &db_url,
        &format!(
            r#"
CREATE TABLE {schema}.receipts (id uuid PRIMARY KEY, qty numeric(8,3));
DO $$ BEGIN
  ASSERT (SELECT count(*) FROM pg_publication WHERE pubname = '{cdc}') = 1,
    'the publication exists exactly once (idempotent re-apply)';
  ASSERT (SELECT puballtables FROM pg_publication WHERE pubname = '{cdc}') = false,
    'the publication is schema-scoped, never FOR ALL TABLES';
  ASSERT (SELECT count(*) FROM pg_publication_tables
            WHERE pubname = '{cdc}' AND schemaname = '{schema}' AND tablename = 'receipts') = 1,
    'FOR TABLES IN SCHEMA auto-includes a table created AFTER the publication';
  ASSERT (SELECT count(*) FROM pg_replication_slots WHERE slot_name = '{cdc}') = 1,
    'the failover slot exists exactly once (idempotent re-apply)';
  ASSERT (SELECT plugin FROM pg_replication_slots WHERE slot_name = '{cdc}') = 'pgoutput'
     AND (SELECT temporary FROM pg_replication_slots WHERE slot_name = '{cdc}') = false
     AND (SELECT two_phase FROM pg_replication_slots WHERE slot_name = '{cdc}') = false
     AND (SELECT failover FROM pg_replication_slots WHERE slot_name = '{cdc}') = true,
    'the slot is pgoutput, durable, single-phase, FAILOVER-enabled (the pg_walstream shape)';
  ASSERT (SELECT database FROM pg_replication_slots WHERE slot_name = '{cdc}') = '{db}',
    'the logical slot is bound to the project-env database';
  ASSERT (SELECT rolreplication FROM pg_roles WHERE rolname = '{cdc}') = true,
    'the role carries REPLICATION';
  ASSERT (SELECT rolsuper FROM pg_roles WHERE rolname = '{cdc}') = false
     AND (SELECT rolcreatedb FROM pg_roles WHERE rolname = '{cdc}') = false
     AND (SELECT rolbypassrls FROM pg_roles WHERE rolname = '{cdc}') = false,
    'the role is otherwise least-privilege (R8b tier)';
  ASSERT has_database_privilege('{cdc}', '{db}', 'CONNECT'),
    'the role may CONNECT to the project-env database';
  ASSERT has_schema_privilege('{cdc}', '{schema}', 'USAGE'),
    'the role has USAGE on the app schema';
  ASSERT has_table_privilege('{cdc}', '{schema}.receipts'::regclass, 'SELECT') = false,
    'a table created AFTER the grant is not retro-granted (decoding needs no SELECT)';
END $$;
"#,
        ),
    );

    // Teardown: slot first (releases the pinned WAL deterministically; an
    // in-use slot would block DROP DATABASE, idle ones are dropped with it),
    // then the database (removes publication + grants), then the role.
    run_ok(&db_url, &sql::drop_replication_slot_sql(&cdc));
    run_ok(
        &url,
        &format!(
            "{drop_db};\nDROP ROLE IF EXISTS \"{cdc}\";\n",
            drop_db = sql::drop_database_named_sql(&db),
        ),
    );
}

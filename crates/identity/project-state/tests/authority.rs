//! The live write-authority gate for `deploy/sql/app-schema.sql` (R11).
//!
//! `tests/schema.rs` pins the GRANT lines as TEXT. This suite proves PostgreSQL
//! actually enforces them: every revoke lands with its positive assertion — this
//! principal, on this relation, denied — rather than as a paper narrowing.
//!
//! One test per adjudicated class:
//! - `users` / `roles` / `user_roles` / `permissions` / `api_keys` are the rows
//!   the trust chain reads as authorization INPUT, so `wamn_app` may read them
//!   and nothing more;
//! - `audit_log` takes appends from the audited party and refuses rewrites;
//! - `configurations` stays fully writable — the class the platform has no
//!   jurisdiction over, and the control proving the other two tests fail for the
//!   revoked privilege rather than for an over-broad narrowing.
//!
//! Gated on `WAMN_SYSSCHEMA_PG_URL` (a superuser URL; the harness provisions
//! `wamn_app`) and skipped cleanly when unset — the `tests/schema.rs`
//! live-apply convention.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

/// The tenant every probe runs under, and the user its seeded rows belong to.
const TENANT: &str = "t1";
const U1: &str = "11111111-1111-1111-1111-111111111111";

/// Each test rebuilds `app_system` in the target database, so they take turns
/// (cargo runs the tests in one binary on parallel threads).
static LIVE_DB: Mutex<()> = Mutex::new(());

fn app_schema_sql() -> String {
    let deploy = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy");
    std::fs::read_to_string(deploy.join("sql/app-schema.sql"))
        .expect("read deploy/sql/app-schema.sql")
}

/// The live gate's URL, or `None` after printing the skip notice.
fn live_url(test: &str) -> Option<String> {
    match std::env::var("WAMN_SYSSCHEMA_PG_URL") {
        Ok(url) => Some(url),
        Err(_) => {
            eprintln!("skipping {test} (set WAMN_SYSSCHEMA_PG_URL to run)");
            None
        }
    }
}

/// The superuser prelude: a production-shaped `wamn_app` (NOSUPERUSER, no
/// BYPASSRLS), a fresh `app_system` applied verbatim from the DDL of record, and
/// one tenant's rows for the probes to aim at. Seeded as the superuser, so the
/// seed itself is unaffected by the grants under test.
fn prelude() -> String {
    let mut script = String::from(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS app_system CASCADE;\n",
    );
    script.push_str(&app_schema_sql());
    script.push_str(&format!(
        r#"
INSERT INTO app_system.users (tenant_id, id, email) VALUES ('{TENANT}','{U1}','u1@t1');
INSERT INTO app_system.roles (tenant_id, name, is_system) VALUES ('{TENANT}','admin',true),('{TENANT}','auditor',false);
INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) VALUES ('{TENANT}','{U1}','admin');
INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES ('{TENANT}','admin','receipts:read');
INSERT INTO app_system.api_keys (tenant_id, user_id, name, key_hash, prefix) VALUES ('{TENANT}','{U1}','ci','hash-1','wk_a');
INSERT INTO app_system.configurations (tenant_id, config_key, config_value) VALUES ('{TENANT}','theme','"dark"'::jsonb);
INSERT INTO app_system.audit_log (tenant_id, actor_id, action) VALUES ('{TENANT}','{U1}','user.login');
"#
    ));
    script
}

const TEARDOWN: &str = "\nDROP SCHEMA app_system CASCADE;\n";

/// Run `script` through `psql`, failing the test with its stderr if any statement
/// or `ASSERT` does.
fn run(url: &str, script: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};

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
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The five relations the trust chain resolves `app.user_id` / `app.role` from
/// are readable by `wamn_app` and writable by nobody through it: author SQL that
/// could insert its own `user_roles` row would be minting the input its own
/// generated policies are then evaluated against.
///
/// Each denied statement is RLS-LEGAL for the probe tenant — same tenant claim,
/// FKs satisfied, no key collision — so `42501` here is the revoked privilege and
/// not a `WITH CHECK` rejection, which shares the SQLSTATE. That the tenant floor
/// does admit such a row under `wamn_app` is what the other two tests prove.
#[test]
fn author_sql_cannot_write_the_relations_that_authorize_it() {
    let Some(url) = live_url("author_sql_cannot_write_the_relations_that_authorize_it") else {
        return;
    };
    let _live = LIVE_DB.lock().unwrap_or_else(PoisonError::into_inner);

    let mut script = prelude();
    script.push_str(&format!(
        r#"
DO $$
DECLARE relation text; operation text;
BEGIN
  FOREACH relation IN ARRAY ARRAY['users','roles','user_roles','permissions','api_keys'] LOOP
    ASSERT has_table_privilege('wamn_app'::name, ('app_system.'||relation)::text, 'SELECT'::text),
      format('%s must stay readable — the platform revokes writes, not reads', relation);
    FOREACH operation IN ARRAY ARRAY['INSERT','UPDATE','DELETE','TRUNCATE'] LOOP
      ASSERT NOT has_table_privilege('wamn_app'::name, ('app_system.'||relation)::text, operation),
        format('wamn_app holds %s on %s — author SQL can mint its own authorization', operation, relation);
    END LOOP;
  END LOOP;
END $$;

BEGIN;
SET LOCAL ROLE wamn_app;
SET LOCAL app.tenant = '{TENANT}';

DO $$ BEGIN
  ASSERT (SELECT count(*) FROM app_system.users) = 1,
    'the tenant claim is live and users is still readable';
  ASSERT (SELECT count(*) FROM app_system.user_roles) = 1,
    'the role linkage 4.2 resolves app.role from is still readable';
END $$;

DO $$
DECLARE probe_sql text;
BEGIN
  FOREACH probe_sql IN ARRAY ARRAY[
    'INSERT INTO app_system.users (tenant_id, email) VALUES (''{TENANT}'', ''intruder@t1'')',
    'UPDATE app_system.users SET status = ''disabled''',
    'DELETE FROM app_system.users',
    'INSERT INTO app_system.roles (tenant_id, name) VALUES (''{TENANT}'', ''superadmin'')',
    'UPDATE app_system.roles SET is_system = false',
    'DELETE FROM app_system.roles',
    'INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) VALUES (''{TENANT}'', ''{U1}'', ''auditor'')',
    'UPDATE app_system.user_roles SET role_name = ''admin''',
    'DELETE FROM app_system.user_roles',
    'INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES (''{TENANT}'', ''admin'', ''users:write'')',
    'UPDATE app_system.permissions SET permission = ''users:write''',
    'DELETE FROM app_system.permissions',
    'INSERT INTO app_system.api_keys (tenant_id, user_id, name, key_hash, prefix) VALUES (''{TENANT}'', ''{U1}'', ''forged'', ''hash-2'', ''wk_b'')',
    'UPDATE app_system.api_keys SET revoked_at = now()',
    'DELETE FROM app_system.api_keys'
  ] LOOP
    BEGIN
      EXECUTE probe_sql;
      RAISE EXCEPTION 'author SQL mutated a platform-protected relation: %', probe_sql;
    EXCEPTION WHEN insufficient_privilege THEN NULL;
    END;
  END LOOP;
END $$;

ROLLBACK;
"#
    ));
    script.push_str(TEARDOWN);
    run(&url, &script);
}

/// `audit_log` is neither platform-protected nor tenant-owned: appending to your
/// own history is the point, rewriting or erasing it is what "audit" forbids. The
/// grant is the entire mechanism — there is no trigger — so this is where the
/// header's append-only claim is actually cashed.
///
/// The successful append also proves the tenant floor admits a well-formed
/// same-tenant row under `wamn_app`, which is what lets the sibling test read a
/// `42501` as "privilege revoked" rather than "policy rejected".
#[test]
fn the_audited_party_may_append_to_its_trail_but_not_rewrite_it() {
    let Some(url) = live_url("the_audited_party_may_append_to_its_trail_but_not_rewrite_it") else {
        return;
    };
    let _live = LIVE_DB.lock().unwrap_or_else(PoisonError::into_inner);

    let mut script = prelude();
    script.push_str(&format!(
        r#"
DO $$ BEGIN
  ASSERT has_table_privilege('wamn_app'::name, 'app_system.audit_log'::text, 'SELECT'::text),
    'the audit trail must stay readable by its tenant';
  ASSERT has_table_privilege('wamn_app'::name, 'app_system.audit_log'::text, 'INSERT'::text),
    'the audit trail must stay appendable — append-only is not read-only';
  ASSERT NOT has_table_privilege('wamn_app'::name, 'app_system.audit_log'::text, 'UPDATE'::text),
    'wamn_app holds UPDATE on the audit trail — the audited party can rewrite it';
  ASSERT NOT has_table_privilege('wamn_app'::name, 'app_system.audit_log'::text, 'DELETE'::text),
    'wamn_app holds DELETE on the audit trail — the audited party can erase it';
  ASSERT NOT has_table_privilege('wamn_app'::name, 'app_system.audit_log'::text, 'TRUNCATE'::text),
    'wamn_app holds TRUNCATE on the audit trail';
END $$;

BEGIN;
SET LOCAL ROLE wamn_app;
SET LOCAL app.tenant = '{TENANT}';

DO $$ BEGIN
  INSERT INTO app_system.audit_log (tenant_id, actor_id, action)
    VALUES ('{TENANT}', '{U1}', 'probe.append');
  ASSERT (SELECT count(*) FROM app_system.audit_log) = 2,
    'the audited party may append to its own trail';
END $$;

DO $$
DECLARE probe_sql text;
BEGIN
  FOREACH probe_sql IN ARRAY ARRAY[
    'UPDATE app_system.audit_log SET action = ''user.logout''',
    'DELETE FROM app_system.audit_log'
  ] LOOP
    BEGIN
      EXECUTE probe_sql;
      RAISE EXCEPTION 'the audited party rewrote its own audit trail: %', probe_sql;
    EXCEPTION WHEN insufficient_privilege THEN NULL;
    END;
  END LOOP;
END $$;

ROLLBACK;
"#
    ));
    script.push_str(TEARDOWN);
    run(&url, &script);
}

/// `configurations` is tenant business state: nothing in the trust chain reads
/// it, so the platform has no standing to narrow it. This is the over-revocation
/// tripwire — if a future sweep applies the class-1 treatment schema-wide, this
/// is the test that fails.
#[test]
fn a_project_still_owns_its_own_configuration() {
    let Some(url) = live_url("a_project_still_owns_its_own_configuration") else {
        return;
    };
    let _live = LIVE_DB.lock().unwrap_or_else(PoisonError::into_inner);

    let mut script = prelude();
    script.push_str(&format!(
        r#"
DO $$
DECLARE operation text;
BEGIN
  FOREACH operation IN ARRAY ARRAY['SELECT','INSERT','UPDATE','DELETE'] LOOP
    ASSERT has_table_privilege('wamn_app'::name, 'app_system.configurations'::text, operation),
      format('configurations is tenant business state — %s must stay granted', operation);
  END LOOP;
END $$;

BEGIN;
SET LOCAL ROLE wamn_app;
SET LOCAL app.tenant = '{TENANT}';

DO $$ BEGIN
  INSERT INTO app_system.configurations (tenant_id, config_key, config_value)
    VALUES ('{TENANT}', 'probe', 'true'::jsonb);
  ASSERT (SELECT count(*) FROM app_system.configurations WHERE config_key = 'probe') = 1,
    'a project may add its own configuration';
  UPDATE app_system.configurations SET config_value = 'false'::jsonb WHERE config_key = 'probe';
  ASSERT (SELECT config_value FROM app_system.configurations WHERE config_key = 'probe') = 'false'::jsonb,
    'a project may rewrite its own configuration';
  DELETE FROM app_system.configurations WHERE config_key = 'probe';
  ASSERT (SELECT count(*) FROM app_system.configurations WHERE config_key = 'probe') = 0,
    'a project may remove its own configuration';
END $$;

ROLLBACK;
"#
    ));
    script.push_str(TEARDOWN);
    run(&url, &script);
}

//! The live write-authority gate for `deploy/sql/app-schema.sql` (R11).
//!
//! `tests/schema.rs` pins the GRANT lines as TEXT. This suite tests that PostgreSQL
//! actually enforces them: every revoke lands with its positive assertion — this
//! principal, on this relation, denied — rather than as a paper narrowing.
//!
//! One test per adjudicated class:
//! - `users` / `roles` / `user_roles` / `permissions` / `api_keys` are the rows
//!   the trust chain reads as authorization INPUT, so an App generation may
//!   inherit their stable `wamn_app` reads and nothing more, so a tenant or an
//!   application cannot create a platform row or a `wamn:` name;
//! - `configurations` stays fully writable — the class the platform has no
//!   jurisdiction over, and the control proving the other test fails for the
//!   revoked privilege rather than for an over-broad narrowing;
//! - the six history tables take no read and no direct write from an App
//!   generation, except the entries that its `configurations` writes append.
//!
//! Gated on `WAMN_SYSSCHEMA_PG_URL` (a superuser URL; the harness prepares one
//! tenant-scoped App generation) and skipped cleanly when unset — the `tests/schema.rs`
//! live-apply convention.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};

/// The tenant every probe runs under, and the user its seeded rows belong to.
const TENANT: &str = "t1";
const U1: &str = "11111111-1111-1111-1111-111111111111";
/// An unused person id for the refused inserts.
const U2: &str = "22222222-2222-2222-2222-222222222222";
/// The pinned `wamn:provisioning` id, the row a tenant must not create.
const PROVISIONING: &str = "770df186-ac15-579e-b46b-c297cae2011b";
const APP_GENERATION_PASSWORD: &str = "test-owned-app-generation-password";
const APP_GENERATION_VALID_UNTIL: &str = "2099-01-01T00:00:00Z";

/// Each test rebuilds `app_system` in the target database, so they take turns
/// (cargo runs the tests in one binary on parallel threads).
static LIVE_DB: Mutex<()> = Mutex::new(());

/// `deploy/sql/record-history.sql`, `deploy/sql/record-history-app-grants.sql`,
/// and `deploy/sql/app-schema.sql`, the order every tenant applier uses.
fn app_schema_sql() -> String {
    let deploy = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy");
    let record_history = std::fs::read_to_string(deploy.join("sql/record-history.sql"))
        .expect("read deploy/sql/record-history.sql");
    let app_grants = std::fs::read_to_string(deploy.join("sql/record-history-app-grants.sql"))
        .expect("read deploy/sql/record-history-app-grants.sql");
    let app_schema = std::fs::read_to_string(deploy.join("sql/app-schema.sql"))
        .expect("read deploy/sql/app-schema.sql");
    format!("{record_history}\n{app_grants}\n{app_schema}")
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

fn current_database(url: &str) -> String {
    use std::process::Command;

    let output = Command::new("psql")
        .arg(url)
        .args(["-X", "-Atq", "-c", "SELECT current_database()"])
        .output()
        .expect("spawn psql (is it installed?)");
    assert!(
        output.status.success(),
        "current_database() probe failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let database = String::from_utf8(output.stdout).expect("database name is utf-8");
    let database = database.trim();
    assert!(!database.is_empty(), "current_database() returned no name");
    database.to_owned()
}

/// The superuser prelude: a production-prepared tenant-scoped App generation,
/// the stable passwordless `wamn_app` ACL carrier it inherits, a fresh
/// `app_system` applied verbatim from the DDL of record, and one tenant's rows.
/// Seeded as the superuser, so the seed itself is unaffected by the grants under
/// test. The fixture writes as U1, its test principal, whose row stamps itself,
/// under an administrative operation, and both bindings stay for the probes.
/// Returns the generation name for the probes' `current_user`.
fn prelude(url: &str) -> (String, String) {
    let database = current_database(url);
    let app_generation = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: TENANT,
            database: &database,
        },
        CredentialGeneration::A,
    )
    .expect("App accepts tenant scope");
    let mut script = sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::App,
        &database,
        &app_generation,
        APP_GENERATION_PASSWORD,
        APP_GENERATION_VALID_UNTIL,
    );
    script.push_str("\nDROP SCHEMA IF EXISTS app_system CASCADE;\n");
    script.push_str(&app_schema_sql());
    script.push_str(&format!(
        r#"
SET app.user_id = '{U1}';
SET app.operation = 'admin:seed-authority-fixture';
INSERT INTO app_system.users (tenant_id, id, type, email) VALUES ('{TENANT}','{U1}','person','u1@t1');
INSERT INTO app_system.roles (tenant_id, name, is_system) VALUES ('{TENANT}','admin',true),('{TENANT}','auditor',false);
INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) VALUES ('{TENANT}','{U1}','admin');
INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES ('{TENANT}','admin','receipts:read');
INSERT INTO app_system.api_keys (tenant_id, user_id, name, key_hash, prefix) VALUES ('{TENANT}','{U1}','ci','hash-1','wk_a');
INSERT INTO app_system.configurations (tenant_id, config_key, config_value) VALUES ('{TENANT}','theme','"dark"'::jsonb);
"#
    ));
    (app_generation, script)
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
/// are readable through `wamn_app` and writable by nobody through it: author SQL that
/// could insert its own `user_roles` row would be minting the input its own
/// generated policies are then evaluated against.
///
/// Each denied statement is RLS-LEGAL for the probe tenant — same generation,
/// FKs satisfied, no key collision — so `42501` here is the revoked privilege and
/// not a `WITH CHECK` rejection, which shares the SQLSTATE. That the tenant floor
/// does admit such a row under the App generation is what the configurations test shows.
#[test]
fn author_sql_cannot_write_the_relations_that_authorize_it() {
    let Some(url) = live_url("author_sql_cannot_write_the_relations_that_authorize_it") else {
        return;
    };
    let _live = LIVE_DB.lock().unwrap_or_else(PoisonError::into_inner);

    let (app_generation, mut script) = prelude(&url);
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
SET LOCAL ROLE {app_generation};

DO $$ BEGIN
  ASSERT current_user = '{app_generation}',
    'the tenant authority is the prepared App generation';
  ASSERT (SELECT count(*) FROM app_system.users) = 1,
    'the generation-derived tenant is live and users is still readable';
  ASSERT (SELECT count(*) FROM app_system.user_roles) = 1,
    'the role linkage 4.2 resolves app.role from is still readable';
END $$;

DO $$
DECLARE probe_sql text;
BEGIN
  FOREACH probe_sql IN ARRAY ARRAY[
    'INSERT INTO app_system.users (tenant_id, id, type, email) VALUES (''{TENANT}'', ''{U2}'', ''person'', ''intruder@t1'')',
    'INSERT INTO app_system.users (tenant_id, id, type, email, display_name) VALUES (''{TENANT}'', ''{PROVISIONING}'', ''platform'', ''provisioning@example.invalid'', ''wamn:provisioning'')',
    'UPDATE app_system.users SET status = ''disabled''',
    'UPDATE app_system.users SET display_name = ''wamn:intruder''',
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

/// `configurations` is tenant business state: nothing in the trust chain reads
/// it, so the platform has no standing to narrow it. This is the over-revocation
/// tripwire — if a future sweep applies the class-1 treatment schema-wide, this
/// is the test that fails. Each write of the App generation appends one entry
/// to `configurations_history` through the log trigger, with the tenant, the
/// bound operation, and the bound actor.
#[test]
fn a_project_still_owns_its_own_configuration() {
    let Some(url) = live_url("a_project_still_owns_its_own_configuration") else {
        return;
    };
    let _live = LIVE_DB.lock().unwrap_or_else(PoisonError::into_inner);

    let (app_generation, mut script) = prelude(&url);
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
SET LOCAL ROLE {app_generation};

DO $$ BEGIN
  ASSERT current_user = '{app_generation}',
    'the tenant authority is the prepared App generation';
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

RESET ROLE;
DO $$ BEGIN
  ASSERT (SELECT array_agg(kind || '|' || tenant_id || '|' || operation || '|' || changed_by
                           ORDER BY position)
            FROM app_system.configurations_history
           WHERE row_key = '{{"tenant_id": "{TENANT}", "config_key": "probe"}}'::jsonb)
         = ARRAY['insert|{TENANT}|admin:seed-authority-fixture|{U1}',
                 'update|{TENANT}|admin:seed-authority-fixture|{U1}',
                 'delete|{TENANT}|admin:seed-authority-fixture|{U1}'],
    'each configurations write of the App generation appends one entry';
END $$;

ROLLBACK;
"#
    ));
    script.push_str(TEARDOWN);
    run(&url, &script);
}

/// The history tables of `app_system` (epic ruling 76). An App generation
/// reads no history table and writes none directly, except
/// `configurations_history`, whose entry columns it inserts when the log
/// trigger fires on its own `configurations` write. The grant leaves out
/// `position`, so the generation cannot choose a position with
/// `OVERRIDING SYSTEM VALUE`.
///
/// Each denied insert is RLS-legal for the probe tenant, so `42501` here is
/// the missing privilege and not a `WITH CHECK` rejection. The configurations
/// test shows that the same generation appends an entry through the trigger.
#[test]
fn author_sql_appends_history_only_through_the_configurations_trigger() {
    let Some(url) = live_url("author_sql_appends_history_only_through_the_configurations_trigger")
    else {
        return;
    };
    let _live = LIVE_DB.lock().unwrap_or_else(PoisonError::into_inner);

    let (app_generation, mut script) = prelude(&url);
    // One direct entry insert. A position also names the position column.
    let entry = |history: &str, position: Option<i64>, row_key: &str| {
        let (column, value, overriding) = match position {
            None => ("", String::new(), ""),
            Some(position) => (
                "position, ",
                format!("{position}, "),
                " OVERRIDING SYSTEM VALUE",
            ),
        };
        format!(
            "'INSERT INTO app_system.{history} ({column}tenant_id, row_key, kind, operation, \
             changed_by, changed_at, transaction_id, before, after){overriding} VALUES \
             ({value}''{TENANT}'', ''{row_key}'', ''delete'', ''admin:forge-history'', \
             ''{U1}'', now(), 1, ''{{}}'', ''{{}}'')'"
        )
    };
    let direct_writes = [
        entry("users_history", None, &format!(r#"{{"id": "{U2}", "tenant_id": "{TENANT}"}}"#)),
        entry("roles_history", None, &format!(r#"{{"name": "admin", "tenant_id": "{TENANT}"}}"#)),
        entry(
            "user_roles_history",
            None,
            &format!(r#"{{"user_id": "{U1}", "role_name": "admin", "tenant_id": "{TENANT}"}}"#),
        ),
        entry(
            "permissions_history",
            None,
            &format!(
                r#"{{"role_name": "admin", "tenant_id": "{TENANT}", "permission": "receipts:read"}}"#
            ),
        ),
        entry("api_keys_history", None, &format!(r#"{{"id": "{U2}", "tenant_id": "{TENANT}"}}"#)),
        entry(
            "configurations_history",
            Some(-5),
            &format!(r#"{{"tenant_id": "{TENANT}", "config_key": "theme"}}"#),
        ),
    ]
    .join(",\n    ");
    script.push_str(&format!(
        r#"
DO $$
DECLARE history text; operation text;
BEGIN
  FOREACH history IN ARRAY ARRAY['users_history','roles_history','user_roles_history',
                                 'permissions_history','configurations_history',
                                 'api_keys_history'] LOOP
    FOREACH operation IN ARRAY ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE'] LOOP
      ASSERT NOT has_table_privilege('wamn_app'::name, ('app_system.'||history)::text, operation),
        format('wamn_app holds table %s on %s', operation, history);
    END LOOP;
    FOREACH operation IN ARRAY ARRAY['SELECT','UPDATE'] LOOP
      ASSERT NOT has_any_column_privilege('wamn_app'::name, ('app_system.'||history)::text, operation),
        format('wamn_app holds column %s on %s', operation, history);
    END LOOP;
    ASSERT has_any_column_privilege('wamn_app'::name, ('app_system.'||history)::text, 'INSERT')
           = (history = 'configurations_history'),
      format('wamn_app column INSERT on %s does not match its R11 class', history);
  END LOOP;
  ASSERT NOT has_column_privilege('wamn_app'::name, 'app_system.configurations_history'::text,
                                  'position'::text, 'INSERT'::text),
    'wamn_app can choose the position of a configurations entry';
END $$;

BEGIN;
SET LOCAL ROLE {app_generation};

DO $$
DECLARE probe_sql text;
BEGIN
  ASSERT current_user = '{app_generation}',
    'the tenant authority is the prepared App generation';
  FOREACH probe_sql IN ARRAY ARRAY[
    'SELECT count(*) FROM app_system.users_history',
    'SELECT count(*) FROM app_system.roles_history',
    'SELECT count(*) FROM app_system.user_roles_history',
    'SELECT count(*) FROM app_system.permissions_history',
    'SELECT count(*) FROM app_system.configurations_history',
    'SELECT count(*) FROM app_system.api_keys_history',
    {direct_writes},
    'UPDATE app_system.configurations_history SET operation = ''admin:forge-history''',
    'DELETE FROM app_system.configurations_history'
  ] LOOP
    BEGIN
      EXECUTE probe_sql;
      RAISE EXCEPTION 'author SQL reached an app_system history table: %', probe_sql;
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

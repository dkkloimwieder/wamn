//! The live write-authority gate for `deploy/sql/app-schema.sql` (R11).
//!
//! `tests/schema.rs` pins the GRANT lines as TEXT. This suite tests that PostgreSQL
//! actually enforces them: every revoke lands with its positive assertion — this
//! principal, on this relation, denied — rather than as a paper narrowing.
//!
//! One test per adjudicated class (docs/architecture/data-access.md#actors):
//! - `users` / `role` / `user_role` / `permission` are the rows the trust
//!   chain reads as authorization INPUT, `environment` is the status the host
//!   admits a request by, and the platform configuration tables are the rows
//!   the platform writes for the application. An App generation may inherit
//!   their stable `wamn_app` reads and nothing more, so a tenant or an
//!   application cannot create a platform row or a `wamn:` name. `api_key`
//!   takes no grant at all;
//! - `number_counter` is written only through `platform.bump_counter`,
//!   `platform.assign_counter` and `platform.set_counter`, which refuse an id
//!   that is not a sequence — the control proving the other test fails for
//!   the revoked privilege rather than for an over-broad narrowing. Which
//!   application's sequence a caller reaches is decided by the `wamn:postgres`
//!   plugin from host state, and its statement admission keeps guest SQL away
//!   from these functions; the plugin's tests cover both;
//! - the history tables take no read and no direct write from an App
//!   generation.
//!
//! Each test runs as the superuser of a test database on the test PostgreSQL
//! server (the harness prepares one tenant-scoped App generation) and holds the
//! process lock, because it changes cluster-wide roles — the `tests/schema.rs`
//! live-apply convention.

use std::fmt::Write as _;

use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};

/// The tenant every probe runs under, and the user its seeded rows belong to.
const ENV_KEY: &str = "t1";
const U1: &str = "11111111-1111-1111-1111-111111111111";
/// An unused user id for the refused inserts.
const U2: &str = "22222222-2222-2222-2222-222222222222";
/// A number sequence of [`APPLICATION`].
const SEQUENCE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
/// The application that owns [`SEQUENCE`].
const APPLICATION: &str = "receiving";
/// A number sequence of another application.
const OTHER_SEQUENCE: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
/// An id that is not a number sequence.
const UNKNOWN_SEQUENCE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
/// The pinned `wamn:provisioning` id, the row a tenant must not create.
const PROVISIONING: &str = "770df186-ac15-579e-b46b-c297cae2011b";
const APP_GENERATION_PASSWORD: &str = "test-owned-app-generation-password";
const APP_GENERATION_VALID_UNTIL: &str = "2099-01-01T00:00:00Z";

/// `deploy/sql/record-history.sql`, `deploy/sql/record-history-app-grants.sql`,
/// and `deploy/sql/app-schema.sql`, the order every tenant applier uses.
fn app_schema_sql() -> String {
    format!(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_scenario_author') THEN CREATE ROLE wamn_scenario_author NOLOGIN; END IF; END $$;\nDROP SCHEMA IF EXISTS catalog CASCADE;\n{}\n{}",
        wamn_catalog::CATALOG_SCHEMA_SQL,
        include_str!("../../../../deploy/sql/app-schema.sql")
    )
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
/// `platform` applied verbatim from the DDL of record, one tenant's role rows,
/// the user and configuration rows of the database, and a sequence of each of
/// two applications.
/// Seeded as the superuser, so the seed itself is unaffected by the grants under
/// test. The fixture writes as U1, its test principal, whose row stamps itself,
/// under an administrative operation, and both bindings stay for the probes.
/// Returns the generation name for the probes' `current_user`.
fn prelude(url: &str) -> (String, String) {
    let database = current_database(url);
    let app_generation = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::EnvKey {
            env_key: ENV_KEY,
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
    script.push_str("\nDROP SCHEMA IF EXISTS platform CASCADE;\n");
    script.push_str(&app_schema_sql());
    writeln!(
        script,
        r#"
SET app.user_id = '{U1}';
SET app.operation = 'admin:seed-authority-fixture';
INSERT INTO platform."user" (id, type, email) VALUES ('{U1}','user','u1@t1');
INSERT INTO platform.role (name) VALUES ('admin'),('auditor');
INSERT INTO platform.user_role (user_id, role_name) VALUES ('{U1}','admin');
INSERT INTO platform.permission (role_name, permission) VALUES ('auditor','platform-fixture:widget/get');
INSERT INTO platform.api_key (user_id, name, key_hash, prefix) VALUES ('{U1}','ci','hash-1','wk_a');
INSERT INTO platform.setting (scope_type, scope_id, key, value) VALUES ('org','acme','display.theme','"dark"'::jsonb);
INSERT INTO platform.number_sequence (id, application, name, pattern) VALUES
  ('{SEQUENCE}','{APPLICATION}','purchase_order','P-{{seq:po,width=8}}'),
  ('{OTHER_SEQUENCE}','wms','purchase_order','P-{{seq:po,width=8}}');"#
    )
    .expect("writing to a String cannot fail");
    (app_generation, script)
}

const TEARDOWN: &str = "\nDROP SCHEMA platform CASCADE;\n";

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

/// The relations the trust chain resolves `app.user_id` / `app.role` from, the
/// environment status, and the platform configuration tables are readable
/// through `wamn_app` and writable by nobody through it: author SQL that could
/// insert its own `user_role` row would be minting the input its own
/// generated policies are then evaluated against, and author SQL that could
/// write `number_counter` would break gapless numbering. `api_key` is not even
/// readable.
///
/// Each denied statement is RLS-LEGAL for the probe tenant — same generation,
/// FKs satisfied, no key collision — so `42501` here is the revoked privilege and
/// not a `WITH CHECK` rejection, which shares the SQLSTATE. That the tenant floor
/// does admit such a row under the App generation is what the counter test shows.
#[test]
fn author_sql_cannot_write_the_relations_that_authorize_it() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

    let (app_generation, mut script) = prelude(&url);
    writeln!(
        script,
        r#"
DO $$
DECLARE relation text; operation text;
BEGIN
  FOREACH relation IN ARRAY ARRAY['user','role','user_role','permission','environment',
                                  'external_system','integration','connection','number_sequence','number_counter',
                                  'setting'] LOOP
    ASSERT has_table_privilege('wamn_app'::name, ('platform.'||relation)::text, 'SELECT'::text),
      format('%s must stay readable — the platform revokes writes, not reads', relation);
    FOREACH operation IN ARRAY ARRAY['INSERT','UPDATE','DELETE','TRUNCATE'] LOOP
      ASSERT NOT has_table_privilege('wamn_app'::name, ('platform.'||relation)::text, operation),
        format('wamn_app holds %s on %s — author SQL can mint its own authorization', operation, relation);
    END LOOP;
  END LOOP;
  FOREACH operation IN ARRAY ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE'] LOOP
    ASSERT NOT has_table_privilege('wamn_app'::name, 'platform.api_key'::text, operation),
      format('wamn_app holds %s on api_key, which no runtime code reads', operation);
  END LOOP;
END $$;

BEGIN;
SET LOCAL ROLE {app_generation};

DO $$ BEGIN
  ASSERT current_user = '{app_generation}',
    'the tenant authority is the prepared App generation';
  ASSERT (SELECT count(*) FROM platform."user") = 1,
    'users is still readable';
  ASSERT (SELECT count(*) FROM platform.user_role) = 1,
    'the role linkage 4.2 resolves app.role from is still readable';
END $$;

DO $$
DECLARE probe_sql text;
BEGIN
  FOREACH probe_sql IN ARRAY ARRAY[
    'INSERT INTO platform."user" (id, type, email) VALUES (''{U2}'', ''user'', ''intruder@t1'')',
    'INSERT INTO platform."user" (id, type, email, display_name) VALUES (''{PROVISIONING}'', ''platform'', ''provisioning@example.invalid'', ''wamn:provisioning'')',
    'UPDATE platform."user" SET status = ''disabled''',
    'UPDATE platform."user" SET display_name = ''wamn:intruder''',
    'DELETE FROM platform."user"',
    'INSERT INTO platform.role (name) VALUES (''superadmin'')',
    'UPDATE platform.role SET description = ''changed''',
    'DELETE FROM platform.role',
    'INSERT INTO platform.user_role (user_id, role_name) VALUES (''{U1}'', ''auditor'')',
    'UPDATE platform.user_role SET role_name = ''admin''',
    'DELETE FROM platform.user_role',
    'INSERT INTO platform.permission (role_name, permission) VALUES (''auditor'', ''platform-fixture:widget/list'')',
    'UPDATE platform.permission SET permission = ''platform-fixture:widget/list''',
    'DELETE FROM platform.permission',
    'INSERT INTO platform.api_key (user_id, name, key_hash, prefix) VALUES (''{U1}'', ''forged'', ''hash-2'', ''wk_b'')',
    'UPDATE platform.api_key SET revoked_at = now()',
    'DELETE FROM platform.api_key',
    'SELECT count(*) FROM platform.api_key',
    'INSERT INTO platform.number_counter (sequence_id, counter, key_value, value) VALUES (''{SEQUENCE}'', ''po'', '''', 100)',
    'UPDATE platform.number_counter SET value = 1',
    'DELETE FROM platform.number_counter',
    'INSERT INTO platform.number_sequence (application, name, pattern) VALUES (''receiving'', ''forged'', ''F-{{seq:f}}'')',
    'UPDATE platform.number_sequence SET pattern = ''F-{{seq:f}}''',
    'INSERT INTO platform.setting (scope_type, scope_id, key, value) VALUES (''org'', ''acme'', ''locale'', ''true'')',
    'UPDATE platform.setting SET locked = true',
    'INSERT INTO platform.external_system (id,name) VALUES (gen_random_uuid(),''forged'')',
    'INSERT INTO platform.integration (id,integration_type,name,status) VALUES (gen_random_uuid(),''http'',''forged'',''active'')',
    'DELETE FROM platform.connection',
    'INSERT INTO platform.environment (status) VALUES (''active'')',
    'UPDATE platform.environment SET status = ''active''',
    'DELETE FROM platform.environment'
  ] LOOP
    BEGIN
      EXECUTE probe_sql;
      RAISE EXCEPTION 'author SQL mutated a platform-protected relation: %', probe_sql;
    EXCEPTION WHEN insufficient_privilege THEN NULL;
    END;
  END LOOP;
END $$;

ROLLBACK;"#
    )
    .expect("writing to a String cannot fail");
    script.push_str(TEARDOWN);
    run(&url, &script);
}

/// `number_counter` advances only through `platform.bump_counter`
/// (docs/architecture/data-access.md#counters). The App generation holds
/// EXECUTE on it and no write on the table. The function runs as its owner on a
/// sequence id and reads no claim: it returns 1 for a new counter row and then
/// counts up, starts each key at 1, and refuses with `undefined_object` an id
/// that is not a sequence. The `wamn:postgres` plugin resolves the id from the
/// application in host state, and its statement admission refuses guest SQL
/// that names the function. `number_counter` has no history table.
#[test]
fn the_application_advances_a_counter_only_through_bump_counter() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

    let (app_generation, mut script) = prelude(&url);
    writeln!(
        script,
        r"
DO $$ BEGIN
  ASSERT has_function_privilege('wamn_app'::name,
                                'platform.bump_counter(uuid, text, text)'::text, 'EXECUTE'),
    'wamn_app must execute bump_counter';
  ASSERT (SELECT prosecdef FROM pg_catalog.pg_proc
           WHERE oid = 'platform.bump_counter(uuid, text, text)'::regprocedure),
    'bump_counter must be SECURITY DEFINER';
  ASSERT to_regclass('platform.number_counter_history') IS NULL,
    'number_counter has no history table';
END $$;

BEGIN;
SET LOCAL ROLE {app_generation};

DO $$ BEGIN
  ASSERT current_user = '{app_generation}',
    'the probe runs as the prepared App generation';
  ASSERT platform.bump_counter('{SEQUENCE}', 'po', '') = 1, 'a new counter renders 1';
  ASSERT platform.bump_counter('{SEQUENCE}', 'po', '') = 2, 'the next call renders 2';
  ASSERT platform.bump_counter('{SEQUENCE}', 'line', 'batch-a') = 1, 'a new key renders 1';
  ASSERT platform.bump_counter('{SEQUENCE}', 'line', 'batch-b') = 1, 'each key starts at 1';
  ASSERT platform.bump_counter('{SEQUENCE}', 'line', 'batch-a') = 2, 'a key counts on its own';
  ASSERT (SELECT value FROM platform.number_counter
           WHERE counter = 'po' AND key_value = '') = 3,
    'the row holds the next value, readable by the application';
  ASSERT (SELECT count(*) FROM platform.number_counter) = 3,
    'the generation sees the counter rows of the database';
  ASSERT (SELECT count(*) FROM platform.number_sequence) = 2,
    'the generation sees every sequence of the database';
END $$;

DO $$ BEGIN
  BEGIN
    PERFORM platform.bump_counter('{UNKNOWN_SEQUENCE}', 'po', '');
    RAISE EXCEPTION 'bump_counter advanced an id that is not a sequence';
  EXCEPTION WHEN undefined_object THEN NULL;
  END;
END $$;

RESET ROLE;
DO $$ BEGIN
  ASSERT (SELECT count(*) FROM platform.number_counter WHERE sequence_id = '{OTHER_SEQUENCE}') = 0,
    'no call wrote a counter of the other application';
  ASSERT (SELECT created_by FROM platform.number_counter
           WHERE counter = 'po' AND key_value = '') = '{U1}',
    'the counter row stamps the bound actor';
END $$;

ROLLBACK;"
    )
    .expect("writing to a String cannot fail");
    script.push_str(TEARDOWN);
    run(&url, &script);
}

/// The per= key rows of `number_counter` are written only by
/// `platform.assign_counter` (docs/architecture/data-access.md#counters,
/// wamn-83eth.2). The App generation holds EXECUTE on it. A key gets the next
/// value of the `''` allocator once, and the same value after; the `''` key is
/// refused, and an id that is not a sequence is refused with
/// `undefined_object`. The function reads no claim.
#[test]
fn the_application_assigns_a_keyed_counter_only_through_assign_counter() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

    let (app_generation, mut script) = prelude(&url);
    writeln!(
        script,
        r"
DO $$ BEGIN
  ASSERT has_function_privilege('wamn_app'::name,
                                'platform.assign_counter(uuid, text, text)'::text, 'EXECUTE'),
    'wamn_app must execute assign_counter';
  ASSERT (SELECT prosecdef FROM pg_catalog.pg_proc
           WHERE oid = 'platform.assign_counter(uuid, text, text)'::regprocedure),
    'assign_counter must be SECURITY DEFINER';
END $$;

BEGIN;
SET LOCAL ROLE {app_generation};

DO $$ BEGIN
  ASSERT platform.assign_counter('{SEQUENCE}', 'batch', 'batch-a') = 1, 'a new key takes 1';
  ASSERT platform.assign_counter('{SEQUENCE}', 'batch', 'batch-b') = 2, 'the next key takes 2';
  ASSERT platform.assign_counter('{SEQUENCE}', 'batch', 'batch-a') = 1, 'a known key keeps 1';
  ASSERT (SELECT value FROM platform.number_counter
           WHERE counter = 'batch' AND key_value = '') = 3,
    'the allocator advanced once for each key';
  BEGIN
    PERFORM platform.assign_counter('{SEQUENCE}', 'batch', '');
    RAISE EXCEPTION 'assign_counter assigned the allocator key';
  EXCEPTION WHEN invalid_parameter_value THEN NULL;
  END;
  BEGIN
    PERFORM platform.assign_counter('{UNKNOWN_SEQUENCE}', 'batch', 'batch-a');
    RAISE EXCEPTION 'assign_counter assigned a key of an id that is not a sequence';
  EXCEPTION WHEN undefined_object THEN NULL;
  END;
END $$;

RESET ROLE;
DO $$ BEGIN
  ASSERT (SELECT count(*) FROM platform.number_counter WHERE sequence_id = '{OTHER_SEQUENCE}') = 0,
    'no call wrote a counter of the other application';
END $$;

ROLLBACK;"
    )
    .expect("writing to a String cannot fail");
    script.push_str(TEARDOWN);
    run(&url, &script);
}

/// `platform.set_counter` is the third writer of `number_counter`, the write of
/// the set_counter host route (docs/architecture/data-access.md#counters,
/// wamn-83eth.3). The App generation holds EXECUTE on it. It inserts a counter
/// row or sets its value; it refuses a value below 1, and refuses with
/// `undefined_object` an id that is not a sequence. It reads no claim: the
/// route resolves the sequence by the application it checked.
#[test]
fn the_application_sets_a_counter_only_through_set_counter() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

    let (app_generation, mut script) = prelude(&url);
    writeln!(
        script,
        r"
DO $$ BEGIN
  ASSERT has_function_privilege('wamn_app'::name,
                                'platform.set_counter(uuid, text, text, bigint)'::text, 'EXECUTE'),
    'wamn_app must execute set_counter';
  ASSERT (SELECT prosecdef FROM pg_catalog.pg_proc
           WHERE oid = 'platform.set_counter(uuid, text, text, bigint)'::regprocedure),
    'set_counter must be SECURITY DEFINER';
END $$;

BEGIN;
SET LOCAL ROLE {app_generation};

DO $$ BEGIN
  PERFORM platform.set_counter('{SEQUENCE}', 'po', '', 100);
  ASSERT platform.bump_counter('{SEQUENCE}', 'po', '') = 100, 'a new row renders the set value';
  PERFORM platform.set_counter('{SEQUENCE}', 'po', '', 7);
  ASSERT platform.bump_counter('{SEQUENCE}', 'po', '') = 7, 'a set row renders the set value';
  BEGIN
    PERFORM platform.set_counter('{SEQUENCE}', 'po', '', 0);
    RAISE EXCEPTION 'set_counter set a value below 1';
  EXCEPTION WHEN invalid_parameter_value THEN NULL;
  END;
  BEGIN
    PERFORM platform.set_counter('{UNKNOWN_SEQUENCE}', 'po', '', 5);
    RAISE EXCEPTION 'set_counter set a counter of an id that is not a sequence';
  EXCEPTION WHEN undefined_object THEN NULL;
  END;
END $$;

RESET ROLE;
DO $$ BEGIN
  ASSERT (SELECT count(*) FROM platform.number_counter WHERE sequence_id = '{OTHER_SEQUENCE}') = 0,
    'no call wrote a counter of the other application';
END $$;

ROLLBACK;"
    )
    .expect("writing to a String cannot fail");
    script.push_str(TEARDOWN);
    run(&url, &script);
}

/// The history tables of `platform` (epic ruling 76). An App generation
/// reads no history table and writes none, not even an entry column, so it
/// cannot choose a `position` with `OVERRIDING SYSTEM VALUE` either.
///
/// These history tables have no row security, so `42501` here means that
/// the caller lacks the privilege to write an entry.
#[test]
fn author_sql_reads_and_writes_no_history_table() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

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
            "'INSERT INTO platform.{history} ({column}row_key, type, operation, \
             changed_by, changed_at, transaction_id, before, after){overriding} VALUES \
             ({value}''{row_key}'', ''delete'', ''admin:forge-history'', \
             ''{U1}'', now(), 1, ''{{}}'', ''{{}}'')'"
        )
    };
    let direct_writes = [
        entry("user_history", None, &format!(r#"{{"id": "{U2}"}}"#)),
        entry("role_history", None, r#"{"name": "admin"}"#),
        entry(
            "user_role_history",
            None,
            &format!(r#"{{"user_id": "{U1}", "role_name": "admin"}}"#),
        ),
        entry(
            "permission_history",
            None,
            r#"{"role_name": "auditor", "permission": "platform-fixture:widget/get"}"#,
        ),
        entry("api_key_history", None, &format!(r#"{{"id": "{U2}"}}"#)),
        entry("environment_history", None, r#"{"id": true}"#),
        entry(
            "setting_history",
            Some(-5),
            r#"{"key": "display.theme", "scope_id": "acme", "scope_type": "org"}"#,
        ),
        entry(
            "number_sequence_history",
            None,
            &format!(r#"{{"id": "{SEQUENCE}"}}"#),
        ),
    ]
    .join(",\n    ");
    writeln!(
        script,
        r"
DO $$
DECLARE history text; operation text;
BEGIN
  FOREACH history IN ARRAY ARRAY['user_history','role_history','user_role_history',
                                 'permission_history','api_key_history',
                                 'environment_history','number_sequence_history',
                                 'setting_history'] LOOP
    FOREACH operation IN ARRAY ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE'] LOOP
      ASSERT NOT has_table_privilege('wamn_app'::name, ('platform.'||history)::text, operation),
        format('wamn_app holds table %s on %s', operation, history);
    END LOOP;
    FOREACH operation IN ARRAY ARRAY['SELECT','UPDATE'] LOOP
      ASSERT NOT has_any_column_privilege('wamn_app'::name, ('platform.'||history)::text, operation),
        format('wamn_app holds column %s on %s', operation, history);
    END LOOP;
    ASSERT NOT has_any_column_privilege('wamn_app'::name, ('platform.'||history)::text, 'INSERT'),
      format('wamn_app holds column INSERT on %s', history);
  END LOOP;
  ASSERT NOT has_column_privilege('wamn_app'::name, 'platform.setting_history'::text,
                                  'position'::text, 'INSERT'::text),
    'wamn_app can choose the position of a setting entry';
END $$;

BEGIN;
SET LOCAL ROLE {app_generation};

DO $$
DECLARE probe_sql text;
BEGIN
  ASSERT current_user = '{app_generation}',
    'the tenant authority is the prepared App generation';
  FOREACH probe_sql IN ARRAY ARRAY[
    'SELECT count(*) FROM platform.user_history',
    'SELECT count(*) FROM platform.role_history',
    'SELECT count(*) FROM platform.user_role_history',
    'SELECT count(*) FROM platform.permission_history',
    'SELECT count(*) FROM platform.api_key_history',
    'SELECT count(*) FROM platform.environment_history',
    'SELECT count(*) FROM platform.number_sequence_history',
    'SELECT count(*) FROM platform.setting_history',
    {direct_writes},
    'UPDATE platform.setting_history SET operation = ''admin:forge-history''',
    'DELETE FROM platform.setting_history'
  ] LOOP
    BEGIN
      EXECUTE probe_sql;
      RAISE EXCEPTION 'author SQL reached an platform history table: %', probe_sql;
    EXCEPTION WHEN insufficient_privilege THEN NULL;
    END;
  END LOOP;
END $$;

ROLLBACK;"
    )
    .expect("writing to a String cannot fail");
    script.push_str(TEARDOWN);
    run(&url, &script);
}

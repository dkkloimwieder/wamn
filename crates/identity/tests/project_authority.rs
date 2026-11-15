//! The live write-authority condition for `deploy/sql/app-schema.sql` (R11).
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
//!   their stable `wamn_app` reads and nothing more, so a project or an
//!   application cannot create a platform row or a `wamn:` name. `api_key`
//!   takes no grant at all;
//! - the history tables take no read and no direct write from an App
//!   generation.
//!
//! Each test runs as the superuser of a test database on the test PostgreSQL
//! server (the harness prepares one project-targeted App generation) and holds the
//! process lock, because it changes cluster-wide roles — the `tests/schema.rs`
//! live-apply convention.

use std::fmt::Write as _;

use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, workload_login};

/// The project every probe runs under, and the user its seeded rows belong to.
const PROJECT_ENV_KEY: &str = "t1--dev--project";
const U1: &str = "11111111-1111-1111-1111-111111111111";
/// An unused user id for the refused inserts.
const U2: &str = "22222222-2222-2222-2222-222222222222";
/// A number sequence of [`APPLICATION`].
const SEQUENCE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
/// The application that owns [`SEQUENCE`].
const APPLICATION: &str = "receiving";
/// The pinned `wamn:provisioning` id, the row a project must not create.
const PROVISIONING: &str = "770df186-ac15-579e-b46b-c297cae2011b";
const APP_GENERATION_PASSWORD: &str = "test-owned-app-generation-password";

/// `deploy/sql/record-history.sql`, `deploy/sql/record-history-app-grants.sql`,
/// and `deploy/sql/app-schema.sql`, the order every project applier uses.
fn app_schema_sql() -> String {
    format!(
        "\nDROP SCHEMA IF EXISTS catalog CASCADE;\n{}\n{}",
        wamn_catalog::CATALOG_SCHEMA_SQL,
        include_str!("../../../deploy/sql/app-schema.sql")
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

/// The superuser prelude: a production-prepared project-targeted App generation,
/// the stable passwordless `wamn_app` ACL carrier it inherits, a fresh
/// `platform` applied verbatim from the DDL of record, one project's role rows,
/// the user and configuration rows of the database, and its application sequence.
/// Seeded as the superuser, so the seed itself is unaffected by the grants under
/// test. The fixture writes as U1, its test principal, whose row stamps itself,
/// under an administrative operation, and both bindings stay for the probes.
/// Returns the generation name for the probes' `current_user`.
fn prelude(url: &str) -> (String, String) {
    let database = current_database(url);
    let app_generation = workload_login(
        WorkloadRoleFamily::App,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: &PROJECT_ENV_KEY.parse().unwrap(),
            database: &database,
        },
    )
    .expect("App accepts project target");
    let mut script = wamn_control_provision::test_database::login_sql(
        WorkloadRoleFamily::App,
        &database,
        &app_generation,
        APP_GENERATION_PASSWORD,
    );
    script.push_str("\nDROP SCHEMA IF EXISTS platform CASCADE;\n");
    script.push_str(&app_schema_sql());
    writeln!(
        script,
        r#"
INSERT INTO platform.login_principal(login,principal_id) VALUES(current_user,'{U1}'::uuid);
INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES('{U1}'::text::uuid,'','fixture','admin:seed-authority-fixture');
INSERT INTO platform."user" (id, type, email) VALUES ('{U1}','user','u1@t1');
INSERT INTO platform.role (name) VALUES ('admin'),('auditor');
INSERT INTO platform.user_role (user_id, role_name) VALUES ('{U1}','admin');
INSERT INTO platform.permission (role_name, permission) VALUES ('auditor','platform-fixture:widget/get');
INSERT INTO platform.api_key (user_id, name, key_hash, prefix) VALUES ('{U1}','ci','hash-1','wk_a');
INSERT INTO platform.setting (target_type, target_id, key, value) VALUES ('org','acme','display.theme','"dark"'::jsonb);
INSERT INTO platform.number_sequence (id, application, name, pattern) VALUES
  ('{SEQUENCE}','{APPLICATION}','purchase_order','P-{{seq:po,width=8}}');"#
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
/// Each denied statement is RLS-LEGAL for the probe project — same generation,
/// FKs satisfied, no key collision — so `42501` here is the revoked privilege and
/// not a `WITH CHECK` rejection, which shares the SQLSTATE. That the project base grants
/// does admit such a row under the App generation is what the counter test shows.
#[sqlx::test(migrations = false)]
async fn author_sql_cannot_write_the_relations_that_authorize_it(pool: sqlx::PgPool) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let url = test_database.as_str().to_owned();

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
INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES('{U1}'::uuid,'','fixture','admin:authority-fixture');
SET LOCAL ROLE {app_generation};

DO $$ BEGIN
  ASSERT current_user = '{app_generation}',
    'the project authority is the prepared App generation';
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
    'INSERT INTO platform.setting (target_type, target_id, key, value) VALUES (''org'', ''acme'', ''locale'', ''true'')',
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

/// The history tables of `platform` (epic ruling 76). An App generation
/// reads no history table and writes none, not even an entry column, so it
/// cannot choose a `position` with `OVERRIDING SYSTEM VALUE` either.
///
/// These history tables have no row security, so `42501` here means that
/// the caller lacks the privilege to write an entry.
#[sqlx::test(migrations = false)]
async fn author_sql_reads_and_writes_no_history_table(pool: sqlx::PgPool) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let url = test_database.as_str().to_owned();

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
            r#"{"key": "display.theme", "target_id": "acme", "target_type": "org"}"#,
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
INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES('{U1}'::uuid,'','fixture','admin:authority-fixture');
SET LOCAL ROLE {app_generation};

DO $$
DECLARE probe_sql text;
BEGIN
  ASSERT current_user = '{app_generation}',
    'the project authority is the prepared App generation';
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

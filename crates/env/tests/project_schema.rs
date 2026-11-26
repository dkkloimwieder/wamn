//! Database constraints and application principal boundaries.

use std::fmt::Write as _;

use std::path::Path;

use postgres_protocol::escape::{escape_identifier, escape_literal};
use wamn_env::sql;
use wamn_identity_protocol::workload_role::{
    WorkloadRoleFamily, WorkloadRoleIdentity, workload_login,
};

const APP_GENERATION_PASSWORD: &str = "test-owned-app-generation-password";
const OTHER_APP_PASSWORD: &str = "other-test-app-password";

fn deploy_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy")
}

fn app_schema_sql() -> String {
    format!(
        "\nDROP SCHEMA IF EXISTS catalog CASCADE;\n{}\n{}",
        wamn_env::project_setup::CATALOG_SCHEMA_SQL,
        include_str!("../../../deploy/sql/app-schema.sql")
    )
}

/// The platform stamp function that every applier installs before
/// `deploy/sql/app-schema.sql`.
fn record_history_sql() -> String {
    std::fs::read_to_string(deploy_dir().join("sql/record-history.sql"))
        .expect("read deploy/sql/record-history.sql")
}

/// The `wamn_app` record history grants that every project applier installs
/// after `deploy/sql/record-history.sql`.
fn record_history_app_grants_sql() -> String {
    std::fs::read_to_string(deploy_dir().join("sql/record-history-app-grants.sql"))
        .expect("read deploy/sql/record-history-app-grants.sql")
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

fn app_generation(
    database: &str,
    project_env_key: &wamn_registry::ProjectEnvKey,
) -> wamn_role::LoginName {
    workload_login(
        WorkloadRoleFamily::App,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key,
            database,
        },
    )
    .expect("App accepts project target")
}

/// Create an App login as CNPG would, with the grants that provisioning adds.
fn app_login_sql(database: &str, login: &wamn_role::LoginName, password: &str) -> String {
    let family = WorkloadRoleFamily::App;
    let role = escape_identifier(login.as_str());
    format!(
        "{} CREATE ROLE {role} LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD {}; \
         GRANT {} TO {role} WITH ADMIN FALSE, INHERIT TRUE, SET FALSE; {}",
        wamn_env::project_setup::ensure_workload_acl_role_sql(family),
        escape_literal(password),
        escape_identifier(family.acl_role()),
        wamn_env::project_setup::grant_workload_login_sql(family, database, login)
    )
}

/// Begin a fixture write under its principal and operation.
async fn transaction<'connection>(
    connection: &'connection mut sqlx::PgConnection,
    principal: &wamn_identity_protocol::PrincipalId,
    operation: &str,
) -> Result<sqlx::Transaction<'connection, sqlx::Postgres>, sqlx::Error> {
    let mut transaction = sqlx::Connection::begin(connection).await?;
    wamn_postgres::bind_transaction_identity(
        &mut transaction,
        principal,
        "",
        "fixture",
        Some(operation),
        None,
    )
    .await?;
    Ok(transaction)
}

/// Apply `deploy/sql/app-schema.sql` to a test database and assert the live,
/// DB-enforced behavior, as the superuser (the harness prepares project-targeted
/// App generations).
#[tokio::test]
#[ignore = "requires: docker, bin:psql"]
async fn app_schema_applies_and_enforces_isolation_on_postgres() {
    const U1: &str = "11111111-1111-1111-1111-111111111111";
    const U2: &str = "22222222-2222-2222-2222-222222222222";
    const U3: &str = "33333333-3333-3333-3333-333333333333";
    const PROJECT_ENV_KEY_1: &str = "t1--dev--project";
    const PROJECT_ENV_KEY_2: &str = "t2--dev--project";

    let case_server = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let test_database = case_server.create_database("fixture_1").await.unwrap();
    let url = test_database.as_str();

    let database = current_database(url);
    let project_env_key_1_app = app_generation(&database, &PROJECT_ENV_KEY_1.parse().unwrap());

    let other_server = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let other = other_server.create_database("fixture_extra").await.unwrap();
    let other_database = current_database(other.as_str());
    let project_env_key_2_app =
        app_generation(&other_database, &PROJECT_ENV_KEY_2.parse().unwrap());
    let mut other_script =
        app_login_sql(&other_database, &project_env_key_2_app, OTHER_APP_PASSWORD);
    other_script.push_str(wamn_platform_install::sql::revoke_public_connect_base_grants_sql());
    other_script.push_str(&record_history_sql());
    other_script.push_str(&record_history_app_grants_sql());
    other_script.push_str(&app_schema_sql());
    writeln!(other_script, "SET ROLE {project_env_key_2_app}; DO $$ BEGIN
        ASSERT (SELECT count(*) FROM platform.role)=0, 'another environment sees none of the first environment roles';
        END $$; RESET ROLE;").unwrap();
    run(other.as_str(), &other_script);

    // Production preparation hardens the stable ACL carrier to passwordless
    // NOLOGIN/NOINHERIT and makes each project generation the only LOGIN.
    let mut script = app_login_sql(&database, &project_env_key_1_app, APP_GENERATION_PASSWORD);
    script.push('\n');

    script.push_str(
        "\nDROP SCHEMA IF EXISTS platform CASCADE;\n\
         DROP SCHEMA IF EXISTS wamn_sysschema_test CASCADE;\n",
    );
    // The schema itself (deploy/sql/app-schema.sql, applied verbatim as the superuser).
    script.push_str(&record_history_sql());
    script.push_str(&record_history_app_grants_sql());
    script.push_str(&app_schema_sql());
    script.push('\n');
    // Seed one environment with known users, roles, a key and a setting.
    // The fixture writes as U1, its test principal, whose row stamps itself.
    writeln!(
        script,
        "BEGIN; INSERT INTO platform.tx_identity(principal_id,role,app,operation) VALUES('{U1}'::uuid,'','fixture','admin:seed-isolation-fixture');\n\
         INSERT INTO platform.\"user\" (id, issuer, sub, type, email) VALUES \
           ('{U1}','https://fixture.example','{U1}','user','u1@t1'),\n\
           ('{U2}','wamn','{U2}','service','u2@t1'),\n\
           ('{U3}','https://fixture.example','{U3}','user','u3@t2');\n\
         INSERT INTO platform.role (name) VALUES ('admin'),('clerk');\n\
         INSERT INTO platform.user_role (user_id, role_name) VALUES ('{U1}','admin');\n\
         INSERT INTO platform.permission (role_name, permission) VALUES ('clerk','platform-fixture:widget/get');\n\
         INSERT INTO platform.setting (target_type, target_id, key, value) VALUES ('user','{U1}','display.theme','\"dark\"'::jsonb);"
    )
    .expect("writing to a String cannot fail");

    script.push_str("COMMIT;\n");
    // Each generation reads the users, roles and settings of its database.
    writeln!(
        script,
        "BEGIN;\n\
         SET LOCAL ROLE {project_env_key_1_app};\n\
         DO $$ BEGIN\n\
           ASSERT current_user='{project_env_key_1_app}', 'project authority is the prepared project-1 generation';\n\
           ASSERT (SELECT count(*) FROM platform.\"user\" WHERE id IN ('{U1}', '{U2}', '{U3}'))=3, 'the app sees the seeded accounts';\n\
           ASSERT (SELECT count(*) FROM platform.role)=2, 't1 sees its roles';\n\
           ASSERT (SELECT count(*) FROM platform.user_role)=1, 't1 sees its grant';\n\
           ASSERT (SELECT count(*) FROM platform.permission)=1, 't1 sees its permission';\n\
           ASSERT (SELECT count(*) FROM platform.setting)=1, 't1 sees the setting of the database';\n\
         END $$;\n\
         COMMIT;"
    )
    .expect("writing to a String cannot fail");
    // The ACL role reads the roles in its database.
    script.push_str(
        "BEGIN; SET LOCAL ROLE wamn_app; \
         DO $$ BEGIN ASSERT (SELECT count(*) FROM platform.role)=2, 'the ACL carrier reads the database roles'; END $$; \
         COMMIT;",
    );
    // users.id is an org-issued UUID: PostgreSQL must report both the exact type
    // and the absence of any database-minted default.
    script.push_str(
        "DO $$ DECLARE t text; d text; BEGIN\n\
           SELECT pg_catalog.format_type(a.atttypid, a.atttypmod),\n\
                  pg_catalog.pg_get_expr(def.adbin, def.adrelid, true)\n\
             INTO t, d\n\
             FROM pg_catalog.pg_attribute AS a\n\
             JOIN pg_catalog.pg_class AS relation ON relation.oid = a.attrelid\n\
             JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace\n\
             LEFT JOIN pg_catalog.pg_attrdef AS def\n\
               ON def.adrelid = a.attrelid AND def.adnum = a.attnum\n\
            WHERE namespace.nspname='platform' AND relation.relname='user'\n\
              AND a.attname='id' AND a.attnum > 0 AND NOT a.attisdropped;\n\
           ASSERT t='uuid', 'user.id must be uuid (the app.user_id ownership target)';\n\
           ASSERT d IS NULL, 'users.id must have no server default; the org issues it';\n\
         END $$;\n",
    );
    script.push_str("DO $$ BEGIN ASSERT NOT EXISTS (
        SELECT FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
        WHERE n.nspname='platform' AND c.relkind IN ('r','p') AND (
            c.relrowsecurity OR c.relforcerowsecurity
            OR EXISTS (SELECT FROM pg_policy p WHERE p.polrelid=c.oid)
            OR EXISTS (SELECT FROM pg_attribute a WHERE a.attrelid=c.oid AND a.attname='project_env_key' AND NOT a.attisdropped)
        )), 'application tables must use the database boundary'; END $$;\n");
    // The retired project column is refused instead of accepted as input.
    for (table, insert) in [
        ("role", "(project_env_key, name) VALUES ('', 'empty')".to_owned()),
        (
            "user_role",
            format!("(project_env_key, user_id, role_name) VALUES ('', '{U2}', 'admin')"),
        ),
        (
            "permission",
            "(project_env_key, role_name, permission) VALUES ('', 'clerk', 'platform-fixture:widget/get')"
                .to_owned(),
        ),
    ] {
        writeln!(
            script,
            "DO $$ BEGIN BEGIN\n\
               INSERT INTO platform.{table} {insert};\n\
               ASSERT false, 'a ''''-project {table} row must be rejected (a45)';\n\
             EXCEPTION WHEN undefined_column THEN NULL; END; END $$;"
        )
        .expect("writing to a String cannot fail");
    }
    // A disabled user row still identifies the actor in existing stamps.
    writeln!(
        script,
        "UPDATE platform.\"user\" SET active = false WHERE id = '{U1}';\n\
         DO $$ BEGIN\n\
           ASSERT EXISTS (SELECT FROM platform.\"user\" WHERE id = '{U1}' AND NOT active);\n\
         END $$;"
    )
    .expect("writing to a String cannot fail");
    // Each delete has exactly one dependent row, so each FK is tested independently.
    writeln!(
        script,
        "DO $$ BEGIN
           BEGIN
             DELETE FROM platform.role WHERE name = 'clerk';
             ASSERT false, 'a role with a permission cannot be deleted';
           EXCEPTION WHEN foreign_key_violation THEN NULL; END;
           BEGIN
             DELETE FROM platform.role WHERE name = 'admin';
             ASSERT false, 'an assigned role cannot be deleted';
           EXCEPTION WHEN foreign_key_violation THEN NULL; END;
           BEGIN
             DELETE FROM platform.\"user\" WHERE id = '{U1}';
             ASSERT false, 'a user with a role assignment cannot be deleted';
           EXCEPTION WHEN foreign_key_violation THEN NULL; END;
         END $$;"
    )
    .expect("writing to a String cannot fail");

    script.push_str("DROP SCHEMA platform CASCADE;\n");

    run(url, &script);

    let mut wrong_credential = test_database;
    wrong_credential
        .set_username(project_env_key_2_app.as_str())
        .unwrap();
    wrong_credential
        .set_password(Some(OTHER_APP_PASSWORD))
        .unwrap();
    let error = wamn_postgres::connect(wrong_credential.as_str())
        .await
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<sqlx::Error>()
            .and_then(sqlx::Error::as_database_error)
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("28P01")
    );
}

/// Run `script` through `psql` and return its unaligned output, failing the
/// test with its stderr if any statement or `ASSERT` does.
fn run(url: &str, script: &str) -> String {
    use std::io::Write;
    use std::process::{Command as Proc, Stdio};
    let mut child = Proc::new("psql")
        .arg(url)
        .args(["-X", "-v", "ON_ERROR_STOP=1", "-q", "-At", "-f", "-"])
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
    String::from_utf8(out.stdout).expect("psql output is utf-8")
}

/// Project history covers the platform and catalog while PAT images stay private.
#[tokio::test]
#[ignore = "requires: docker, bin:psql"]
async fn project_history_covers_platform_and_catalog_writes() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = wamn_test_infrastructure::identity::project(&postgres, "platform_history").await;
    run(
        database.as_str(),
        r#"
DO $$
DECLARE
  source record;
  logs_rows boolean;
BEGIN
  FOR source IN
    SELECT schemaname, tablename FROM pg_tables
    WHERE schemaname IN ('platform', 'catalog')
      AND tablename::text !~ '_history$'
  LOOP
    logs_rows := source.schemaname = 'catalog' OR source.tablename NOT IN (
      'credential', 'oauth_token', 'oauth_authorization', 'number_counter',
      'write_log', 'tx_identity', 'login_principal'
    );
    ASSERT logs_rows = EXISTS (
      SELECT FROM pg_trigger
      WHERE tgrelid = to_regclass(format('%I.%I', source.schemaname, source.tablename))
        AND tgfoid = 'wamn_history.log_row_change()'::regprocedure
    ), format('%I.%I', source.schemaname, source.tablename);
  END LOOP;
  ASSERT NOT has_table_privilege('wamn_query', 'platform.pat_history', 'SELECT');
END $$;

BEGIN;
SET LOCAL ROLE wamn_platform;
INSERT INTO platform.tx_identity (principal_id, role, app, operation)
VALUES ('11111111-1111-1111-1111-111111111111', '', '', 'admin:pat-history-fixture');
INSERT INTO platform."user" (id, issuer, sub, type, email)
VALUES ('11111111-1111-1111-1111-111111111111', 'wamn', 'fixture', 'service', 'fixture@example.invalid');
INSERT INTO platform.pat (user_id, prefix, hash, label, expires_at)
VALUES ('11111111-1111-1111-1111-111111111111', '0123456789abcdef',
        'sha256:' || repeat('0', 64), 'fixture', now() + interval '1 day');
COMMIT;

DO $$ BEGIN
  ASSERT EXISTS (
    SELECT FROM platform.pat p JOIN platform.pat_history h ON h.row_key = jsonb_build_object('id', p.id)
    WHERE p.prefix = '0123456789abcdef' AND h.type = 'insert'
      AND h.changed_by = p.user_id AND h.operation = 'admin:pat-history-fixture'
  );
END $$;
"#,
    );
}

/// Row changes record the actor and operation, including key changes and revocations.
#[tokio::test]
#[ignore = "requires: docker, bin:psql"]
async fn platform_relations_log_every_row_change_on_postgres() {
    const U1: &str = "11111111-1111-1111-1111-111111111111";
    const U2: &str = "22222222-2222-2222-2222-222222222222";
    const U3: &str = "33333333-3333-3333-3333-333333333333";
    const SEED: &str = "admin:seed-history-fixture";
    const CHANGE: &str = "admin:change-history-fixture";
    const REMOVE: &str = "admin:remove-history-fixture";

    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let test_database = postgres.create_database("history").await.unwrap();
    let url = test_database.as_str();
    let mut script = sql::ensure_app_acl_role_sql();
    script.push_str("\nDROP SCHEMA IF EXISTS platform CASCADE;\n");
    script.push_str(&record_history_sql());
    script.push_str(&record_history_app_grants_sql());
    script.push_str(&app_schema_sql());
    writeln!(
        script,
        "BEGIN;\n\
         INSERT INTO platform.tx_identity(principal_id,role,app,operation) VALUES('{U1}'::uuid,'','fixture','{SEED}');\n\
         INSERT INTO platform.\"user\" (id, issuer, sub, type, email) VALUES \
           ('{U1}','https://fixture.example','{U1}','user','u1@t1'),\n\
           ('{U2}','https://fixture.example','{U2}','user','u2@t2'),\n\
           ('{U3}','wamn','{U3}','service','u3@t1');\n\
         INSERT INTO platform.role (name) VALUES ('admin'), ('auditor');\n\
         INSERT INTO platform.user_role (user_id, role_name) VALUES \
           ('{U1}', 'admin'), ('{U3}', 'admin');\n\
         INSERT INTO platform.permission (role_name, permission) VALUES \
           ('auditor', 'platform-fixture:widget/get');\n\
         INSERT INTO platform.setting (target_type, target_id, key, value) VALUES \
           ('org', 'acme', 'display.theme', '\"dark\"');\n\
         COMMIT;\n\
         BEGIN;\n\
         INSERT INTO platform.tx_identity(principal_id,role,app,operation) VALUES('{U2}'::uuid,'','fixture','{CHANGE}');\n\
         UPDATE platform.\"user\" SET active = false WHERE id = '{U2}';\n\
         UPDATE platform.role SET description = 'administrators' WHERE name = 'admin';\n\
         UPDATE platform.user_role SET role_name = 'auditor' WHERE user_id = '{U3}';\n\
         UPDATE platform.permission SET permission = 'platform-fixture:widget/list';\n\
         UPDATE platform.setting SET value = '\"light\"';\n\
         COMMIT;\n\
         BEGIN;\n\
         INSERT INTO platform.tx_identity(principal_id,role,app,operation) VALUES('{U3}'::uuid,'','fixture','{REMOVE}');\n\
         DELETE FROM platform.setting;\n\
         UPDATE platform.\"user\" SET active = false WHERE id = '{U1}';\n\
         UPDATE platform.role SET active = false WHERE name = 'auditor';\n\
         DELETE FROM platform.user_role WHERE user_id = '{U1}' OR role_name = 'auditor';\n\
         DELETE FROM platform.permission WHERE role_name = 'auditor';\n\
         COMMIT;"
    )
    .expect("writing to a String cannot fail");
    // Each fixture mutation writes one entry. A key change writes a delete
    // and an insert instead of an update.
    for (table, inserted, updated, removed) in [
        ("user", 3, 1, 0),
        ("role", 2, 1, 0),
        ("user_role", 2, 0, 2),
        ("permission", 1, 0, 1),
        ("setting", 1, 1, 1),
    ] {
        let key_change = updated == 0;
        let deactivated = u32::from(matches!(table, "user" | "role"));
        let changed = if key_change { 2 } else { updated + deactivated };
        writeln!(script, "DO $$ BEGIN
            ASSERT (SELECT count(*) FROM platform.{table}_history) = {inserted} + {changed} + {removed};
            ASSERT (SELECT count(*) FROM platform.{table}_history WHERE type = 'insert'
                AND operation = '{SEED}' AND changed_by = '{U1}') = {inserted};
            ASSERT (SELECT count(*) FROM platform.{table}_history WHERE type = 'update'
                AND operation = '{CHANGE}' AND changed_by = '{U2}') = {updated};
            ASSERT (SELECT count(*) FROM platform.{table}_history WHERE type = 'delete'
                AND operation = '{REMOVE}' AND changed_by = '{U3}') = {removed};
            END $$;").unwrap();
        if key_change {
            for entry_type in ["delete", "insert"] {
                writeln!(
                    script,
                    "DO $$ BEGIN ASSERT (SELECT count(*) FROM platform.{table}_history
                    WHERE type = '{entry_type}' AND operation = '{CHANGE}'
                        AND changed_by = '{U2}') = 1; END $$;"
                )
                .unwrap();
            }
        }
    }
    writeln!(script, "DO $$ BEGIN
        ASSERT EXISTS (SELECT FROM platform.user_history WHERE type = 'update'
            AND row_key ->> 'id' = '{U2}' AND before -> 'active' = 'true'::jsonb AND after -> 'active' = 'false'::jsonb);
        ASSERT EXISTS (SELECT FROM platform.user_history WHERE type = 'update'
            AND operation = '{REMOVE}' AND changed_by = '{U3}'
            AND row_key ->> 'id' = '{U1}' AND before -> 'active' = 'true'::jsonb AND after -> 'active' = 'false'::jsonb);
        ASSERT EXISTS (SELECT FROM platform.role_history WHERE type = 'update'
            AND row_key ->> 'name' = 'admin' AND after ->> 'description' = 'administrators');
        ASSERT EXISTS (SELECT FROM platform.role_history WHERE type = 'update'
            AND operation = '{REMOVE}' AND changed_by = '{U3}'
            AND row_key ->> 'name' = 'auditor' AND before -> 'active' = 'true'::jsonb AND after -> 'active' = 'false'::jsonb);
        ASSERT EXISTS (SELECT FROM platform.setting_history WHERE type = 'update'
            AND before ->> 'value' = 'dark' AND after ->> 'value' = 'light');
        ASSERT EXISTS (SELECT FROM platform.user_role_history WHERE type = 'delete'
            AND operation = '{CHANGE}' AND row_key ->> 'user_id' = '{U3}' AND row_key ->> 'role_name' = 'admin');
        ASSERT EXISTS (SELECT FROM platform.user_role_history WHERE type = 'insert'
            AND operation = '{CHANGE}' AND row_key ->> 'user_id' = '{U3}' AND row_key ->> 'role_name' = 'auditor');
        ASSERT EXISTS (SELECT FROM platform.permission_history WHERE type = 'delete'
            AND operation = '{CHANGE}' AND row_key ->> 'permission' = 'platform-fixture:widget/get');
        ASSERT EXISTS (SELECT FROM platform.permission_history WHERE type = 'insert'
            AND operation = '{CHANGE}' AND row_key ->> 'permission' = 'platform-fixture:widget/list');
        ASSERT EXISTS (SELECT FROM platform.user_role_history WHERE type = 'delete'
            AND operation = '{REMOVE}' AND row_key ->> 'user_id' = '{U1}');
        ASSERT EXISTS (SELECT FROM platform.user_role_history WHERE type = 'delete'
            AND operation = '{REMOVE}' AND row_key ->> 'user_id' = '{U3}' AND row_key ->> 'role_name' = 'auditor');
        ASSERT EXISTS (SELECT FROM platform.permission_history WHERE type = 'delete'
            AND operation = '{REMOVE}' AND row_key ->> 'permission' = 'platform-fixture:widget/list');
        ASSERT NOT EXISTS (SELECT FROM platform.user_role);
        ASSERT NOT EXISTS (SELECT FROM platform.permission);
        END $$;").unwrap();
    run(url, &script);
}

#[tokio::test]
#[ignore = "requires: docker, bin:psql"]
async fn row_image_spells_timestamptz_as_the_platform_canonicalizer_on_postgres() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let url = postgres
        .create_database("history")
        .await
        .unwrap()
        .to_string();

    let mut script = sql::ensure_app_acl_role_sql();
    script.push('\n');
    script.push_str(&record_history_sql());
    // One line per timestamptz value: the microseconds since the epoch that
    // PostgreSQL holds, then the spelling in the row image.
    script.push_str(
        "\nSET TimeZone = 'America/St_Johns';\n\
         CREATE TEMPORARY TABLE spelled (id integer PRIMARY KEY, at timestamptz NOT NULL);\n\
         INSERT INTO spelled VALUES \
             (1, '2026-10-01T09:07:00+02:00'), \
             (2, '2026-10-01T09:07:00.25Z'), \
             (3, '2026-10-01T09:07:00.123456-03:30'), \
             (4, '1969-12-31T23:59:59.999999Z');\n\
         SELECT (extract(epoch FROM at) * 1000000)::bigint || '|' \
                || (wamn_history.row_image(spelled) ->> 'at') \
           FROM spelled ORDER BY id;\n",
    );

    let output = run(&url, &script);
    let lines = output.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 4, "every value must render: {output}");
    for line in lines {
        let (micros, spelled) = line.split_once('|').expect("microseconds and spelling");
        let held = chrono::DateTime::from_timestamp_micros(micros.parse().expect("bigint"))
            .expect("the value is in range");
        assert_eq!(
            spelled,
            wamn_scalar::format_timestamptz(held),
            "the row image must spell the held value as the canonicalizer does"
        );
    }
}

/// `wamn_history.timestamptz_image` spells a `timestamptz` value exactly as the
/// platform canonicalizer spells it, and exactly as `wamn_history.row_image`
/// spells the same column. A history read builds its current row image with
/// this function. The session time zone is not UTC, and the values include
/// zero and non-zero microseconds and infinity.
#[tokio::test]
#[ignore = "requires: docker, bin:psql"]
async fn timestamptz_image_spells_timestamptz_as_the_platform_canonicalizer_on_postgres() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let url = postgres
        .create_database("history")
        .await
        .unwrap()
        .to_string();

    let mut script = sql::ensure_app_acl_role_sql();
    script.push('\n');
    script.push_str(&record_history_sql());
    // One line per value: the microseconds since the epoch that PostgreSQL
    // holds, the spelling of the value image, and whether the row image
    // spells the same JSONB value.
    script.push_str(
        "\nSET TimeZone = 'America/St_Johns';\n\
         CREATE TEMPORARY TABLE spelled (id integer PRIMARY KEY, at timestamptz NOT NULL);\n\
         INSERT INTO spelled VALUES \
             (1, '2026-10-01T09:07:00+02:00'), \
             (2, '2026-10-01T09:07:00.25Z'), \
             (3, '2026-10-01T09:07:00.123456-03:30'), \
             (4, '1969-12-31T23:59:59.999999Z'), \
             (5, 'infinity');\n\
         SELECT CASE WHEN isfinite(at) THEN (extract(epoch FROM at) * 1000000)::bigint::text \
                     ELSE 'infinite' END \
                || '|' || (wamn_history.timestamptz_image(at) #>> '{}') \
                || '|' || (wamn_history.timestamptz_image(at) \
                           = wamn_history.row_image(spelled) -> 'at')::text \
           FROM spelled ORDER BY id;\n",
    );

    let output = run(&url, &script);
    let lines = output.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 5, "every value must render: {output}");
    for line in lines {
        let mut parts = line.split('|');
        let (Some(micros), Some(spelled), Some(same), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            panic!("microseconds, spelling, and row image agreement: {line}");
        };
        assert_eq!(
            same, "true",
            "the value image must equal the row image: {line}"
        );
        if micros == "infinite" {
            continue;
        }
        let held = chrono::DateTime::from_timestamp_micros(micros.parse().expect("bigint"))
            .expect("the value is in range");
        assert_eq!(
            spelled,
            wamn_scalar::format_timestamptz(held),
            "the value image must spell the held value as the canonicalizer does"
        );
    }
}

const HISTORY_PROBE: &str = r"
CREATE TABLE public.history_probe (
    id integer PRIMARY KEY,
    amount numeric NOT NULL,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL
);
SELECT wamn_history.create_history_table('public', 'history_probe', false);
CREATE TRIGGER history_stamp BEFORE INSERT OR UPDATE ON public.history_probe
    FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row(
        'created_at', 'created_by', 'updated_at', 'updated_by');
CREATE TRIGGER history_log AFTER INSERT OR UPDATE OR DELETE ON public.history_probe
    FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('unlimited');
";

const HISTORY_ACTOR: &str = "00000000-0000-4000-8000-00000000000a";
const OTHER_HISTORY_ACTOR: &str = "00000000-0000-4000-8000-00000000000b";

fn install_history_probe(url: &str) {
    let mut script = sql::ensure_app_acl_role_sql();
    script.push_str(wamn_env::project_setup::CATALOG_SCHEMA_SQL);
    script.push_str(HISTORY_PROBE);
    run(url, &script);
}

#[tokio::test]
#[ignore = "requires: docker, bin:psql"]
async fn app_history_keeps_bound_actor_and_records_scale_changes() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = postgres
        .create_database("application_history")
        .await
        .unwrap();
    let url = database.as_str();
    install_history_probe(url);

    run(
        url,
        &format!(
            r#"
GRANT SELECT, INSERT, UPDATE, DELETE ON public.history_probe TO wamn_app;
GRANT INSERT ON public.history_probe_history TO wamn_app;

BEGIN;
SET LOCAL ROLE wamn_app;
INSERT INTO platform.tx_identity (principal_id, role, app, operation)
VALUES ('{HISTORY_ACTOR}', '', 'fixture', 'fixture:probe/create@1.0.0');
SELECT set_config('app.user_id', '{OTHER_HISTORY_ACTOR}', true);
INSERT INTO public.history_probe (id, amount, created_by, updated_by, created_at, updated_at)
VALUES (1, 1.0, '{OTHER_HISTORY_ACTOR}', '{OTHER_HISTORY_ACTOR}', '2000-01-01', '2000-01-01');
INSERT INTO public.history_probe (id, amount) VALUES (2, 2.0);
DO $$ BEGIN
    ASSERT (SELECT count(*) FROM public.history_probe WHERE created_by = '{HISTORY_ACTOR}'
            AND updated_by = '{HISTORY_ACTOR}' AND created_at = transaction_timestamp()
            AND updated_at = transaction_timestamp()) = 2;
END $$;
COMMIT;

BEGIN;
SET LOCAL ROLE wamn_app;
INSERT INTO platform.tx_identity (principal_id, role, app, operation)
VALUES ('{OTHER_HISTORY_ACTOR}', '', 'fixture', 'fixture:probe/update@1.0.0');
INSERT INTO public.history_probe (id, amount) VALUES (1, 1.00)
ON CONFLICT (id) DO UPDATE
SET amount = EXCLUDED.amount, created_at = EXCLUDED.created_at, created_by = EXCLUDED.created_by;
DO $$ BEGIN
    ASSERT EXISTS (SELECT FROM public.history_probe WHERE id = 1 AND created_by = '{HISTORY_ACTOR}'
                   AND created_at < transaction_timestamp() AND updated_by = '{OTHER_HISTORY_ACTOR}'
                   AND updated_at = transaction_timestamp());
    ASSERT EXISTS (SELECT FROM public.history_probe WHERE id = 2 AND updated_by = '{HISTORY_ACTOR}');
END $$;
COMMIT;

BEGIN;
SET LOCAL ROLE wamn_app;
INSERT INTO platform.tx_identity (principal_id, role, app, operation)
VALUES ('{HISTORY_ACTOR}', '', 'fixture', 'fixture:probe/update@1.0.0');
UPDATE public.history_probe SET amount = amount, updated_at = '2000-01-01',
    updated_by = '{HISTORY_ACTOR}' WHERE id = 1;
COMMIT;

DO $$ BEGIN
    ASSERT EXISTS (SELECT FROM public.history_probe_history AS h JOIN public.history_probe AS p ON p.id = 1
                   WHERE h.row_key = '{{"id":1}}' AND h.type = 'insert'
                     AND h.changed_by = '{HISTORY_ACTOR}' AND h.changed_at = p.created_at
                     AND h.before = '{{}}' AND (h.after ->> 'amount') = '1.0');
    ASSERT EXISTS (SELECT FROM public.history_probe_history AS h JOIN public.history_probe AS p ON p.id = 1
                   WHERE h.row_key = '{{"id":1}}' AND h.type = 'update'
                     AND h.changed_by = '{OTHER_HISTORY_ACTOR}' AND h.changed_at = p.updated_at
                     AND p.updated_by = h.changed_by AND (h.before ->> 'amount') = '1.0'
                     AND (h.after ->> 'amount') = '1.00');
    ASSERT (SELECT count(*) FROM public.history_probe_history WHERE row_key = '{{"id":1}}') = 2;
END $$;

BEGIN;
SET LOCAL ROLE wamn_app;
INSERT INTO platform.tx_identity (principal_id, role, app, operation)
VALUES ('{HISTORY_ACTOR}', '', 'fixture', 'fixture:probe/delete@1.0.0');
DELETE FROM public.history_probe WHERE id = 1;
RESET ROLE;
DO $$ BEGIN
    ASSERT EXISTS (SELECT FROM public.history_probe_history
                   WHERE row_key = '{{"id":1}}' AND type = 'delete'
                     AND changed_by = '{HISTORY_ACTOR}' AND changed_at = transaction_timestamp()
                     AND transaction_id = pg_current_xact_id()::text::bigint
                     AND operation = 'fixture:probe/delete@1.0.0' AND after = '{{}}'
                     AND before ->> 'amount' = '1.00' AND before ->> 'id' = '1'
                     AND before ->> 'created_by' = '{HISTORY_ACTOR}');
END $$;
COMMIT;
"#
        ),
    );
}

#[tokio::test]
#[ignore = "requires: docker, bin:psql"]
async fn history_refuses_unattributed_or_invalid_writes_atomically() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = postgres.create_database("history_refusal").await.unwrap();
    let url = database.as_str();
    install_history_probe(url);

    run(
        url,
        &format!(
            r"
CREATE TABLE public.keyless_history_probe (id integer);
SELECT wamn_history.create_history_table('public', 'keyless_history_probe', false);
CREATE TRIGGER history_log AFTER INSERT ON public.keyless_history_probe
    FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('unlimited');
GRANT SELECT, INSERT, UPDATE, DELETE ON public.history_probe TO wamn_app;
GRANT INSERT ON public.history_probe_history TO wamn_app;

INSERT INTO public.history_probe (id, amount) VALUES (1, 1);

BEGIN;
SET LOCAL ROLE wamn_app;
DO $$ BEGIN
    BEGIN
        INSERT INTO public.history_probe (id, amount) VALUES (2, 2);
        ASSERT false, 'insert without an actor';
    EXCEPTION WHEN SQLSTATE '55000' THEN NULL;
    END;
    BEGIN
        UPDATE public.history_probe SET amount = amount WHERE id = 1;
        ASSERT false, 'no-op without an actor';
    EXCEPTION WHEN SQLSTATE '55000' THEN NULL;
    END;
END $$;
INSERT INTO platform.tx_identity (principal_id, role, app)
VALUES ('{HISTORY_ACTOR}', '', 'fixture');
DO $$ BEGIN
    BEGIN
        DELETE FROM public.history_probe WHERE id = 1;
        ASSERT false, 'delete without an operation';
    EXCEPTION WHEN SQLSTATE '55000' THEN NULL;
    END;
END $$;
COMMIT;

DO $$
DECLARE
    proposed text;
BEGIN
    BEGIN
        INSERT INTO public.keyless_history_probe VALUES (1);
        ASSERT false, 'logged write without a primary key';
    EXCEPTION WHEN SQLSTATE '55000' THEN NULL;
    END;

    FOREACH proposed IN ARRAY ARRAY[
        'fixture:probe/update', 'fixture:probe/update@', 'Fixture:probe/update@1',
        'fixture_probe:probe/update@1', 'fixture--probe:probe/update@1',
        'fixture-:probe/update@1', '1fixture:probe/update@1',
        'fixture:probe/update@1/extra', 'fixture:probe/update@1@2',
        'fixture:probe/update@ 1', 'fixture:probe/update@1' || chr(160),
        'wamn:', 'wamn:Audit-retention', 'wamn:audit_retention', 'wamn:audit-retention/run',
        'admin:', 'admin:fix--grades', 'admin:fix-grades ', ' admin:fix-grades',
        'operator:fix-grades'
    ] LOOP
        BEGIN
            INSERT INTO platform.tx_identity (principal_id, role, app, operation)
            VALUES ('{HISTORY_ACTOR}', '', 'fixture', proposed);
            UPDATE public.history_probe SET amount = 2 WHERE id = 1;
            ASSERT false, 'invalid operation %', proposed;
        EXCEPTION WHEN check_violation THEN NULL;
        END;
    END LOOP;

    ASSERT EXISTS (SELECT FROM public.history_probe WHERE id = 1 AND amount = 1);
    ASSERT (SELECT count(*) FROM public.history_probe) = 1;
    ASSERT (SELECT count(*) FROM public.history_probe_history) = 1;
    ASSERT NOT EXISTS (SELECT FROM public.keyless_history_probe);
    ASSERT NOT EXISTS (SELECT FROM public.keyless_history_probe_history);
END $$;
"
        ),
    );
}

#[tokio::test]
#[ignore = "requires: docker, bin:psql"]
async fn history_tables_keep_the_entry_shape_and_reject_truncated_names() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = postgres.create_database("history_shape").await.unwrap();
    let url = database.as_str();
    install_history_probe(url);

    run(
        url,
        r"
SELECT wamn_history.create_history_table('public', 'history_probe', false);
SELECT wamn_history.create_history_table('public', repeat('r', 31), false);

DO $$ BEGIN
    ASSERT EXISTS (
        SELECT FROM pg_index AS i
        WHERE i.indrelid = 'public.history_probe_history'::regclass AND i.indisprimary
          AND ARRAY(SELECT a.attname FROM unnest(i.indkey) WITH ORDINALITY AS key(attnum, ord)
                    JOIN pg_attribute AS a ON a.attrelid = i.indrelid AND a.attnum = key.attnum
                    ORDER BY key.ord) = ARRAY['row_key', 'position']::name[]);
    ASSERT EXISTS (
        SELECT FROM pg_attribute
        WHERE attrelid = 'public.history_probe_history'::regclass AND attname = 'position'
          AND atttypid = 'bigint'::regtype AND attidentity = 'a');
    ASSERT NOT EXISTS (
        SELECT FROM (VALUES
            ('position', 'bigint'::regtype), ('id', 'uuid'::regtype),
            ('row_key', 'jsonb'::regtype), ('type', 'text'::regtype), ('operation', 'text'::regtype),
            ('changed_by', 'uuid'::regtype), ('changed_at', 'timestamptz'::regtype),
            ('transaction_id', 'bigint'::regtype), ('before', 'jsonb'::regtype), ('after', 'jsonb'::regtype)
        ) AS entry(column_name, column_type)
        WHERE NOT EXISTS (
            SELECT FROM pg_attribute
            WHERE attrelid = 'public.history_probe_history'::regclass AND attnum > 0
              AND attname = entry.column_name AND atttypid = entry.column_type AND attnotnull));
    ASSERT NOT EXISTS (
        SELECT FROM pg_class
        WHERE oid IN ('public.history_probe_history'::regclass,
                      pg_get_serial_sequence('public.history_probe_history', 'position')::regclass)
          AND relowner <> (SELECT datdba FROM pg_database WHERE datname = current_database()));
    ASSERT EXISTS (
        SELECT FROM pg_constraint
        WHERE conrelid = ('public.' || repeat('r', 31) || '_history')::regclass
          AND conname = repeat('r', 31) || '_history_transaction_id_not_null');
    BEGIN
        PERFORM wamn_history.create_history_table('public', repeat('r', 32), false);
        ASSERT false, 'overlong derived identifier';
    EXCEPTION WHEN invalid_parameter_value THEN NULL;
    END;
    BEGIN
        PERFORM wamn_history.create_history_table('public', repeat('é', 16), false);
        ASSERT false, 'overlong multibyte derived identifier';
    EXCEPTION WHEN invalid_parameter_value THEN NULL;
    END;
    BEGIN
        INSERT INTO public.history_probe_history
            (row_key, type, operation, changed_by, changed_at, transaction_id, before, after)
        VALUES ('{}', 'upsert', 'admin:probe', gen_random_uuid(), now(), 1, '{}', '{}');
        ASSERT false, 'invalid history entry type';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
END $$;
",
    );
}

#[tokio::test]
#[ignore = "requires: docker, bin:psql"]
async fn run_queue_stamps_scheduled_runs() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = postgres.create_database("run_queue_history").await.unwrap();
    let script = format!(
        r"
{}
{}
{}
{}
BEGIN;
{}
INSERT INTO wamn_run.run
    (run_id, graph_id, graph_version, graph_hash, package_id, env,
     trigger_source, execution_principal_id, execution_role, invoked_by)
VALUES ('run', 'fixture', 1, 'sha256:' || repeat('d', 64), 'package', 'dev',
        'schedule', '{HISTORY_ACTOR}', 'admin', 'schedule');
INSERT INTO wamn_run.run_queue (run_id) VALUES ('run');
DO $$ BEGIN
    ASSERT EXISTS (SELECT FROM wamn_run.run_queue WHERE run_id = 'run'
                   AND created_by = '{}' AND updated_by = created_by
                   AND created_at = transaction_timestamp() AND updated_at = created_at);
END $$;
COMMIT;
",
        wamn_env::project_setup::ensure_project_roles_sql(),
        wamn_env::project_setup::CATALOG_SCHEMA_SQL,
        include_str!("../../../deploy/sql/run-state.sql"),
        include_str!("../../../deploy/sql/run-queue.sql"),
        wamn_postgres::bind_platform_principal_sql(wamn_principal::PlatformComponent::Provisioning),
        wamn_principal::PlatformComponent::Provisioning.principal_id(),
    );
    run(database.as_str(), &script);
}

#[tokio::test]
#[ignore = "requires: docker, bin:psql"]
async fn concurrent_history_uses_the_last_committed_row_image() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = postgres
        .create_database("concurrent_history")
        .await
        .unwrap();
    let url = database.as_str();
    install_history_probe(url);
    run(
        url,
        "INSERT INTO public.history_probe (id, amount) VALUES (1, 1), (2, 1), (3, 1), (4, 1);",
    );

    let mut first = wamn_postgres::connect(url).await.unwrap();
    let mut second = wamn_postgres::connect(url).await.unwrap();
    let mut observer = wamn_postgres::connect(url).await.unwrap();
    let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut second)
        .await
        .unwrap();
    let first_actor = HISTORY_ACTOR.parse().unwrap();
    let second_actor = OTHER_HISTORY_ACTOR.parse().unwrap();
    let advance = "UPDATE public.history_probe SET amount = amount + 1 WHERE id = $1";

    for (id, commit_first) in [(1, true), (2, false)] {
        let mut earlier = transaction(&mut first, &first_actor, "admin:advance-history")
            .await
            .unwrap();
        sqlx::query(advance)
            .bind(id)
            .execute(&mut *earlier)
            .await
            .unwrap();
        let mut later = transaction(&mut second, &second_actor, "admin:advance-history")
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let release = async {
                loop {
                    let blocked: bool =
                        sqlx::query_scalar("SELECT cardinality(pg_blocking_pids($1)) > 0")
                            .bind(second_pid)
                            .fetch_one(&mut observer)
                            .await
                            .unwrap();
                    if blocked {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                if commit_first {
                    earlier.commit().await.unwrap();
                } else {
                    earlier.rollback().await.unwrap();
                }
            };
            let update = async {
                sqlx::query(advance)
                    .bind(id)
                    .execute(&mut *later)
                    .await
                    .unwrap();
                later.commit().await.unwrap();
            };
            tokio::join!(release, update);
        })
        .await
        .expect("competing writers finish");
    }

    let mut earlier = transaction(&mut first, &first_actor, "admin:advance-history")
        .await
        .unwrap();
    let mut later = transaction(&mut second, &second_actor, "admin:advance-history")
        .await
        .unwrap();
    sqlx::query(advance)
        .bind(3_i32)
        .execute(&mut *earlier)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        sqlx::query(advance)
            .bind(4_i32)
            .execute(&mut *later)
            .await
            .unwrap();
        later.commit().await.unwrap();
    })
    .await
    .expect("writers on independent rows can commit out of order");
    earlier.commit().await.unwrap();

    run(
        url,
        &format!(
            r#"
DO $$ BEGIN
    ASSERT EXISTS (
        SELECT FROM public.history_probe_history AS first
        JOIN public.history_probe_history AS second USING (row_key)
        WHERE first.row_key = '{{"id":1}}' AND first.type = 'update' AND second.type = 'update'
          AND first.changed_by = '{HISTORY_ACTOR}' AND second.changed_by = '{OTHER_HISTORY_ACTOR}'
          AND first.position < second.position AND first.transaction_id <> second.transaction_id
          AND first.before -> 'amount' = '1' AND first.after = second.before
          AND second.after -> 'amount' = '3');
    ASSERT (SELECT count(*) FROM public.history_probe_history WHERE row_key = '{{"id":2}}') = 2;
    ASSERT EXISTS (SELECT FROM public.history_probe_history
                   WHERE row_key = '{{"id":2}}' AND type = 'update'
                     AND changed_by = '{OTHER_HISTORY_ACTOR}' AND before -> 'amount' = '1'
                     AND after -> 'amount' = '2');
    ASSERT EXISTS (
        SELECT FROM public.history_probe_history AS first, public.history_probe_history AS second
        WHERE first.row_key = '{{"id":3}}' AND second.row_key = '{{"id":4}}'
          AND first.type = 'update' AND second.type = 'update'
          AND first.position < second.position
          AND first.after -> 'amount' = '2' AND second.after -> 'amount' = '2');
END $$;
"#
        ),
    );
}

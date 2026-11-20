//! Database constraints and application principal boundaries.

use std::fmt::Write as _;

use std::path::Path;

use wamn_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login};

const APP_GENERATION_PASSWORD: &str = "test-owned-app-generation-password";

fn deploy_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy")
}

fn app_schema_sql() -> String {
    format!(
        "\nDROP SCHEMA IF EXISTS catalog CASCADE;\n{}\n{}",
        wamn_catalog::CATALOG_SCHEMA_SQL,
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
) -> wamn_identity_protocol::LoginName {
    workload_login(
        WorkloadRoleFamily::App,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key,
            database,
        },
    )
    .expect("App accepts project target")
}

/// Apply `deploy/sql/app-schema.sql` to a test database and assert the live,
/// DB-enforced behavior, as the superuser (the harness prepares project-targeted
/// App generations).
#[tokio::test]
#[ignore = "requires: postgres"]
async fn app_schema_applies_and_enforces_isolation_on_postgres() {
    const U1: &str = "11111111-1111-1111-1111-111111111111";
    const U2: &str = "22222222-2222-2222-2222-222222222222";
    const U3: &str = "33333333-3333-3333-3333-333333333333";
    const U4: &str = "44444444-4444-4444-4444-444444444444";
    const PROJECT_ENV_KEY_1: &str = "t1--dev--project";
    const PROJECT_ENV_KEY_2: &str = "t2--dev--project";

    let _serialized = wamn_test_postgres::lock();
    let mut case_server = wamn_test_postgres::start(&[]).unwrap();
    let test_database = url::Url::parse(
        case_server
            .create_database("fixture_1")
            .await
            .unwrap()
            .url(),
    )
    .unwrap();
    let url = test_database.as_str().to_owned();

    let database = current_database(&url);
    let project_env_key_1_app = app_generation(&database, &PROJECT_ENV_KEY_1.parse().unwrap());

    let other = url::Url::parse(
        case_server
            .create_database("fixture_extra")
            .await
            .unwrap()
            .url(),
    )
    .unwrap();
    let other_database = current_database(other.as_str());
    let project_env_key_2_app =
        app_generation(&other_database, &PROJECT_ENV_KEY_2.parse().unwrap());
    let mut other_script = wamn_provision::test_database::login_sql(
        WorkloadRoleFamily::App,
        &other_database,
        &project_env_key_2_app,
        APP_GENERATION_PASSWORD,
    );
    other_script.push_str(sql::revoke_public_connect_base_grants_sql());
    other_script.push_str(&record_history_sql());
    other_script.push_str(&record_history_app_grants_sql());
    other_script.push_str(&app_schema_sql());
    writeln!(other_script, "SET ROLE {project_env_key_2_app}; DO $$ BEGIN
        ASSERT (SELECT count(*) FROM platform.role)=0, 'another environment sees none of the first environment roles';
        END $$; RESET ROLE;").unwrap();
    run(other.as_str(), &other_script);

    // Production preparation hardens the stable ACL carrier to passwordless
    // NOLOGIN/NOINHERIT and makes each project generation the only LOGIN.
    let mut script = wamn_provision::test_database::login_sql(
        WorkloadRoleFamily::App,
        &database,
        &project_env_key_1_app,
        APP_GENERATION_PASSWORD,
    );
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
        "BEGIN; INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES('{U1}'::uuid,'','fixture','admin:seed-isolation-fixture');\n\
         INSERT INTO platform.\"user\" (id, type, email) VALUES \
           ('{U1}','user','u1@t1'),('{U2}','service','u2@t1'),('{U3}','user','u3@t2');\n\
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
           ASSERT (SELECT count(*) FROM platform.\"user\")=3, 't1 sees every user of the database';\n\
           ASSERT (SELECT count(*) FROM platform.role)=2, 't1 sees its roles';\n\
           ASSERT (SELECT count(*) FROM platform.user_role)=1, 't1 sees its grant';\n\
           ASSERT (SELECT count(*) FROM platform.permission)=1, 't1 sees its permission';\n\
           ASSERT (SELECT count(*) FROM platform.setting)=1, 't1 sees the setting of the database';\n\
         END $$;\n\
         COMMIT;"
    )
    .expect("writing to a String cannot fail");
    writeln!(
        script,
        "DO $$ BEGIN
        ASSERT NOT has_database_privilege('{project_env_key_2_app}', current_database(), 'CONNECT'),
        'another environment credential cannot connect to this database'; END $$;"
    )
    .unwrap();
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
    // The status CHECK rejects a bad row.
    writeln!(
        script,
        "INSERT INTO platform.login_principal(login,principal_id) VALUES(current_user,'{}');",
        wamn_provision::PlatformComponent::Provisioning.principal_id()
    )
    .expect("writing to a String cannot fail");
    writeln!(
        script,
        "DO $$ BEGIN BEGIN\n\
           INSERT INTO platform.\"user\" (id, type, email, status) VALUES ('{U4}','user','x@t1','zombie');\n\
           ASSERT false, 'an unknown user status must be rejected';\n\
         EXCEPTION WHEN check_violation THEN NULL; END; END $$;"
    )
    .expect("writing to a String cannot fail");
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
    // A second holder of the admin role, so the role cascade below removes a
    // grant that the user cascade leaves.
    writeln!(
        script,
        "INSERT INTO platform.user_role (user_id, role_name) VALUES ('{U2}','admin');"
    )
    .expect("writing to a String cannot fail");
    // FK cascade: deleting U1 prunes its role grant.
    writeln!(
        script,
        "DELETE FROM platform.\"user\" WHERE id='{U1}';\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM platform.user_role WHERE user_id='{U1}')=0, 'user_role cascade';\n\
         END $$;"
    )
    .expect("writing to a String cannot fail");
    // FK cascade: deleting the clerk role prunes its permission, and deleting
    // the admin role prunes its remaining grant.
    script.push_str(
        "DELETE FROM platform.role WHERE name IN ('admin', 'clerk');\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM platform.permission WHERE role_name='clerk')=0, 'permission cascade';\n\
           ASSERT (SELECT count(*) FROM platform.user_role WHERE role_name='admin')=0, 'user_role cascade from role';\n\
         END $$;\n",
    );

    script.push_str("DROP SCHEMA platform CASCADE;\n");

    run(&url, &script);
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

/// Row changes record the actor and operation, including key changes and cascades.
#[sqlx::test(migrations = false)]
async fn platform_relations_log_every_row_change_on_postgres(pool: sqlx::PgPool) {
    const U1: &str = "11111111-1111-1111-1111-111111111111";
    const U2: &str = "22222222-2222-2222-2222-222222222222";
    const U3: &str = "33333333-3333-3333-3333-333333333333";
    const SEED: &str = "admin:seed-history-fixture";
    const CHANGE: &str = "admin:change-history-fixture";
    const REMOVE: &str = "admin:remove-history-fixture";

    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let url = test_database.as_str();
    let mut script = sql::ensure_app_acl_role_sql();
    script.push_str("\nDROP SCHEMA IF EXISTS platform CASCADE;\n");
    script.push_str(&record_history_sql());
    script.push_str(&record_history_app_grants_sql());
    script.push_str(&app_schema_sql());
    writeln!(
        script,
        "BEGIN;\n\
         INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES('{U1}'::uuid,'','fixture','{SEED}');\n\
         INSERT INTO platform.\"user\" (id, type, email) VALUES \
           ('{U1}', 'user', 'u1@t1'), ('{U2}', 'user', 'u2@t2'), \
           ('{U3}', 'service', 'u3@t1');\n\
         INSERT INTO platform.role (name) VALUES ('admin'), ('auditor');\n\
         INSERT INTO platform.user_role (user_id, role_name) VALUES \
           ('{U1}', 'admin'), ('{U3}', 'admin');\n\
         INSERT INTO platform.permission (role_name, permission) VALUES \
           ('auditor', 'platform-fixture:widget/get');\n\
         INSERT INTO platform.setting (target_type, target_id, key, value) VALUES \
           ('org', 'acme', 'display.theme', '\"dark\"');\n\
         COMMIT;\n\
         BEGIN;\n\
         INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES('{U2}'::uuid,'','fixture','{CHANGE}');\n\
         UPDATE platform.\"user\" SET status = 'disabled' WHERE id = '{U2}';\n\
         UPDATE platform.role SET description = 'administrators' WHERE name = 'admin';\n\
         UPDATE platform.user_role SET role_name = 'auditor' WHERE user_id = '{U3}';\n\
         UPDATE platform.permission SET permission = 'platform-fixture:widget/list';\n\
         UPDATE platform.setting SET value = '\"light\"';\n\
         COMMIT;\n\
         BEGIN;\n\
         INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES('{U3}'::uuid,'','fixture','{REMOVE}');\n\
         DELETE FROM platform.setting;\n\
         DELETE FROM platform.\"user\" WHERE id = '{U1}';\n\
         DELETE FROM platform.\"user\" WHERE id = '{U2}';\n\
         DELETE FROM platform.role WHERE name IN ('admin', 'auditor');\n\
         COMMIT;"
    )
    .expect("writing to a String cannot fail");
    // Each fixture mutation writes one entry. A key change writes a delete
    // and an insert instead of an update.
    for (table, inserted, updated, removed) in [
        ("user", 3, 1, 2),
        ("role", 2, 1, 2),
        ("user_role", 2, 0, 2),
        ("permission", 1, 0, 1),
        ("setting", 1, 1, 1),
    ] {
        let key_change = updated == 0;
        let changed = if key_change { 2 } else { updated };
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
            AND row_key ->> 'id' = '{U2}' AND before ->> 'status' = 'active' AND after ->> 'status' = 'disabled');
        ASSERT EXISTS (SELECT FROM platform.role_history WHERE type = 'update'
            AND row_key ->> 'name' = 'admin' AND after ->> 'description' = 'administrators');
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

#[sqlx::test(migrations = false)]
async fn row_image_spells_timestamptz_as_the_platform_canonicalizer_on_postgres(
    pool: sqlx::PgPool,
) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let url = test_database.as_str().to_owned();

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
            wamn_postgres::types::canonical_timestamptz(held),
            "the row image must spell the held value as the canonicalizer does"
        );
    }
}

/// `wamn_history.timestamptz_image` spells a `timestamptz` value exactly as the
/// platform canonicalizer spells it, and exactly as `wamn_history.row_image`
/// spells the same column. A history read builds its current row image with
/// this function. The session time zone is not UTC, and the values include
/// zero and non-zero microseconds and infinity.
#[sqlx::test(migrations = false)]
async fn timestamptz_image_spells_timestamptz_as_the_platform_canonicalizer_on_postgres(
    pool: sqlx::PgPool,
) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let url = test_database.as_str().to_owned();

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
            wamn_postgres::types::canonical_timestamptz(held),
            "the value image must spell the held value as the canonicalizer does"
        );
    }
}

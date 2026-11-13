//! Database constraints and application principal boundaries.

use std::fmt::Write as _;

use std::path::Path;

use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, platform_principals_sql, sql,
    workload_generation_role,
};
use wamn_identity_protocol::PlatformComponent;

const APP_GENERATION_PASSWORD: &str = "test-owned-app-generation-password";
const APP_GENERATION_VALID_UNTIL: &str = "2099-01-01T00:00:00Z";

fn deploy_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy")
}

fn app_schema_sql() -> String {
    format!(
        "\nDROP SCHEMA IF EXISTS catalog CASCADE;\n{}\n{}",
        wamn_catalog::CATALOG_SCHEMA_SQL,
        include_str!("../../../../deploy/sql/app-schema.sql")
    )
}

/// The platform stamp function that every applier installs before
/// `deploy/sql/app-schema.sql`.
fn record_history_sql() -> String {
    std::fs::read_to_string(deploy_dir().join("sql/record-history.sql"))
        .expect("read deploy/sql/record-history.sql")
}

/// The `wamn_app` record history grants that every tenant applier installs
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

fn app_generation(database: &str, env_key: &str) -> String {
    workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::EnvKey { env_key, database },
        CredentialGeneration::A,
    )
    .expect("App accepts tenant scope")
}

/// Apply `deploy/sql/app-schema.sql` to a test database and assert the live,
/// DB-enforced behavior, as the superuser (the harness prepares tenant-scoped
/// App generations).
#[test]
fn app_schema_applies_and_enforces_isolation_on_postgres() {
    const U1: &str = "11111111-1111-1111-1111-111111111111";
    const U2: &str = "22222222-2222-2222-2222-222222222222";
    const U3: &str = "33333333-3333-3333-3333-333333333333";
    const U4: &str = "44444444-4444-4444-4444-444444444444";
    const ENV_KEY_1: &str = "t1";
    const ENV_KEY_2: &str = "t2";

    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

    let database = current_database(&url);
    let env_key_1_app = app_generation(&database, ENV_KEY_1);
    let other = wamn_test_postgres::database();
    let other_database = current_database(other.url());
    let env_key_2_app = app_generation(&other_database, ENV_KEY_2);
    let mut other_script = sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::App,
        &other_database,
        &env_key_2_app,
        APP_GENERATION_PASSWORD,
        APP_GENERATION_VALID_UNTIL,
    );
    other_script.push_str(sql::revoke_public_connect_floor_sql());
    other_script.push_str(&record_history_sql());
    other_script.push_str(&record_history_app_grants_sql());
    other_script.push_str(&app_schema_sql());
    writeln!(other_script, "SET ROLE {env_key_2_app}; DO $$ BEGIN
        ASSERT (SELECT count(*) FROM platform.role)=0, 'another environment sees none of the first environment roles';
        END $$; RESET ROLE;").unwrap();
    run(other.url(), &other_script);

    // Production preparation hardens the stable ACL carrier to passwordless
    // NOLOGIN/NOINHERIT and makes each tenant generation the only LOGIN.
    let mut script = sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::App,
        &database,
        &env_key_1_app,
        APP_GENERATION_PASSWORD,
        APP_GENERATION_VALID_UNTIL,
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
        "SET app.user_id = '{U1}';\n\
         SET app.operation = 'admin:seed-isolation-fixture';\n\
         INSERT INTO platform.\"user\" (id, type, email) VALUES \
           ('{U1}','user','u1@t1'),('{U2}','service','u2@t1'),('{U3}','user','u3@t2');\n\
         INSERT INTO platform.role (name) VALUES ('admin'),('clerk');\n\
         INSERT INTO platform.user_role (user_id, role_name) VALUES ('{U1}','admin');\n\
         INSERT INTO platform.permission (role_name, permission) VALUES ('clerk','platform-fixture:widget/get');\n\
         INSERT INTO platform.api_key (user_id, name, key_hash, prefix) VALUES ('{U1}','ci','hash-1','wk_a');\n\
         INSERT INTO platform.setting (scope_type, scope_id, key, value) VALUES ('user','{U1}','display.theme','\"dark\"'::jsonb);"
    )
    .expect("writing to a String cannot fail");

    // Each generation reads the users, roles and settings of its database.
    writeln!(
        script,
        "BEGIN;\n\
         SET LOCAL ROLE {env_key_1_app};\n\
         DO $$ BEGIN\n\
           ASSERT current_user='{env_key_1_app}', 'tenant authority is the prepared tenant-1 generation';\n\
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
        ASSERT NOT has_database_privilege('{env_key_2_app}', current_database(), 'CONNECT'),
        'another environment credential cannot connect to this database'; END $$;"
    )
    .unwrap();
    // A caller cannot alter the database boundary with a settable claim.
    writeln!(
        script,
        "         BEGIN;\n\
         SET LOCAL ROLE {env_key_1_app};\n\
         SET LOCAL app.env_key = '{ENV_KEY_2}';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM platform.role)=2, 'a settable tenant claim cannot spoof tenant 2'; END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL app.env_key = '{ENV_KEY_1}';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM platform.role)=2, 'the ACL carrier reads the database roles'; END $$;\n\
         COMMIT;"
    )
    .expect("writing to a String cannot fail");
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
            OR EXISTS (SELECT FROM pg_attribute a WHERE a.attrelid=c.oid AND a.attname='env_key' AND NOT a.attisdropped)
        )), 'application tables must use the database boundary'; END $$;\n");
    // The retired tenant column is refused instead of accepted as input.
    for (table, insert) in [
        ("role", "(env_key, name) VALUES ('', 'empty')".to_owned()),
        (
            "user_role",
            format!("(env_key, user_id, role_name) VALUES ('', '{U2}', 'admin')"),
        ),
        (
            "permission",
            "(env_key, role_name, permission) VALUES ('', 'clerk', 'platform-fixture:widget/get')"
                .to_owned(),
        ),
        (
            "api_key",
            format!(
                "(env_key, user_id, name, key_hash, prefix) VALUES ('', '{U2}', 'ci', 'hash-2', 'wk_b')"
            ),
        ),
    ] {
        writeln!(
            script,
            "DO $$ BEGIN BEGIN\n\
               INSERT INTO platform.{table} {insert};\n\
               ASSERT false, 'a ''''-tenant {table} row must be rejected (a45)';\n\
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
    // FK cascade: deleting U1 prunes its role grant and api key.
    writeln!(
        script,
        "DELETE FROM platform.\"user\" WHERE id='{U1}';\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM platform.user_role WHERE user_id='{U1}')=0, 'user_role cascade';\n\
           ASSERT (SELECT count(*) FROM platform.api_key WHERE user_id='{U1}')=0, 'api_key cascade';\n\
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

/// A fresh tenant database holds the platform rows that the provisioning
/// function renders, with the ids that `PlatformComponent::principal_id`
/// derives. The users CHECKs refuse every other shape: a missing or unknown
/// type, a platform row with a name or id outside the pinned pairs, and a
/// user or service row with a `wamn:` name.
#[test]
fn platform_rows_carry_their_pinned_ids_on_postgres() {
    const DOMAIN: &str = "example.invalid";
    const USER: &str = "11111111-1111-1111-1111-111111111111";
    const FIXTURE_USER: &str = "00000000-0000-0000-0000-000000000000";

    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

    let provisioning = PlatformComponent::Provisioning;
    let executor = PlatformComponent::Executor;
    let mut script = sql::ensure_app_acl_role_sql();
    script.push_str("\nDROP SCHEMA IF EXISTS platform CASCADE;\n");
    script.push_str(&record_history_sql());
    script.push_str(&record_history_app_grants_sql());
    script.push_str(&app_schema_sql());
    script.push_str("\nBEGIN;\n");
    script.push_str(&platform_principals_sql(DOMAIN).expect("example.invalid is a domain name"));
    // The fixture writes as its first admitted user row below.
    writeln!(
        script,
        "COMMIT;\nSET app.user_id = '{FIXTURE_USER}';\n\
         SET app.operation = 'admin:seed-platform-row-fixture';"
    )
    .expect("writing to a String cannot fail");
    // Every refused row sits in its own subtransaction, so the fresh rows
    // above stay as the only rows in the table.
    let refused = [
        (
            "not_null_violation",
            format!(
                "INSERT INTO platform.\"user\" (id, email) \
                 VALUES ('{USER}', 'untyped@{DOMAIN}')"
            ),
        ),
        (
            "check_violation",
            format!(
                "INSERT INTO platform.\"user\" (id, type, email) \
                 VALUES ('{USER}', 'robot', 'robot@{DOMAIN}')"
            ),
        ),
        (
            "check_violation",
            format!(
                "INSERT INTO platform.\"user\" (id, type, email) \
                 VALUES ('{USER}', 'platform', 'unnamed@{DOMAIN}')"
            ),
        ),
        (
            "check_violation",
            format!(
                "INSERT INTO platform.\"user\" (id, type, email, display_name) \
                 VALUES ('{}', 'platform', 'swapped@{DOMAIN}', '{}')",
                executor.principal_id(),
                provisioning.principal_name(),
            ),
        ),
        (
            "check_violation",
            format!(
                "INSERT INTO platform.\"user\" (id, type, email, display_name) \
                 VALUES ('{USER}', 'platform', 'unknown@{DOMAIN}', 'wamn:unknown')"
            ),
        ),
    ]
    .into_iter()
    .chain(
        ["user", "service"]
            .into_iter()
            .flat_map(|user_type| {
                [
                    format!(
                        "INSERT INTO platform.\"user\" (id, type, email, display_name) \
                         VALUES ('{USER}', '{user_type}', 'named@{DOMAIN}', 'wamn:user')"
                    ),
                    format!(
                        "INSERT INTO platform.\"user\" (id, type, email, display_name) \
                         VALUES ('{}', '{user_type}', 'pinned@{DOMAIN}', '{}')",
                        provisioning.principal_id(),
                        provisioning.principal_name(),
                    ),
                ]
            })
            .map(|statement| ("check_violation", statement)),
    );
    for (condition, statement) in refused {
        let quoted = statement.replace('\'', "''");
        writeln!(
            script,
            "DO $$ BEGIN BEGIN\n\
               EXECUTE '{quoted}';\n\
               RAISE EXCEPTION 'the users CHECKs admitted: %', '{quoted}';\n\
             EXCEPTION WHEN {condition} THEN NULL; END; END $$;"
        )
        .expect("writing to a String cannot fail");
    }
    // Every user type is admitted.
    for (index, user_type) in ["user", "service"].into_iter().enumerate() {
        writeln!(
            script,
            "INSERT INTO platform.\"user\" (id, type, email, display_name) \
             VALUES ('00000000-0000-0000-0000-00000000000{index}', '{user_type}', \
                     '{user_type}@{DOMAIN}', 'Fixture {user_type}');"
        )
        .expect("writing to a String cannot fail");
    }
    for component in PlatformComponent::ALL {
        writeln!(script, "DO $$ BEGIN ASSERT EXISTS (SELECT FROM platform.\"user\" WHERE id='{}' AND type='platform' AND display_name='{}'), 'platform principal must retain its identity'; END $$;", component.principal_id(), component.principal_name()).unwrap();
    }
    run(&url, &script);
}

/// Provisioning stamps its writes and releases the transaction binding.
#[test]
fn platform_relations_stamp_provisioning_writes_on_postgres() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url();
    let mut script = sql::ensure_app_acl_role_sql();
    script.push_str("\nDROP SCHEMA IF EXISTS platform CASCADE;\n");
    script.push_str(&record_history_sql());
    script.push_str(&record_history_app_grants_sql());
    script.push_str(&app_schema_sql());
    script.push_str("\nBEGIN;\n");
    script.push_str(&platform_principals_sql("example.invalid").unwrap());
    script.push_str(
        "COMMIT;\nDO $$ BEGIN
        ASSERT COALESCE(current_setting('app.user_id', true), '') = '';
        ASSERT COALESCE(current_setting('app.operation', true), '') = '';
        END $$;\n",
    );
    let provisioning = PlatformComponent::Provisioning.principal_id();
    let operation = PlatformComponent::Provisioning.principal_name();
    for component in PlatformComponent::ALL {
        let id = component.principal_id();
        writeln!(script, "DO $$ BEGIN
            ASSERT EXISTS (SELECT FROM platform.\"user\" WHERE id = '{id}'
                AND created_by = '{provisioning}' AND updated_by = '{provisioning}'
                AND created_at IS NOT NULL AND updated_at >= created_at);
            ASSERT EXISTS (SELECT FROM platform.user_history WHERE row_key ->> 'id' = '{id}'
                AND type = 'insert' AND changed_by = '{provisioning}' AND operation = '{operation}');
            END $$;").unwrap();
    }
    run(url, &script);
}

/// Row changes record the actor and operation, including key changes and cascades.
#[test]
fn platform_relations_log_every_row_change_on_postgres() {
    const U1: &str = "11111111-1111-1111-1111-111111111111";
    const U2: &str = "22222222-2222-2222-2222-222222222222";
    const U3: &str = "33333333-3333-3333-3333-333333333333";
    const K1: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const K2: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const SEED: &str = "admin:seed-history-fixture";
    const CHANGE: &str = "admin:change-history-fixture";
    const REMOVE: &str = "admin:remove-history-fixture";

    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url();
    let mut script = sql::ensure_app_acl_role_sql();
    script.push_str("\nDROP SCHEMA IF EXISTS platform CASCADE;\n");
    script.push_str(&record_history_sql());
    script.push_str(&record_history_app_grants_sql());
    script.push_str(&app_schema_sql());
    writeln!(
        script,
        "BEGIN;\n\
         SET LOCAL app.user_id = '{U1}';\n\
         SET LOCAL app.operation = '{SEED}';\n\
         INSERT INTO platform.\"user\" (id, type, email) VALUES \
           ('{U1}', 'user', 'u1@t1'), ('{U2}', 'user', 'u2@t2'), \
           ('{U3}', 'service', 'u3@t1');\n\
         INSERT INTO platform.role (name) VALUES ('admin'), ('auditor');\n\
         INSERT INTO platform.user_role (user_id, role_name) VALUES \
           ('{U1}', 'admin'), ('{U3}', 'admin');\n\
         INSERT INTO platform.permission (role_name, permission) VALUES \
           ('auditor', 'platform-fixture:widget/get');\n\
         INSERT INTO platform.setting (scope_type, scope_id, key, value) VALUES \
           ('org', 'acme', 'display.theme', '\"dark\"');\n\
         INSERT INTO platform.api_key (id, user_id, name, key_hash, prefix) VALUES \
           ('{K1}', '{U1}', 'ci', 'hash-1', 'wk_a'), ('{K2}', '{U1}', 'cd', 'hash-2', 'wk_b');\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL app.user_id = '{U2}';\n\
         SET LOCAL app.operation = '{CHANGE}';\n\
         UPDATE platform.\"user\" SET status = 'disabled' WHERE id = '{U2}';\n\
         UPDATE platform.role SET description = 'administrators' WHERE name = 'admin';\n\
         UPDATE platform.user_role SET role_name = 'auditor' WHERE user_id = '{U3}';\n\
         UPDATE platform.permission SET permission = 'platform-fixture:widget/list';\n\
         UPDATE platform.setting SET value = '\"light\"';\n\
         UPDATE platform.api_key SET revoked_at = '2026-09-14T00:00:00Z' WHERE id = '{K1}';\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL app.user_id = '{U3}';\n\
         SET LOCAL app.operation = '{REMOVE}';\n\
         DELETE FROM platform.api_key WHERE id = '{K2}';\n\
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
        ("api_key", 2, 1, 2),
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
        ASSERT EXISTS (SELECT FROM platform.api_key_history WHERE type = 'update'
            AND row_key ->> 'id' = '{K1}' AND after ->> 'revoked_at' IS NOT NULL);
        ASSERT EXISTS (SELECT FROM platform.user_role_history WHERE type = 'delete'
            AND operation = '{CHANGE}' AND row_key ->> 'user_id' = '{U3}' AND row_key ->> 'role_name' = 'admin');
        ASSERT EXISTS (SELECT FROM platform.user_role_history WHERE type = 'insert'
            AND operation = '{CHANGE}' AND row_key ->> 'user_id' = '{U3}' AND row_key ->> 'role_name' = 'auditor');
        ASSERT EXISTS (SELECT FROM platform.permission_history WHERE type = 'delete'
            AND operation = '{CHANGE}' AND row_key ->> 'permission' = 'platform-fixture:widget/get');
        ASSERT EXISTS (SELECT FROM platform.permission_history WHERE type = 'insert'
            AND operation = '{CHANGE}' AND row_key ->> 'permission' = 'platform-fixture:widget/list');
        ASSERT EXISTS (SELECT FROM platform.api_key_history WHERE type = 'insert'
            AND row_key ->> 'id' = '{K1}' AND after ->> 'key_hash' = 'hash-1');
        ASSERT EXISTS (SELECT FROM platform.api_key_history WHERE type = 'delete'
            AND operation = '{REMOVE}' AND row_key ->> 'id' = '{K1}' AND before ->> 'key_hash' = 'hash-1');
        ASSERT EXISTS (SELECT FROM platform.user_role_history WHERE type = 'delete'
            AND operation = '{REMOVE}' AND row_key ->> 'user_id' = '{U1}');
        ASSERT EXISTS (SELECT FROM platform.user_role_history WHERE type = 'delete'
            AND operation = '{REMOVE}' AND row_key ->> 'user_id' = '{U3}' AND row_key ->> 'role_name' = 'auditor');
        ASSERT EXISTS (SELECT FROM platform.permission_history WHERE type = 'delete'
            AND operation = '{REMOVE}' AND row_key ->> 'permission' = 'platform-fixture:widget/list');
        ASSERT NOT EXISTS (SELECT FROM platform.user_role);
        ASSERT NOT EXISTS (SELECT FROM platform.permission);
        ASSERT NOT EXISTS (SELECT FROM platform.api_key);
        END $$;").unwrap();
    run(url, &script);
}

#[test]
fn row_image_spells_timestamptz_as_the_platform_canonicalizer_on_postgres() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

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
            wamn_runtime::plugins::wamn_postgres::canonical_timestamptz(held),
            "the row image must spell the held value as the canonicalizer does"
        );
    }
}

/// `wamn_history.timestamptz_image` spells a `timestamptz` value exactly as the
/// platform canonicalizer spells it, and exactly as `wamn_history.row_image`
/// spells the same column. A history read builds its current row image with
/// this function. The session time zone is not UTC, and the values include
/// zero and non-zero microseconds and infinity.
#[test]
fn timestamptz_image_spells_timestamptz_as_the_platform_canonicalizer_on_postgres() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

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
            wamn_runtime::plugins::wamn_postgres::canonical_timestamptz(held),
            "the value image must spell the held value as the canonicalizer does"
        );
    }
}

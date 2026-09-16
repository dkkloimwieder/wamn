//! Storage-schema tests for the per-project system schema v1 (wamn-as5).
//!
//! Two layers (the `wamn-control-registry` / `deploy/sql/system-schema.sql` precedent):
//! - a **drift guard** tying `deploy/sql/app-schema.sql` to the `wamn-project-state`
//!   model (the schema name, each table + its pinned columns, the RLS floor +
//!   a45 empty-tenant-row hardening, the `users.status` CHECK literals from
//!   `UserStatus::as_str`, and the FK cascades);
//! - a **live-apply gate** showing the DB-enforced behavior — tenant RLS
//!   isolation, the FK cascades, the empty-tenant / status / type CHECKs, and
//!   the platform principal rows — on a test database of the test PostgreSQL
//!   server (a superuser URL; the harness prepares App generations).

use std::fmt::Write as _;

use std::path::Path;

use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, platform_principals_sql, sql,
    workload_generation_role,
};
use wamn_project_state::{PlatformComponent, SCHEMA_NAME, TABLES, UserStatus, UserType};

const APP_GENERATION_PASSWORD: &str = "test-owned-app-generation-password";
const APP_GENERATION_VALID_UNTIL: &str = "2099-01-01T00:00:00Z";

fn deploy_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy")
}

fn app_schema_sql() -> String {
    std::fs::read_to_string(deploy_dir().join("sql/app-schema.sql"))
        .expect("read deploy/sql/app-schema.sql")
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

/// The SQL with `--` line comments stripped, so text assertions test the actual
/// DDL and not the explanatory prose (the header names the app.user_id/app.role
/// claims to explain the integration, but they do not appear in the DDL itself).
/// No `--` appears inside a string literal in this file, so a per-line truncate
/// is exact.
fn code_only(sql: &str) -> String {
    sql.lines()
        .map(|l| l.find("--").map_or(l, |i| &l[..i]))
        .collect::<Vec<_>>()
        .join("\n")
}

// --- drift guard: DDL ↔ model ----------------------------------------------

/// The privileges `wamn_app` holds on each table. The R11 adjudication is per
/// relation, not one blanket grant, so the drift guard pins it per relation too:
///
/// - `users` / `roles` / `user_roles` / `permissions` / `api_keys` are
///   SELECT-only. The trust chain reads these rows as authorization INPUT — the
///   3.5 builder compiles `app.user_id` (a `users.id`) and `app.role` (a
///   `roles.name`, reached through `user_roles`) into the data policies it
///   generates, and 4.2 resolves those claims from exactly these tables. Author
///   SQL holding DML here could write itself the credential its own policies are
///   then evaluated against.
/// - `configurations` is tenant business state — nothing in the trust chain reads
///   it — so it deliberately keeps full DML.
/// - The six `<table>_history` tables have no read. The log trigger fires as the
///   writer, and `wamn_app` writes `configurations` only, so `wamn_app` holds
///   `INSERT` on the entry columns of `configurations_history` and no other
///   privilege. The grant leaves out `position`.
///
/// A table the model gains later has no adjudicated class, so this panics rather
/// than silently defaulting it into one.
fn wamn_app_privileges(table: &str) -> &'static str {
    match table {
        "users" | "roles" | "user_roles" | "permissions" | "api_keys" => "SELECT",
        "configurations" => "SELECT, INSERT, UPDATE, DELETE",
        "users_history"
        | "roles_history"
        | "user_roles_history"
        | "permissions_history"
        | "api_keys_history" => "",
        "configurations_history" => {
            "INSERT (tenant_id, row_key, kind, operation, changed_by, changed_at, \
             transaction_id, before, after)"
        }
        other => panic!("table {other} has no adjudicated wamn_app grant (R11)"),
    }
}

/// `deploy/sql/app-schema.sql` must mirror the `wamn-project-state` model: the schema
/// name, every table + its pinned columns, and the tenant RLS floor on each.
#[test]
fn app_schema_sql_mirrors_the_model() {
    let sql = code_only(&app_schema_sql());

    assert!(
        sql.contains(&format!("CREATE SCHEMA {SCHEMA_NAME}")),
        "the schema name must match the model ({SCHEMA_NAME})"
    );
    assert!(sql.contains(&format!("GRANT USAGE ON SCHEMA {SCHEMA_NAME} TO wamn_app")));

    for t in TABLES {
        let qualified = t.qualified();
        assert!(
            sql.contains(&format!("CREATE TABLE {qualified}")),
            "app-schema.sql is missing table {qualified}"
        );
        for col in t.columns {
            assert!(
                sql.contains(col),
                "table {qualified} is missing pinned column {col:?}"
            );
        }
        // Every table carries the RLS floor: a tenant policy, FORCE RLS, and the
        // one grant its R11 class allows.
        assert!(
            sql.contains(&format!("CREATE POLICY {}_tenant ON {qualified}", t.name)),
            "table {qualified} is missing its tenant RLS policy"
        );
        assert!(
            sql.contains(&format!("ALTER TABLE {qualified} FORCE ROW LEVEL SECURITY")),
            "table {qualified} must FORCE row level security"
        );
        let privileges = wamn_app_privileges(t.name);
        assert!(
            sql.contains(&format!("GRANT {privileges} ON {qualified} TO wamn_app")),
            "table {qualified} must grant exactly `{privileges}` to wamn_app"
        );
        // …and only that one line, so a second GRANT cannot widen the class back
        // out while the assertion above still passes.
        assert_eq!(
            sql.matches(&format!("ON {qualified} TO wamn_app")).count(),
            1,
            "table {qualified} must carry exactly one wamn_app grant"
        );
    }
}

/// The tenant floor derives from `current_user`, not from a claim the session
/// can set (`wamn-0h0g.22.6.3`). Pinned by expression, not just presence (the
/// drift-guard lesson), and pinned in BOTH directions: the retired boundary
/// must be absent, because a policy that kept it would hand every tenant's rows
/// to whoever sets the GUC.
#[test]
fn tenant_floor_derives_from_the_connected_role() {
    let sql = code_only(&app_schema_sql());
    assert!(
        sql.contains("wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()"),
        "the tenant read must derive from current_user"
    );
    assert!(
        !sql.contains("app.tenant"),
        "a settable tenant claim survived in the app schema"
    );
    // The expression index rides the predicate: without it the derivation
    // sequential-scans every relation. One per table and one per history table.
    let indexes = sql
        .matches("((wamn_authority.tenant_key(tenant_id)))")
        .count();
    assert_eq!(
        indexes,
        2 * TABLES.len(),
        "every table and its history table must carry a tenant-key expression index"
    );
    // Every table still forbids a ''-tenant row (one CHECK per table). This
    // half of the a45 hardening SURVIVES the re-key: it is what makes a
    // ''-tenant row structurally impossible rather than merely unmatched.
    let checks = sql.matches("CHECK (tenant_id <> '')").count();
    assert_eq!(
        checks,
        TABLES.len(),
        "every table must CHECK (tenant_id <> '') — one per table"
    );
}

/// The `users.status` CHECK literals come from the model (`UserStatus::as_str`),
/// drift-guarded like the registry's tier/env literals. The live arm below
/// asks PostgreSQL for the `users.id` type and default.
#[test]
fn user_status_literals_are_pinned() {
    let sql = code_only(&app_schema_sql());
    assert!(sql.contains("users_status_check"));
    for s in UserStatus::ALL {
        assert!(
            sql.contains(&format!("'{}'", s.as_str())),
            "app-schema.sql is missing the users.status literal {:?}",
            s.as_str()
        );
    }
}

/// The FK cascades that keep the graph consistent are pinned: the user↔role
/// linkage and api_keys reference users ON DELETE CASCADE; permissions and the
/// linkage reference roles ON DELETE CASCADE.
#[test]
fn fk_cascades_are_pinned() {
    let sql = code_only(&app_schema_sql());
    assert!(
        sql.contains("REFERENCES app_system.users (tenant_id, id) ON DELETE CASCADE"),
        "user_roles / api_keys must FK users ON DELETE CASCADE"
    );
    assert!(
        sql.contains("REFERENCES app_system.roles (tenant_id, name) ON DELETE CASCADE"),
        "user_roles / permissions must FK roles ON DELETE CASCADE"
    );
}

// --- live-apply gate --------------------------------------------------------

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

fn app_generation(database: &str, tenant: &str) -> String {
    workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant { tenant, database },
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
    const TENANT_1: &str = "t1";
    const TENANT_2: &str = "t2";

    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

    let database = current_database(&url);
    let tenant_1_app = app_generation(&database, TENANT_1);
    let tenant_2_app = app_generation(&database, TENANT_2);

    // Production preparation hardens the stable ACL carrier to passwordless
    // NOLOGIN/NOINHERIT and makes each tenant generation the only LOGIN.
    let mut script = sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::App,
        &database,
        &tenant_1_app,
        APP_GENERATION_PASSWORD,
        APP_GENERATION_VALID_UNTIL,
    );
    script.push('\n');
    script.push_str(&sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::App,
        &database,
        &tenant_2_app,
        APP_GENERATION_PASSWORD,
        APP_GENERATION_VALID_UNTIL,
    ));
    script.push_str(
        "\nDROP SCHEMA IF EXISTS app_system CASCADE;\n\
         DROP SCHEMA IF EXISTS wamn_sysschema_test CASCADE;\n",
    );
    // The schema itself (deploy/sql/app-schema.sql, applied verbatim as the superuser).
    script.push_str(&record_history_sql());
    script.push_str(&record_history_app_grants_sql());
    script.push_str(&app_schema_sql());
    script.push('\n');
    // Seed as the superuser (bypasses RLS): two tenants for the isolation test,
    // known user ids to tie the docs rows to. U1 has a role, key, and config.
    // The fixture writes as U1, its test principal, whose row stamps itself.
    writeln!(
        script,
        "SET app.user_id = '{U1}';\n\
         SET app.operation = 'admin:seed-isolation-fixture';\n\
         INSERT INTO app_system.users (tenant_id, id, type, email) VALUES \
           ('t1','{U1}','person','u1@t1'),('t1','{U2}','service','u2@t1'),('t2','{U3}','person','u3@t2');\n\
         INSERT INTO app_system.roles (tenant_id, name, is_system) VALUES ('t1','admin',true);\n\
         INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) VALUES ('t1','{U1}','admin');\n\
         INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES ('t1','admin','receipts:read');\n\
         INSERT INTO app_system.api_keys (tenant_id, user_id, name, key_hash, prefix) VALUES ('t1','{U1}','ci','hash-1','wk_a');\n\
         INSERT INTO app_system.configurations (tenant_id, config_key, config_value) VALUES ('t1','theme','\"dark\"'::jsonb);"
    )
    .expect("writing to a String cannot fail");

    // Tenant isolation follows current_user's prepared scope: tenant 1 sees only
    // tenant 1's rows without any settable tenant claim.
    writeln!(
        script,
        "BEGIN;\n\
         SET LOCAL ROLE {tenant_1_app};\n\
         DO $$ BEGIN\n\
           ASSERT current_user='{tenant_1_app}', 'tenant authority is the prepared tenant-1 generation';\n\
           ASSERT (SELECT count(*) FROM app_system.users)=2, 't1 sees its 2 users, not t2''s';\n\
           ASSERT (SELECT count(*) FROM app_system.roles)=1, 't1 sees its role';\n\
           ASSERT (SELECT count(*) FROM app_system.user_roles)=1, 't1 sees its grant';\n\
           ASSERT (SELECT count(*) FROM app_system.permissions)=1, 't1 sees its permission';\n\
           ASSERT (SELECT count(*) FROM app_system.api_keys)=1, 't1 sees its api key';\n\
           ASSERT (SELECT count(*) FROM app_system.configurations)=1, 't1 sees its config';\n\
         END $$;\n\
         COMMIT;"
    )
    .expect("writing to a String cannot fail");
    // Tenant 2 sees only its row. Spoofing the retired app.tenant claim does not
    // move tenant 1, and the stable ACL carrier itself maps to no tenant.
    writeln!(
        script,
        "BEGIN;\n\
         SET LOCAL ROLE {tenant_2_app};\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM app_system.users)=1, 't2 sees only its user'; END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE {tenant_1_app};\n\
         SET LOCAL app.tenant = '{TENANT_2}';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM app_system.users)=2, 'a settable tenant claim cannot spoof tenant 2'; END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL app.tenant = '{TENANT_1}';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM app_system.users)=0, 'the stable ACL carrier derives no tenant'; END $$;\n\
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
            WHERE namespace.nspname='app_system' AND relation.relname='users'\n\
              AND a.attname='id' AND a.attnum > 0 AND NOT a.attisdropped;\n\
           ASSERT t='uuid', 'users.id must be uuid (the app.user_id ownership target)';\n\
           ASSERT d IS NULL, 'users.id must have no server default; the org issues it';\n\
         END $$;\n",
    );
    // The status / empty-tenant CHECKs reject bad rows.
    writeln!(
        script,
        "DO $$ BEGIN BEGIN\n\
           INSERT INTO app_system.users (tenant_id, id, type, email, status) VALUES ('t1','{U3}','person','x@t1','zombie');\n\
           ASSERT false, 'an unknown user status must be rejected';\n\
         EXCEPTION WHEN check_violation THEN NULL; END; END $$;\n\
         DO $$ BEGIN BEGIN\n\
           INSERT INTO app_system.users (tenant_id, id, type, email) VALUES ('','{U4}','person','x@none');\n\
           ASSERT false, 'a ''-tenant row must be rejected (a45)';\n\
         EXCEPTION WHEN check_violation THEN NULL; END; END $$;"
    )
    .expect("writing to a String cannot fail");
    // FK cascade: deleting U1 prunes its role grant and api key.
    writeln!(
        script,
        "DELETE FROM app_system.users WHERE tenant_id='t1' AND id='{U1}';\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM app_system.user_roles WHERE user_id='{U1}')=0, 'user_roles cascade';\n\
           ASSERT (SELECT count(*) FROM app_system.api_keys WHERE user_id='{U1}')=0, 'api_keys cascade';\n\
         END $$;"
    )
    .expect("writing to a String cannot fail");

    script.push_str("DROP SCHEMA app_system CASCADE;\n");

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
/// person or service row with a `wamn:` name.
#[test]
fn platform_rows_carry_their_pinned_ids_on_postgres() {
    const TENANT: &str = "t1";
    const DOMAIN: &str = "example.invalid";
    const PERSON: &str = "11111111-1111-1111-1111-111111111111";
    const FIXTURE_PERSON: &str = "00000000-0000-0000-0000-000000000000";

    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

    let provisioning = PlatformComponent::Provisioning;
    let executor = PlatformComponent::Executor;
    let mut script = sql::ensure_app_acl_role_sql();
    script.push_str("\nDROP SCHEMA IF EXISTS app_system CASCADE;\n");
    script.push_str(&record_history_sql());
    script.push_str(&record_history_app_grants_sql());
    script.push_str(&app_schema_sql());
    script.push_str("\nBEGIN;\n");
    script.push_str(
        &platform_principals_sql(TENANT, DOMAIN).expect("example.invalid is a domain name"),
    );
    // The fixture writes as its first admitted person row below.
    writeln!(
        script,
        "COMMIT;\nSET app.user_id = '{FIXTURE_PERSON}';\n\
         SET app.operation = 'admin:seed-platform-row-fixture';"
    )
    .expect("writing to a String cannot fail");
    // Every refused row sits in its own subtransaction, so the fresh rows
    // above stay as the only rows in the table.
    let refused = [
        (
            "not_null_violation",
            format!(
                "INSERT INTO app_system.users (tenant_id, id, email) \
                 VALUES ('{TENANT}', '{PERSON}', 'untyped@{DOMAIN}')"
            ),
        ),
        (
            "check_violation",
            format!(
                "INSERT INTO app_system.users (tenant_id, id, type, email) \
                 VALUES ('{TENANT}', '{PERSON}', 'robot', 'robot@{DOMAIN}')"
            ),
        ),
        (
            "check_violation",
            format!(
                "INSERT INTO app_system.users (tenant_id, id, type, email) \
                 VALUES ('{TENANT}', '{PERSON}', 'platform', 'unnamed@{DOMAIN}')"
            ),
        ),
        (
            "check_violation",
            format!(
                "INSERT INTO app_system.users (tenant_id, id, type, email, display_name) \
                 VALUES ('{TENANT}', '{}', 'platform', 'swapped@{DOMAIN}', '{}')",
                executor.principal_id(),
                provisioning.principal_name(),
            ),
        ),
        (
            "check_violation",
            format!(
                "INSERT INTO app_system.users (tenant_id, id, type, email, display_name) \
                 VALUES ('{TENANT}', '{PERSON}', 'platform', 'unknown@{DOMAIN}', 'wamn:unknown')"
            ),
        ),
    ]
    .into_iter()
    .chain(
        [UserType::Person, UserType::Service]
            .into_iter()
            .flat_map(|user_type| {
                let user_type = user_type.as_str();
                [
                    format!(
                        "INSERT INTO app_system.users (tenant_id, id, type, email, display_name) \
                         VALUES ('{TENANT}', '{PERSON}', '{user_type}', 'named@{DOMAIN}', 'wamn:person')"
                    ),
                    format!(
                        "INSERT INTO app_system.users (tenant_id, id, type, email, display_name) \
                         VALUES ('{TENANT}', '{}', '{user_type}', 'pinned@{DOMAIN}', '{}')",
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
    for (index, user_type) in UserType::ALL.into_iter().enumerate() {
        if user_type == UserType::Platform {
            continue;
        }
        writeln!(
            script,
            "INSERT INTO app_system.users (tenant_id, id, type, email, display_name) \
             VALUES ('other', '00000000-0000-0000-0000-00000000000{index}', '{user_type}', \
                     '{user_type}@{DOMAIN}', 'Fixture {user_type}');"
        )
        .expect("writing to a String cannot fail");
    }
    writeln!(
        script,
        "SELECT display_name, id, type, email FROM app_system.users \
          WHERE tenant_id = '{TENANT}' ORDER BY display_name;\n\
         SELECT count(*) FROM app_system.users WHERE tenant_id = 'other';\n\
         DROP SCHEMA app_system CASCADE;"
    )
    .expect("writing to a String cannot fail");

    let output = run(&url, &script);
    // The provisioning SQL first binds its actor and operation, and psql prints that result.
    let mut expected = vec![format!(
        "{}|{}",
        PlatformComponent::Provisioning.principal_id(),
        PlatformComponent::Provisioning.principal_name()
    )];
    let mut rows = PlatformComponent::ALL
        .map(|component| {
            format!(
                "{}|{}|platform|{}@{DOMAIN}",
                component.principal_name(),
                component.principal_id(),
                component.as_str()
            )
        })
        .to_vec();
    rows.sort();
    expected.extend(rows);
    expected.push("2".to_owned());
    assert_eq!(output.lines().collect::<Vec<_>>(), expected);
}

/// Spec test 11, for the relations that `deploy/sql/app-schema.sql` owns. Every
/// `app_system` relation carries the four stamp columns as `NOT NULL`, one
/// `wamn_record_history_stamp` trigger, one `wamn_record_history_log` trigger with the
/// argument `unlimited`, and its history table. The provisioning SQL binds
/// `wamn:provisioning` for its writes, every platform row stamps that
/// principal, and the `wamn:provisioning` row stamps itself. The binding ends
/// with the provisioning transaction.
#[test]
fn app_system_relations_stamp_provisioning_writes_on_postgres() {
    const TENANT: &str = "t1";
    const DOMAIN: &str = "example.invalid";

    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

    let mut script = sql::ensure_app_acl_role_sql();
    script.push_str("\nDROP SCHEMA IF EXISTS app_system CASCADE;\n");
    script.push_str(&record_history_sql());
    script.push_str(&record_history_app_grants_sql());
    script.push_str(&app_schema_sql());
    // The stamp columns and their types, from the server catalogs.
    script.push_str(
        "\nSELECT 'column|' || c.relname || '|' || a.attname || '|' \
                 || pg_catalog.format_type(a.atttypid, a.atttypmod) || '|' || a.attnotnull \
           FROM pg_catalog.pg_attribute AS a \
           JOIN pg_catalog.pg_class AS c ON c.oid = a.attrelid \
           JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace \
          WHERE n.nspname = 'app_system' AND c.relkind = 'r' \
            AND a.attname IN ('created_at', 'created_by', 'updated_at', 'updated_by') \
            AND a.attnum > 0 AND NOT a.attisdropped \
          ORDER BY c.relname COLLATE \"C\", a.attname;\n\
         SELECT 'relation|' || c.relname \
           FROM pg_catalog.pg_class AS c \
           JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace \
          WHERE n.nspname = 'app_system' AND c.relkind IN ('r', 'p', 'v', 'm', 'f') \
          ORDER BY c.relname COLLATE \"C\";\n\
         SELECT 'trigger|' || pg_catalog.pg_get_triggerdef(t.oid) \
           FROM pg_catalog.pg_trigger AS t \
           JOIN pg_catalog.pg_class AS c ON c.oid = t.tgrelid \
           JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace \
          WHERE n.nspname = 'app_system' AND NOT t.tgisinternal \
          ORDER BY c.relname COLLATE \"C\", t.tgname;\n\
         BEGIN;\n",
    );
    script.push_str(
        &platform_principals_sql(TENANT, DOMAIN).expect("example.invalid is a domain name"),
    );
    writeln!(
        script,
        "COMMIT;\n\
         SELECT 'actor|' || COALESCE(current_setting('app.user_id', true), '');\n\
         SELECT 'stamp|' || display_name || '|' || (id = created_by)::text || '|' \
                || created_by::text || '|' || updated_by::text || '|' \
                || (updated_at >= created_at)::text \
           FROM app_system.users WHERE tenant_id = '{TENANT}' ORDER BY display_name;\n\
         DROP SCHEMA app_system CASCADE;"
    )
    .expect("writing to a String cannot fail");

    let output = run(&url, &script);
    let mut expected = Vec::new();
    let mut names = TABLES.iter().map(|table| table.name).collect::<Vec<_>>();
    names.sort_unstable();
    for name in &names {
        for (column, column_type) in [
            ("created_at", "timestamp with time zone"),
            ("created_by", "uuid"),
            ("updated_at", "timestamp with time zone"),
            ("updated_by", "uuid"),
        ] {
            expected.push(format!("column|{name}|{column}|{column_type}|true"));
        }
    }
    let mut relations = names
        .iter()
        .flat_map(|name| {
            [
                format!("relation|{name}"),
                format!("relation|{name}_history"),
            ]
        })
        .collect::<Vec<_>>();
    relations.sort_unstable();
    expected.extend(relations);
    expected.extend(names.iter().flat_map(|name| {
        [
            format!(
                "trigger|CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE \
                 ON {SCHEMA_NAME}.{name} FOR EACH ROW EXECUTE FUNCTION \
                 wamn_history.log_row_change('unlimited')"
            ),
            format!(
                "trigger|CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE \
                 ON {SCHEMA_NAME}.{name} FOR EACH ROW EXECUTE FUNCTION \
                 wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by')"
            ),
        ]
    }));
    // The provisioning SQL first binds its actor and operation, and psql prints that result.
    let provisioning = PlatformComponent::Provisioning.principal_id();
    expected.push(format!(
        "{provisioning}|{}",
        PlatformComponent::Provisioning.principal_name()
    ));
    expected.push("actor|".to_owned());
    let mut stamps = PlatformComponent::ALL
        .map(|component| {
            format!(
                "stamp|{}|{}|{provisioning}|{provisioning}|true",
                component.principal_name(),
                component == PlatformComponent::Provisioning,
            )
        })
        .to_vec();
    stamps.sort();
    expected.extend(stamps);
    assert_eq!(output.lines().collect::<Vec<_>>(), expected);
}

/// Record history level 2 for the relations that `deploy/sql/app-schema.sql`
/// owns (epic rulings 14, 40 to 43, and 52 to 57). Administrative SQL writes
/// each `app_system` relation in three transactions. Each change writes one
/// entry with the tenant of its row, the bound operation, and the bound actor.
/// An update that changes a key column writes a delete and an insert entry.
/// The user delete cascades to `user_roles` and `api_keys`, and those delete
/// entries carry the actor and the operation of the deleting transaction. The
/// `api_keys` images carry `key_hash` (ruling 41). The server ACLs show the
/// `wamn_app` grant of every relation (ruling 76).
#[test]
fn app_system_relations_log_every_row_change_on_postgres() {
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
    let url = test_database.url().to_owned();

    let mut script = sql::ensure_app_acl_role_sql();
    script.push_str("\nDROP SCHEMA IF EXISTS app_system CASCADE;\n");
    script.push_str(&record_history_sql());
    script.push_str(&record_history_app_grants_sql());
    script.push_str(&app_schema_sql());
    // The wamn_app privileges of every relation, from the server ACLs.
    script.push_str(
        "\nSELECT 'grant|' || c.relname || '|' || concat_ws(', ', \
                (SELECT string_agg(acl.privilege_type, ', ' ORDER BY array_position( \
                            ARRAY['SELECT', 'INSERT', 'UPDATE', 'DELETE'], acl.privilege_type)) \
                   FROM pg_catalog.aclexplode(c.relacl) AS acl \
                  WHERE acl.grantee = 'wamn_app'::regrole), \
                (SELECT string_agg(granted.privilege_type || ' (' || granted.columns || ')', \
                                   ', ' ORDER BY granted.privilege_type) \
                   FROM (SELECT acl.privilege_type, \
                                string_agg(a.attname, ', ' ORDER BY a.attnum) AS columns \
                           FROM pg_catalog.pg_attribute AS a \
                          CROSS JOIN LATERAL pg_catalog.aclexplode(a.attacl) AS acl \
                          WHERE a.attrelid = c.oid AND acl.grantee = 'wamn_app'::regrole \
                          GROUP BY acl.privilege_type) AS granted)) \
           FROM pg_catalog.pg_class AS c \
           JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace \
          WHERE n.nspname = 'app_system' AND c.relkind = 'r' \
          ORDER BY c.relname COLLATE \"C\";\n",
    );
    writeln!(
        script,
        "BEGIN;\n\
         SET LOCAL app.user_id = '{U1}';\n\
         SET LOCAL app.operation = '{SEED}';\n\
         INSERT INTO app_system.users (tenant_id, id, type, email) VALUES \
           ('t1', '{U1}', 'person', 'u1@t1'), ('t2', '{U2}', 'person', 'u2@t2'), \
           ('t1', '{U3}', 'service', 'u3@t1');\n\
         INSERT INTO app_system.roles (tenant_id, name) VALUES ('t1', 'admin'), ('t1', 'auditor');\n\
         INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) VALUES \
           ('t1', '{U1}', 'admin'), ('t1', '{U3}', 'admin');\n\
         INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES \
           ('t1', 'admin', 'receipts:read');\n\
         INSERT INTO app_system.configurations (tenant_id, config_key, config_value) VALUES \
           ('t1', 'theme', '\"dark\"');\n\
         INSERT INTO app_system.api_keys (tenant_id, id, user_id, name, key_hash, prefix) VALUES \
           ('t1', '{K1}', '{U1}', 'ci', 'hash-1', 'wk_a'), ('t1', '{K2}', '{U1}', 'cd', 'hash-2', 'wk_b');\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL app.user_id = '{U2}';\n\
         SET LOCAL app.operation = '{CHANGE}';\n\
         UPDATE app_system.users SET status = 'disabled' WHERE id = '{U2}';\n\
         UPDATE app_system.roles SET description = 'administrators' WHERE name = 'admin';\n\
         UPDATE app_system.user_roles SET role_name = 'auditor' WHERE user_id = '{U3}';\n\
         UPDATE app_system.permissions SET permission = 'receipts:write';\n\
         UPDATE app_system.configurations SET config_value = '\"light\"';\n\
         UPDATE app_system.api_keys SET revoked_at = '2026-09-14T00:00:00Z' WHERE id = '{K1}';\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL app.user_id = '{U3}';\n\
         SET LOCAL app.operation = '{REMOVE}';\n\
         DELETE FROM app_system.user_roles WHERE user_id = '{U3}';\n\
         DELETE FROM app_system.api_keys WHERE id = '{K2}';\n\
         DELETE FROM app_system.permissions;\n\
         DELETE FROM app_system.configurations;\n\
         DELETE FROM app_system.users WHERE id = '{U1}';\n\
         DELETE FROM app_system.users WHERE id = '{U2}';\n\
         DELETE FROM app_system.roles WHERE name = 'admin';\n\
         COMMIT;"
    )
    .expect("writing to a String cannot fail");
    // One line per entry, in relation and position order. The images leave out
    // the stamp columns, whose times differ in each run.
    let entries = TABLES
        .iter()
        .map(|table| {
            format!(
                "SELECT '{name}' AS relation, position, tenant_id, kind, operation, \
                        changed_by, row_key, before, after \
                   FROM {qualified}_history",
                name = table.name,
                qualified = table.qualified(),
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    writeln!(
        script,
        "SELECT 'entry|' || relation || '|' || tenant_id || '|' || kind || '|' || operation \
                || '|' || changed_by || '|' || row_key::text \
                || '|' || (before - '{{created_at,created_by,updated_at,updated_by}}'::text[])::text \
                || '|' || (after - '{{created_at,created_by,updated_at,updated_by}}'::text[])::text \
           FROM ({entries}) AS entry \
          ORDER BY relation COLLATE \"C\", position;\n\
         DROP SCHEMA app_system CASCADE;"
    )
    .expect("writing to a String cannot fail");

    let output = run(&url, &script);
    let mut relations = TABLES
        .iter()
        .flat_map(|table| [table.name.to_owned(), format!("{}_history", table.name)])
        .collect::<Vec<_>>();
    relations.sort_unstable();
    let mut expected = relations
        .iter()
        .map(|relation| format!("grant|{relation}|{}", wamn_app_privileges(relation)))
        .collect::<Vec<_>>();
    // The expected entries name the fixture ids U1, U2, U3, K1, and K2.
    expected.extend(
        r#"entry|api_keys|t1|insert|admin:seed-history-fixture|U1|{"id": "K1", "tenant_id": "t1"}|{}|{"id": "K1", "name": "ci", "prefix": "wk_a", "user_id": "U1", "key_hash": "hash-1", "tenant_id": "t1", "expires_at": null, "revoked_at": null, "last_used_at": null}
entry|api_keys|t1|insert|admin:seed-history-fixture|U1|{"id": "K2", "tenant_id": "t1"}|{}|{"id": "K2", "name": "cd", "prefix": "wk_b", "user_id": "U1", "key_hash": "hash-2", "tenant_id": "t1", "expires_at": null, "revoked_at": null, "last_used_at": null}
entry|api_keys|t1|update|admin:change-history-fixture|U2|{"id": "K1", "tenant_id": "t1"}|{"revoked_at": null}|{"revoked_at": "2026-09-14T00:00:00.000000Z"}
entry|api_keys|t1|delete|admin:remove-history-fixture|U3|{"id": "K2", "tenant_id": "t1"}|{"id": "K2", "name": "cd", "prefix": "wk_b", "user_id": "U1", "key_hash": "hash-2", "tenant_id": "t1", "expires_at": null, "revoked_at": null, "last_used_at": null}|{}
entry|api_keys|t1|delete|admin:remove-history-fixture|U3|{"id": "K1", "tenant_id": "t1"}|{"id": "K1", "name": "ci", "prefix": "wk_a", "user_id": "U1", "key_hash": "hash-1", "tenant_id": "t1", "expires_at": null, "revoked_at": "2026-09-14T00:00:00.000000Z", "last_used_at": null}|{}
entry|configurations|t1|insert|admin:seed-history-fixture|U1|{"tenant_id": "t1", "config_key": "theme"}|{}|{"tenant_id": "t1", "config_key": "theme", "config_value": "dark"}
entry|configurations|t1|update|admin:change-history-fixture|U2|{"tenant_id": "t1", "config_key": "theme"}|{"config_value": "dark"}|{"config_value": "light"}
entry|configurations|t1|delete|admin:remove-history-fixture|U3|{"tenant_id": "t1", "config_key": "theme"}|{"tenant_id": "t1", "config_key": "theme", "config_value": "light"}|{}
entry|permissions|t1|insert|admin:seed-history-fixture|U1|{"role_name": "admin", "tenant_id": "t1", "permission": "receipts:read"}|{}|{"role_name": "admin", "tenant_id": "t1", "permission": "receipts:read"}
entry|permissions|t1|delete|admin:change-history-fixture|U2|{"role_name": "admin", "tenant_id": "t1", "permission": "receipts:read"}|{"role_name": "admin", "tenant_id": "t1", "permission": "receipts:read"}|{}
entry|permissions|t1|insert|admin:change-history-fixture|U2|{"role_name": "admin", "tenant_id": "t1", "permission": "receipts:write"}|{}|{"role_name": "admin", "tenant_id": "t1", "permission": "receipts:write"}
entry|permissions|t1|delete|admin:remove-history-fixture|U3|{"role_name": "admin", "tenant_id": "t1", "permission": "receipts:write"}|{"role_name": "admin", "tenant_id": "t1", "permission": "receipts:write"}|{}
entry|roles|t1|insert|admin:seed-history-fixture|U1|{"name": "admin", "tenant_id": "t1"}|{}|{"name": "admin", "is_system": false, "tenant_id": "t1", "description": null}
entry|roles|t1|insert|admin:seed-history-fixture|U1|{"name": "auditor", "tenant_id": "t1"}|{}|{"name": "auditor", "is_system": false, "tenant_id": "t1", "description": null}
entry|roles|t1|update|admin:change-history-fixture|U2|{"name": "admin", "tenant_id": "t1"}|{"description": null}|{"description": "administrators"}
entry|roles|t1|delete|admin:remove-history-fixture|U3|{"name": "admin", "tenant_id": "t1"}|{"name": "admin", "is_system": false, "tenant_id": "t1", "description": "administrators"}|{}
entry|user_roles|t1|insert|admin:seed-history-fixture|U1|{"user_id": "U1", "role_name": "admin", "tenant_id": "t1"}|{}|{"user_id": "U1", "role_name": "admin", "tenant_id": "t1"}
entry|user_roles|t1|insert|admin:seed-history-fixture|U1|{"user_id": "U3", "role_name": "admin", "tenant_id": "t1"}|{}|{"user_id": "U3", "role_name": "admin", "tenant_id": "t1"}
entry|user_roles|t1|delete|admin:change-history-fixture|U2|{"user_id": "U3", "role_name": "admin", "tenant_id": "t1"}|{"user_id": "U3", "role_name": "admin", "tenant_id": "t1"}|{}
entry|user_roles|t1|insert|admin:change-history-fixture|U2|{"user_id": "U3", "role_name": "auditor", "tenant_id": "t1"}|{}|{"user_id": "U3", "role_name": "auditor", "tenant_id": "t1"}
entry|user_roles|t1|delete|admin:remove-history-fixture|U3|{"user_id": "U3", "role_name": "auditor", "tenant_id": "t1"}|{"user_id": "U3", "role_name": "auditor", "tenant_id": "t1"}|{}
entry|user_roles|t1|delete|admin:remove-history-fixture|U3|{"user_id": "U1", "role_name": "admin", "tenant_id": "t1"}|{"user_id": "U1", "role_name": "admin", "tenant_id": "t1"}|{}
entry|users|t1|insert|admin:seed-history-fixture|U1|{"id": "U1", "tenant_id": "t1"}|{}|{"id": "U1", "type": "person", "email": "u1@t1", "status": "active", "tenant_id": "t1", "display_name": null}
entry|users|t2|insert|admin:seed-history-fixture|U1|{"id": "U2", "tenant_id": "t2"}|{}|{"id": "U2", "type": "person", "email": "u2@t2", "status": "active", "tenant_id": "t2", "display_name": null}
entry|users|t1|insert|admin:seed-history-fixture|U1|{"id": "U3", "tenant_id": "t1"}|{}|{"id": "U3", "type": "service", "email": "u3@t1", "status": "active", "tenant_id": "t1", "display_name": null}
entry|users|t2|update|admin:change-history-fixture|U2|{"id": "U2", "tenant_id": "t2"}|{"status": "active"}|{"status": "disabled"}
entry|users|t1|delete|admin:remove-history-fixture|U3|{"id": "U1", "tenant_id": "t1"}|{"id": "U1", "type": "person", "email": "u1@t1", "status": "active", "tenant_id": "t1", "display_name": null}|{}
entry|users|t2|delete|admin:remove-history-fixture|U3|{"id": "U2", "tenant_id": "t2"}|{"id": "U2", "type": "person", "email": "u2@t2", "status": "disabled", "tenant_id": "t2", "display_name": null}|{}"#
            .lines()
            .map(str::to_owned),
    );
    let output = [("U1", U1), ("U2", U2), ("U3", U3), ("K1", K1), ("K2", K2)]
        .into_iter()
        .fold(output, |output, (name, id)| output.replace(id, name));
    assert_eq!(output.lines().collect::<Vec<_>>(), expected);
}

/// Record history spec test 19. `wamn_history.row_image` spells each
/// `timestamptz` exactly as the platform canonicalizer spells the value that
/// PostgreSQL holds. The session time zone is not UTC, and the values have
/// zero and non-zero microseconds.
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

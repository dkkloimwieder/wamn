//! Storage-schema tests for the per-project system schema v1 (wamn-as5).
//!
//! Two layers (the `wamn-control-registry` / `deploy/sql/system-schema.sql` precedent):
//! - a **drift guard** tying `deploy/sql/app-schema.sql` to the `wamn-project-state`
//!   model (the schema name, each table + its pinned columns, the RLS floor +
//!   a45 empty-tenant-row hardening, the `users.status` CHECK literals from
//!   `UserStatus::as_str`, and the FK cascades);
//! - a **live-apply gate** showing the DB-enforced behavior — tenant RLS
//!   isolation, the FK cascades, the empty-tenant / status / type CHECKs, and
//!   the platform principal rows — gated on
//!   `WAMN_SYSSCHEMA_PG_URL` (a superuser URL; the harness prepares App generations)
//!   and skipped cleanly when unset.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, platform_principals_sql, sql,
    workload_generation_role,
};
use wamn_project_state::{PlatformComponent, SCHEMA_NAME, TABLES, UserStatus, UserType};

const APP_GENERATION_PASSWORD: &str = "test-owned-app-generation-password";
const APP_GENERATION_VALID_UNTIL: &str = "2099-01-01T00:00:00Z";

/// Each live test rebuilds `app_system` in the target database, so they take
/// turns (cargo runs the tests in one binary on parallel threads).
static LIVE_DB: Mutex<()> = Mutex::new(());

fn deploy_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy")
}

fn app_schema_sql() -> String {
    std::fs::read_to_string(deploy_dir().join("sql/app-schema.sql"))
        .expect("read deploy/sql/app-schema.sql")
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
///
/// A table the model gains later has no adjudicated class, so this panics rather
/// than silently defaulting it into one.
fn wamn_app_privileges(table: &str) -> &'static str {
    match table {
        "users" | "roles" | "user_roles" | "permissions" | "api_keys" => "SELECT",
        "configurations" => "SELECT, INSERT, UPDATE, DELETE",
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
    // sequential-scans every relation. One per table, same count as the CHECKs.
    let indexes = sql
        .matches("((wamn_authority.tenant_key(tenant_id)))")
        .count();
    assert_eq!(
        indexes,
        TABLES.len(),
        "every table must carry its tenant-key expression index — one per table"
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

/// Apply `deploy/sql/app-schema.sql` to a throwaway Postgres and assert the live,
/// DB-enforced behavior. Set `WAMN_SYSSCHEMA_PG_URL` to a superuser URL (the
/// harness prepares tenant-scoped App generations); skipped when unset.
#[test]
fn app_schema_applies_and_enforces_isolation_on_postgres() {
    let Ok(url) = std::env::var("WAMN_SYSSCHEMA_PG_URL") else {
        eprintln!(
            "skipping app_schema_applies_and_enforces_isolation_and_claims_on_postgres \
             (set WAMN_SYSSCHEMA_PG_URL to run)"
        );
        return;
    };
    let _live = LIVE_DB.lock().unwrap_or_else(PoisonError::into_inner);

    const U1: &str = "11111111-1111-1111-1111-111111111111";
    const U2: &str = "22222222-2222-2222-2222-222222222222";
    const U3: &str = "33333333-3333-3333-3333-333333333333";
    const U4: &str = "44444444-4444-4444-4444-444444444444";
    const TENANT_1: &str = "t1";
    const TENANT_2: &str = "t2";

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
    script.push_str(&app_schema_sql());
    script.push('\n');
    // Seed as the superuser (bypasses RLS): two tenants for the isolation test,
    // known user ids to tie the docs rows to. U1 has a role, key, and config.
    script.push_str(&format!(
        "INSERT INTO app_system.users (tenant_id, id, type, email) VALUES \
           ('t1','{U1}','person','u1@t1'),('t1','{U2}','service','u2@t1'),('t2','{U3}','person','u3@t2');\n\
         INSERT INTO app_system.roles (tenant_id, name, is_system) VALUES ('t1','admin',true);\n\
         INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) VALUES ('t1','{U1}','admin');\n\
         INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES ('t1','admin','receipts:read');\n\
         INSERT INTO app_system.api_keys (tenant_id, user_id, name, key_hash, prefix) VALUES ('t1','{U1}','ci','hash-1','wk_a');\n\
         INSERT INTO app_system.configurations (tenant_id, config_key, config_value) VALUES ('t1','theme','\"dark\"'::jsonb);\n"
    ));

    // Tenant isolation follows current_user's prepared scope: tenant 1 sees only
    // tenant 1's rows without any settable tenant claim.
    script.push_str(&format!(
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
         COMMIT;\n"
    ));
    // Tenant 2 sees only its row. Spoofing the retired app.tenant claim does not
    // move tenant 1, and the stable ACL carrier itself maps to no tenant.
    script.push_str(&format!(
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
         COMMIT;\n"
    ));
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
    script.push_str(&format!(
        "DO $$ BEGIN BEGIN\n\
           INSERT INTO app_system.users (tenant_id, id, type, email, status) VALUES ('t1','{U3}','person','x@t1','zombie');\n\
           ASSERT false, 'an unknown user status must be rejected';\n\
         EXCEPTION WHEN check_violation THEN NULL; END; END $$;\n\
         DO $$ BEGIN BEGIN\n\
           INSERT INTO app_system.users (tenant_id, id, type, email) VALUES ('','{U4}','person','x@none');\n\
           ASSERT false, 'a ''-tenant row must be rejected (a45)';\n\
         EXCEPTION WHEN check_violation THEN NULL; END; END $$;\n"
    ));
    // FK cascade: deleting U1 prunes its role grant and api key.
    script.push_str(&format!(
        "DELETE FROM app_system.users WHERE tenant_id='t1' AND id='{U1}';\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM app_system.user_roles WHERE user_id='{U1}')=0, 'user_roles cascade';\n\
           ASSERT (SELECT count(*) FROM app_system.api_keys WHERE user_id='{U1}')=0, 'api_keys cascade';\n\
         END $$;\n"
    ));

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
/// person or service row with a `wamn:` name. Set `WAMN_SYSSCHEMA_PG_URL` to a
/// superuser URL. Skipped when unset.
#[test]
fn platform_rows_carry_their_pinned_ids_on_postgres() {
    const TENANT: &str = "t1";
    const DOMAIN: &str = "example.invalid";
    const PERSON: &str = "11111111-1111-1111-1111-111111111111";

    let Ok(url) = std::env::var("WAMN_SYSSCHEMA_PG_URL") else {
        eprintln!(
            "skipping platform_rows_carry_their_pinned_ids_on_postgres \
             (set WAMN_SYSSCHEMA_PG_URL to run)"
        );
        return;
    };
    let _live = LIVE_DB.lock().unwrap_or_else(PoisonError::into_inner);

    let provisioning = PlatformComponent::Provisioning;
    let executor = PlatformComponent::Executor;
    let mut script = sql::ensure_app_acl_role_sql();
    script.push_str("\nDROP SCHEMA IF EXISTS app_system CASCADE;\n");
    script.push_str(&app_schema_sql());
    script.push('\n');
    script.push_str(
        &platform_principals_sql(TENANT, DOMAIN).expect("example.invalid is a domain name"),
    );
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
        script.push_str(&format!(
            "DO $$ BEGIN BEGIN\n\
               EXECUTE '{quoted}';\n\
               RAISE EXCEPTION 'the users CHECKs admitted: %', '{quoted}';\n\
             EXCEPTION WHEN {condition} THEN NULL; END; END $$;\n"
        ));
    }
    // Every user type is admitted.
    for (index, user_type) in UserType::ALL.into_iter().enumerate() {
        if user_type == UserType::Platform {
            continue;
        }
        script.push_str(&format!(
            "INSERT INTO app_system.users (tenant_id, id, type, email, display_name) \
             VALUES ('other', '00000000-0000-0000-0000-00000000000{index}', '{user_type}', \
                     '{user_type}@{DOMAIN}', 'Fixture {user_type}');\n"
        ));
    }
    script.push_str(&format!(
        "SELECT display_name, id, type, email FROM app_system.users \
          WHERE tenant_id = '{TENANT}' ORDER BY display_name;\n\
         SELECT count(*) FROM app_system.users WHERE tenant_id = 'other';\n\
         DROP SCHEMA app_system CASCADE;\n"
    ));

    let output = run(&url, &script);
    let mut expected = PlatformComponent::ALL
        .map(|component| {
            format!(
                "{}|{}|platform|{}@{DOMAIN}",
                component.principal_name(),
                component.principal_id(),
                component.as_str()
            )
        })
        .to_vec();
    expected.sort();
    expected.push("2".to_owned());
    assert_eq!(output.lines().collect::<Vec<_>>(), expected);
}

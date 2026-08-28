//! Storage-schema tests for the per-project system schema v1 (wamn-as5).
//!
//! Two layers (the `wamn-control-registry` / `deploy/sql/system-schema.sql` precedent):
//! - a **drift guard** tying `deploy/sql/app-schema.sql` to the `wamn-project-state`
//!   model (the schema name, each table + its pinned columns, the RLS floor +
//!   a45 empty-tenant-row hardening, the `users.status` CHECK literals from
//!   `UserStatus::as_str`, and the FK cascades);
//! - a **live-apply gate** proving the DB-enforced behavior — tenant RLS
//!   isolation, the FK cascades (and audit-log immutability), the empty-tenant /
//!   status CHECKs, and that `users.id` (uuid) + `roles.name` (text) are the
//!   right targets for a REAL compiled 3.5 RLS policy — gated on
//!   `WAMN_SYSSCHEMA_PG_URL` (a superuser URL; the harness prepares App generations)
//!   and skipped cleanly when unset (mirrors wamn-schema-compiler / wamn-schema-compiler / wamn-control-registry).

use std::path::Path;

use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_project_state::{SCHEMA_NAME, TABLES, UserStatus};

const APP_GENERATION_PASSWORD: &str = "test-owned-app-generation-password";
const APP_GENERATION_VALID_UNTIL: &str = "2099-01-01T00:00:00Z";

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
/// - `audit_log` is append-only against the audited party: `SELECT` + `INSERT`,
///   no `UPDATE`/`DELETE`. The grant is the whole enforcement mechanism.
/// - `configurations` is tenant business state — nothing in the trust chain reads
///   it — so it deliberately keeps full DML.
///
/// A table the model gains later has no adjudicated class, so this panics rather
/// than silently defaulting it into one.
fn wamn_app_privileges(table: &str) -> &'static str {
    match table {
        "users" | "roles" | "user_roles" | "permissions" | "api_keys" => "SELECT",
        "audit_log" => "SELECT, INSERT",
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
/// drift-guarded like the registry's tier/env literals. `users.id` is the
/// app.user_id ownership target (declared uuid; its type is proven live).
#[test]
fn user_status_literals_and_ownership_target_are_pinned() {
    let sql = code_only(&app_schema_sql());
    assert!(sql.contains("users_status_check"));
    for s in UserStatus::ALL {
        assert!(
            sql.contains(&format!("'{}'", s.as_str())),
            "app-schema.sql is missing the users.status literal {:?}",
            s.as_str()
        );
    }
    // users.id is declared uuid (the ownership target the 3.5 builder casts to).
    assert!(
        sql.contains("id           uuid NOT NULL DEFAULT gen_random_uuid()"),
        "users.id must be a uuid (the app.user_id ownership target)"
    );
}

/// The FK cascades that keep the graph consistent are pinned: the user↔role
/// linkage and api_keys reference users ON DELETE CASCADE; permissions and the
/// linkage reference roles ON DELETE CASCADE. (audit_log deliberately does NOT
/// FK actor_id — immutable history survives user deletion; proven live.)
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
    // audit_log must NOT FK actor_id (immutable history survives user deletion) —
    // a real audit FK would introduce a `FOREIGN KEY (tenant_id, actor_id)` clause.
    assert!(
        !sql.contains("FOREIGN KEY (tenant_id, actor_id)"),
        "audit_log.actor_id must NOT be FK'd — the audit trail is immutable"
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
fn app_schema_applies_and_enforces_isolation_and_claims_on_postgres() {
    let Ok(url) = std::env::var("WAMN_SYSSCHEMA_PG_URL") else {
        eprintln!(
            "skipping app_schema_applies_and_enforces_isolation_and_claims_on_postgres \
             (set WAMN_SYSSCHEMA_PG_URL to run)"
        );
        return;
    };

    // A minimal single-entity catalog with a uuid owner column (no FKs — seeds
    // cleanly). Its owner uuids ARE app_system.users ids, so the compiled 3.5
    // policy proves users.id / roles.name are the right claim targets.
    let catalog = notes_catalog();
    let floor = wamn_schema_compiler::Migration::create(&catalog).unwrap();
    let policy = wamn_schema_compiler::rls::AccessPolicy {
        schema_version: "0.1".into(),
        catalog_id: "docs".into(),
        rules: vec![wamn_schema_compiler::rls::Rule::RowOwnership {
            entity: "docs".into(),
            owner_field: "owner_id".into(),
            exempt_roles: vec!["admin".into()],
            name: None,
        }],
    };
    let policies = wamn_schema_compiler::rls::compile(&policy, &catalog).unwrap();

    const U1: &str = "11111111-1111-1111-1111-111111111111";
    const U2: &str = "22222222-2222-2222-2222-222222222222";
    const U3: &str = "33333333-3333-3333-3333-333333333333";
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
    // The data table (3.2 floor) + its compiled 3.5 ownership policy, in a test
    // schema. owner_id (uuid) rows will be owned by app_system.users ids.
    script.push_str(
        "CREATE SCHEMA wamn_sysschema_test AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA wamn_sysschema_test TO wamn_app;\n\
         SET search_path TO wamn_sysschema_test;\n",
    );
    script.push_str(&floor.sql().unwrap());
    script.push_str(&policies.sql().unwrap());
    script.push_str("\nRESET search_path;\n");

    // Seed as the superuser (bypasses RLS): two tenants for the isolation proof,
    // known user ids to tie the docs rows to. U1 has a role, key, config, and two
    // audit entries; the docs rows are owned by U1 and U2.
    script.push_str(&format!(
        "INSERT INTO app_system.users (tenant_id, id, email) VALUES \
           ('t1','{U1}','u1@t1'),('t1','{U2}','u2@t1'),('t2','{U3}','u3@t2');\n\
         INSERT INTO app_system.roles (tenant_id, name, is_system) VALUES ('t1','admin',true);\n\
         INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) VALUES ('t1','{U1}','admin');\n\
         INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES ('t1','admin','receipts:read');\n\
         INSERT INTO app_system.api_keys (tenant_id, user_id, name, key_hash, prefix) VALUES ('t1','{U1}','ci','hash-1','wk_a');\n\
         INSERT INTO app_system.configurations (tenant_id, config_key, config_value) VALUES ('t1','theme','\"dark\"'::jsonb);\n\
         INSERT INTO app_system.audit_log (tenant_id, actor_id, action) VALUES ('t1','{U1}','user.login'),('t1','{U1}','receipt.create');\n\
         INSERT INTO wamn_sysschema_test.docs (tenant_id, owner_id, body) VALUES ('t1','{U1}','a'),('t1','{U2}','b');\n"
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
           ASSERT (SELECT count(*) FROM app_system.audit_log)=2, 't1 sees its 2 audit rows';\n\
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
    // Claim integration: the compiled 3.5 ownership policy filters the data table
    // by app.user_id (= a users.id) and honors the exempt role (= a roles.name).
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE {tenant_1_app};\n\
         SET LOCAL search_path TO wamn_sysschema_test;\n\
         SET LOCAL app.role = 'inspector';\n\
         SET LOCAL app.user_id = '{U1}';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM docs)=1, 'app.user_id (=a users.id) sees only its own row'; END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE {tenant_1_app};\n\
         SET LOCAL search_path TO wamn_sysschema_test;\n\
         SET LOCAL app.role = 'admin';\n\
         SET LOCAL app.user_id = '{U2}';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM docs)=2, 'app.role admin (a roles.name) is exempt — sees all'; END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE {tenant_1_app};\n\
         SET LOCAL search_path TO wamn_sysschema_test;\n\
         SET LOCAL app.role = 'inspector';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM docs)=0, 'no app.user_id claim denies ownership'; END $$;\n\
         COMMIT;\n"
    ));

    // users.id is uuid (the ownership target's type — pinned mechanically here so
    // a uuid→text mutation is caught even though the docs owner column is separate).
    script.push_str(
        "DO $$ DECLARE t text; BEGIN\n\
           SELECT data_type INTO t FROM information_schema.columns\n\
             WHERE table_schema='app_system' AND table_name='users' AND column_name='id';\n\
           ASSERT t='uuid', 'users.id must be uuid (the app.user_id ownership target)';\n\
         END $$;\n",
    );
    // The status / empty-tenant CHECKs reject bad rows.
    script.push_str(&format!(
        "DO $$ BEGIN BEGIN\n\
           INSERT INTO app_system.users (tenant_id, id, email, status) VALUES ('t1','{U3}','x@t1','zombie');\n\
           ASSERT false, 'an unknown user status must be rejected';\n\
         EXCEPTION WHEN check_violation THEN NULL; END; END $$;\n\
         DO $$ BEGIN BEGIN\n\
           INSERT INTO app_system.users (tenant_id, email) VALUES ('','x@none');\n\
           ASSERT false, 'a ''-tenant row must be rejected (a45)';\n\
         EXCEPTION WHEN check_violation THEN NULL; END; END $$;\n"
    ));
    // FK cascade + audit immutability: deleting U1 prunes its role grant and api
    // key, but its audit rows SURVIVE (actor_id is not FK'd — immutable history).
    script.push_str(&format!(
        "DELETE FROM app_system.users WHERE tenant_id='t1' AND id='{U1}';\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM app_system.user_roles WHERE user_id='{U1}')=0, 'user_roles cascade';\n\
           ASSERT (SELECT count(*) FROM app_system.api_keys WHERE user_id='{U1}')=0, 'api_keys cascade';\n\
           ASSERT (SELECT count(*) FROM app_system.audit_log WHERE actor_id='{U1}')=2, 'audit_log survives user deletion (immutable)';\n\
         END $$;\n"
    ));

    script.push_str("DROP SCHEMA app_system CASCADE;\n");
    script.push_str("DROP SCHEMA wamn_sysschema_test CASCADE;\n");

    use std::io::Write;
    use std::process::{Command as Proc, Stdio};
    let mut child = Proc::new("psql")
        .arg(&url)
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

/// A minimal single-entity catalog with a uuid owner column and no foreign keys
/// (the wamn-schema-compiler live-apply precedent) — owner uuids are `app_system.users` ids.
fn notes_catalog() -> wamn_schema_model::Catalog {
    use wamn_schema_model::{Catalog, Entity, Field, FieldType};
    let f = |id: &str, ty: FieldType, nullable: bool| Field {
        id: id.into(),
        name: id.into(),
        field_type: ty,
        nullable,
        default: None,
        sensitive: false,
        is_system: false,
        label: None,
        description: None,
    };
    Catalog {
        schema_version: "0.1".into(),
        catalog_id: "docs".into(),
        version: 1,
        name: None,
        entities: vec![Entity {
            id: "docs".into(),
            name: "docs".into(),
            is_system: false,
            label: None,
            description: None,
            fields: vec![
                f("owner_id", FieldType::Uuid, false),
                f("body", FieldType::Text { max_len: None }, true),
            ],
            indexes: vec![],
            constraints: vec![],
        }],
        relations: vec![],
    }
}

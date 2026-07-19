//! Storage-schema tests for the per-project system schema v1 (wamn-as5).
//!
//! Two layers (the `wamn-registry` / `deploy/sql/system-schema.sql` precedent):
//! - a **drift guard** tying `deploy/sql/app-schema.sql` to the `wamn-sysschema`
//!   model (the schema name, each table + its pinned columns, the RLS floor +
//!   a45 empty-claim hardening, the `users.status` CHECK literals from
//!   `UserStatus::as_str`, and the FK cascades);
//! - a **live-apply gate** proving the DB-enforced behavior — tenant RLS
//!   isolation, the FK cascades (and audit-log immutability), the empty-claim /
//!   status CHECKs, and that `users.id` (uuid) + `roles.name` (text) are the
//!   right targets for a REAL compiled 3.5 RLS policy — gated on
//!   `WAMN_SYSSCHEMA_PG_URL` (a superuser URL; the harness provisions `wamn_app`)
//!   and skipped cleanly when unset (mirrors wamn-ddl / wamn-rls / wamn-registry).

use std::path::Path;

use wamn_sysschema::{SCHEMA_NAME, TABLES, UserStatus};

fn deploy_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy")
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

/// `deploy/sql/app-schema.sql` must mirror the `wamn-sysschema` model: the schema
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
        // Every table carries the RLS floor: a tenant policy, FORCE RLS, grants.
        assert!(
            sql.contains(&format!("CREATE POLICY {}_tenant ON {qualified}", t.name)),
            "table {qualified} is missing its tenant RLS policy"
        );
        assert!(
            sql.contains(&format!("ALTER TABLE {qualified} FORCE ROW LEVEL SECURITY")),
            "table {qualified} must FORCE row level security"
        );
        assert!(
            sql.contains(&format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON {qualified} TO wamn_app"
            )),
            "table {qualified} is missing its wamn_app grant"
        );
    }
}

/// The tenant floor is the a45-hardened shape: the read is NULLIF-wrapped (empty
/// claim ⇒ NULL ⇒ no match) and every table forbids a ''-tenant row. Pinned by
/// expression, not just presence (the drift-guard lesson).
#[test]
fn tenant_floor_is_the_hardened_shape() {
    let sql = code_only(&app_schema_sql());
    assert!(
        sql.contains("NULLIF(current_setting('app.tenant', true), '')"),
        "the tenant read must be NULLIF-wrapped (a45 empty-claim hardening)"
    );
    // Every table forbids a ''-tenant row (one CHECK per table).
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

/// Apply `deploy/sql/app-schema.sql` to a throwaway Postgres and assert the live,
/// DB-enforced behavior. Set `WAMN_SYSSCHEMA_PG_URL` to a superuser URL (the
/// harness provisions `wamn_app`); skipped when unset.
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
    let floor = wamn_ddl::Migration::create(&catalog).unwrap();
    let policy = wamn_rls::AccessPolicy {
        schema_version: "0.1".into(),
        catalog_id: "docs".into(),
        rules: vec![wamn_rls::Rule::RowOwnership {
            entity: "docs".into(),
            owner_field: "owner_id".into(),
            exempt_roles: vec!["admin".into()],
            name: None,
        }],
    };
    let policies = wamn_rls::compile(&policy, &catalog).unwrap();

    const U1: &str = "11111111-1111-1111-1111-111111111111";
    const U2: &str = "22222222-2222-2222-2222-222222222222";
    const U3: &str = "33333333-3333-3333-3333-333333333333";

    let mut script = String::new();
    // Provision wamn_app (NOSUPERUSER, no BYPASSRLS — as in production) and a
    // fresh app_system + a test schema for the data table.
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS app_system CASCADE;\n\
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
    script.push_str(&floor.sql(wamn_ddl::Confirmation::None).unwrap());
    script.push_str(&policies.sql(wamn_ddl::Confirmation::None).unwrap());
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

    // Tenant isolation as wamn_app under app.tenant='t1': sees only t1's rows.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL app.tenant = 't1';\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM app_system.users)=2, 't1 sees its 2 users, not t2''s';\n\
           ASSERT (SELECT count(*) FROM app_system.roles)=1, 't1 sees its role';\n\
           ASSERT (SELECT count(*) FROM app_system.user_roles)=1, 't1 sees its grant';\n\
           ASSERT (SELECT count(*) FROM app_system.permissions)=1, 't1 sees its permission';\n\
           ASSERT (SELECT count(*) FROM app_system.api_keys)=1, 't1 sees its api key';\n\
           ASSERT (SELECT count(*) FROM app_system.configurations)=1, 't1 sees its config';\n\
           ASSERT (SELECT count(*) FROM app_system.audit_log)=2, 't1 sees its 2 audit rows';\n\
         END $$;\n\
         COMMIT;\n",
    );
    // The other tenant sees only ITS row; an empty claim sees nothing (a45).
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL app.tenant = 't2';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM app_system.users)=1, 't2 sees only its user'; END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL app.tenant = '';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM app_system.users)=0, 'an empty tenant claim sees nothing'; END $$;\n\
         COMMIT;\n",
    );
    // Claim integration: the compiled 3.5 ownership policy filters the data table
    // by app.user_id (= a users.id) and honors the exempt role (= a roles.name).
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_sysschema_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         SET LOCAL app.role = 'inspector';\n\
         SET LOCAL app.user_id = '{U1}';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM docs)=1, 'app.user_id (=a users.id) sees only its own row'; END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_sysschema_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         SET LOCAL app.role = 'admin';\n\
         SET LOCAL app.user_id = '{U2}';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM docs)=2, 'app.role admin (a roles.name) is exempt — sees all'; END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_sysschema_test;\n\
         SET LOCAL app.tenant = 't1';\n\
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
/// (the wamn-rls live-apply precedent) — owner uuids are `app_system.users` ids.
fn notes_catalog() -> wamn_catalog::Catalog {
    use wamn_catalog::{Catalog, Entity, Field, FieldType};
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

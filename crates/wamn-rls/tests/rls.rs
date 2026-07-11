//! RLS policy-builder tests. Deterministic emission + validation over the POC
//! catalog (reused from wamn-catalog's fixtures), plus an optional live-apply
//! test that applies the 3.2 tenant floor + the compiled policies to a throwaway
//! Postgres and asserts the restrictive policy actually filters rows.

use std::path::{Path, PathBuf};

use wamn_catalog::{Catalog, Entity, Field, FieldType};
use wamn_rls::{AccessPolicy, Command, CommandGrant, CompileError, Confirmation, Rule, compile};

fn poc_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../wamn-catalog/tests/fixtures/poc-receiving.catalog.json")
}

fn poc() -> Catalog {
    let raw = std::fs::read_to_string(poc_fixture()).expect("read POC fixture");
    Catalog::from_json(&raw).expect("POC fixture parses")
}

fn policy(rules: Vec<Rule>) -> AccessPolicy {
    AccessPolicy {
        schema_version: "0.1".into(),
        catalog_id: "poc-material-receiving".into(),
        rules,
    }
}

// --- emission --------------------------------------------------------------

#[test]
fn row_ownership_compiles_to_restrictive_owner_policy() {
    let p = policy(vec![Rule::RowOwnership {
        entity: "dispositions".into(),
        owner_field: "inspector_id".into(),
        exempt_roles: vec!["supervisor".into(), "admin".into()],
        name: None,
    }]);
    let plan = compile(&p, &poc()).expect("compiles");
    assert!(plan.is_additive());
    assert!(!plan.requires_confirmation());
    let op = &plan.operations[0];
    assert_eq!(op.entity, "dispositions");
    assert_eq!(op.field.as_deref(), Some("inspector_id"));

    let sql = &op.sql;
    assert!(
        sql.contains("CREATE POLICY \"dispositions_owner_0\" ON \"dispositions\" AS RESTRICTIVE")
    );
    assert!(sql.contains("FOR ALL"));
    // Ownership check with the safe uuid coercion.
    assert!(
        sql.contains("\"inspector_id\" = NULLIF(current_setting('app.user_id', true), '')::uuid")
    );
    // Exempt roles bypass, via the coalesced role claim.
    assert!(
        sql.contains("COALESCE(current_setting('app.role', true), '') IN ('supervisor', 'admin')")
    );
    // FOR ALL has both USING and WITH CHECK.
    assert!(sql.contains("USING (") && sql.contains("WITH CHECK ("));
}

#[test]
fn role_command_gates_emit_per_command_policies() {
    let p = policy(vec![Rule::RoleCommands {
        entity: "dispositions".into(),
        grants: vec![
            CommandGrant {
                command: Command::Insert,
                roles: vec!["inspector".into(), "supervisor".into()],
            },
            CommandGrant {
                command: Command::Delete,
                roles: vec!["admin".into()],
            },
        ],
        name: None,
    }]);
    let plan = compile(&p, &poc()).expect("compiles");
    assert_eq!(plan.operations.len(), 2);

    let insert = plan
        .operations
        .iter()
        .find(|o| o.sql.contains("FOR INSERT"))
        .expect("insert policy");
    // INSERT gates via WITH CHECK only (no USING on an insert policy).
    assert!(insert.sql.contains("WITH CHECK ("));
    assert!(!insert.sql.contains("USING ("));
    assert!(insert.sql.contains(
        "COALESCE(current_setting('app.role', true), '') IN ('inspector', 'supervisor')"
    ));

    let delete = plan
        .operations
        .iter()
        .find(|o| o.sql.contains("FOR DELETE"))
        .expect("delete policy");
    // DELETE gates via USING only.
    assert!(delete.sql.contains("USING ("));
    assert!(!delete.sql.contains("WITH CHECK ("));
    assert!(delete.sql.contains("IN ('admin')"));
}

#[test]
fn custom_role_predicate_is_emitted_verbatim_and_role_scoped() {
    let p = policy(vec![Rule::RolePredicate {
        entity: "quality_holds".into(),
        role: "inspector".into(),
        command: Command::Select,
        expression: "site_id = NULLIF(current_setting('app.site', true), '')::uuid".into(),
        name: None,
    }]);
    let plan = compile(&p, &poc()).expect("compiles");
    let sql = &plan.operations[0].sql;
    assert!(sql.contains("FOR SELECT"));
    // Only this role is constrained; others are unaffected (role <> 'inspector' OR …).
    assert!(sql.contains(
        "COALESCE(current_setting('app.role', true), '') <> 'inspector' OR (site_id = NULLIF(current_setting('app.site', true), '')::uuid)"
    ));
    // SELECT is a USING-only policy.
    assert!(sql.contains("USING (") && !sql.contains("WITH CHECK ("));
}

#[test]
fn plan_is_additive_and_gate_free_but_notes_the_claim_dependency() {
    let p = policy(vec![Rule::RowOwnership {
        entity: "dispositions".into(),
        owner_field: "inspector_id".into(),
        exempt_roles: vec![],
        name: None,
    }]);
    let plan = compile(&p, &poc()).expect("compiles");
    assert!(plan.sql(Confirmation::None).is_ok());
    let report = plan.report();
    assert!(report.contains("additive"));
    assert!(report.contains("app.role / app.user_id"));
}

#[test]
fn explicit_name_is_used_and_suffixed_per_command() {
    let p = policy(vec![Rule::RoleCommands {
        entity: "dispositions".into(),
        grants: vec![CommandGrant {
            command: Command::Update,
            roles: vec!["supervisor".into()],
        }],
        name: Some("disp_write".into()),
    }]);
    let plan = compile(&p, &poc()).expect("compiles");
    // UPDATE gets both USING and WITH CHECK; the name is the explicit base + cmd.
    let sql = &plan.operations[0].sql;
    assert!(sql.contains("CREATE POLICY \"disp_write_update\""));
    assert!(sql.contains("USING (") && sql.contains("WITH CHECK ("));
}

// --- validation ------------------------------------------------------------

#[test]
fn unknown_entity_is_rejected() {
    let p = policy(vec![Rule::RowOwnership {
        entity: "nope".into(),
        owner_field: "x".into(),
        exempt_roles: vec![],
        name: None,
    }]);
    match compile(&p, &poc()) {
        Err(CompileError::InvalidPolicy(issues)) => {
            assert!(issues.iter().any(|i| i.code == "unknown-entity"))
        }
        other => panic!("expected InvalidPolicy, got {other:?}"),
    }
}

#[test]
fn owner_field_must_be_uuid() {
    // quality_holds.status is an enum, not a uuid/reference.
    let p = policy(vec![Rule::RowOwnership {
        entity: "quality_holds".into(),
        owner_field: "status".into(),
        exempt_roles: vec![],
        name: None,
    }]);
    match compile(&p, &poc()) {
        Err(CompileError::InvalidPolicy(issues)) => {
            assert!(issues.iter().any(|i| i.code == "owner-field-not-uuid"))
        }
        other => panic!("expected InvalidPolicy, got {other:?}"),
    }
}

#[test]
fn empty_predicate_and_mismatched_catalog_are_rejected() {
    let mut p = policy(vec![Rule::RolePredicate {
        entity: "quality_holds".into(),
        role: "inspector".into(),
        command: Command::All,
        expression: "   ".into(),
        name: None,
    }]);
    p.catalog_id = "other".into();
    match compile(&p, &poc()) {
        Err(CompileError::InvalidPolicy(issues)) => {
            assert!(issues.iter().any(|i| i.code == "empty-expression"));
            assert!(issues.iter().any(|i| i.code == "catalog-id-mismatch"));
        }
        other => panic!("expected InvalidPolicy, got {other:?}"),
    }
}

#[test]
fn duplicate_explicit_name_and_empty_grant_roles_are_rejected() {
    let p = policy(vec![
        Rule::RolePredicate {
            entity: "quality_holds".into(),
            role: "inspector".into(),
            command: Command::All,
            expression: "true".into(),
            name: Some("dup".into()),
        },
        Rule::RoleCommands {
            entity: "dispositions".into(),
            grants: vec![CommandGrant {
                command: Command::Insert,
                roles: vec![],
            }],
            name: Some("dup".into()),
        },
    ]);
    match compile(&p, &poc()) {
        Err(CompileError::InvalidPolicy(issues)) => {
            assert!(issues.iter().any(|i| i.code == "duplicate-policy-name"));
            assert!(issues.iter().any(|i| i.code == "empty-grant-roles"));
        }
        other => panic!("expected InvalidPolicy, got {other:?}"),
    }
}

// --- model round-trip ------------------------------------------------------

#[test]
fn json_round_trips_and_defaults_command_to_all() {
    let p = policy(vec![Rule::RowOwnership {
        entity: "dispositions".into(),
        owner_field: "inspector_id".into(),
        exempt_roles: vec!["admin".into()],
        name: None,
    }]);
    let back = AccessPolicy::from_json(&p.to_json()).expect("round-trips");
    assert_eq!(p, back);

    // A role predicate without an explicit command defaults to ALL.
    let raw = r#"{"schema-version":"0.1","catalog-id":"c","rules":[
        {"kind":"role-predicate","entity":"e","role":"r","expression":"true"}]}"#;
    let parsed = AccessPolicy::from_json(raw).expect("parses");
    match &parsed.rules[0] {
        Rule::RolePredicate { command, .. } => assert_eq!(*command, Command::All),
        other => panic!("expected role-predicate, got {other:?}"),
    }
}

// --- storage drift guard ---------------------------------------------------

#[test]
fn rls_storage_table_exists_in_catalog_schema_sql() {
    let sql = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/catalog-schema.sql"),
    )
    .expect("read catalog-schema.sql");
    assert!(sql.contains("CREATE TABLE catalog.rls_policies"));
    assert!(sql.contains("CREATE POLICY rls_policies_tenant"));
}

// --- live apply (gated) ----------------------------------------------------

/// A minimal single-entity catalog with a uuid owner column and no foreign keys
/// — so rows seed cleanly for the functional RLS check.
fn notes_catalog() -> Catalog {
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
        catalog_id: "notes".into(),
        version: 1,
        name: None,
        entities: vec![Entity {
            id: "notes".into(),
            name: "notes".into(),
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

/// Apply the tenant floor + a compiled ownership policy, then assert the
/// restrictive policy filters rows for the `wamn_app` role under session claims.
/// Gated on `WAMN_RLS_PG_URL` (a superuser URL — the harness provisions the
/// `wamn_app` role and an ephemeral schema). Skips cleanly when unset.
#[test]
fn compiled_policy_filters_rows_on_postgres() {
    let Ok(url) = std::env::var("WAMN_RLS_PG_URL") else {
        eprintln!("skipping compiled_policy_filters_rows_on_postgres (set WAMN_RLS_PG_URL to run)");
        return;
    };

    let catalog = notes_catalog();
    let floor = wamn_ddl::Migration::create(&catalog).unwrap();
    let p = AccessPolicy {
        schema_version: "0.1".into(),
        catalog_id: "notes".into(),
        rules: vec![Rule::RowOwnership {
            entity: "notes".into(),
            owner_field: "owner_id".into(),
            exempt_roles: vec!["admin".into()],
            name: None,
        }],
    };
    let policies = compile(&p, &catalog).unwrap();

    const U1: &str = "11111111-1111-1111-1111-111111111111";
    const U2: &str = "22222222-2222-2222-2222-222222222222";

    let mut script = String::new();
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS wamn_rls_test CASCADE;\n\
         CREATE SCHEMA wamn_rls_test AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA wamn_rls_test TO wamn_app;\n\
         SET search_path TO wamn_rls_test;\n",
    );
    script.push_str(&floor.sql(Confirmation::None).unwrap());
    script.push_str(&policies.sql(Confirmation::None).unwrap());
    // Seed as the (superuser) owner — superusers bypass RLS.
    script.push_str(&format!(
        "INSERT INTO notes (tenant_id, owner_id, body) VALUES ('t1','{U1}','a'),('t1','{U2}','b');\n"
    ));
    // As wamn_app with claims: an inspector sees only their own row…
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_rls_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         SET LOCAL app.role = 'inspector';\n\
         SET LOCAL app.user_id = '{U1}';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM notes) = 1, 'inspector sees only own row'; END $$;\n\
         COMMIT;\n"
    ));
    // …an exempt admin sees both…
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_rls_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         SET LOCAL app.role = 'admin';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM notes) = 2, 'admin (exempt) sees all rows'; END $$;\n\
         COMMIT;\n",
    );
    // …and with no user claim, ownership denies everything (safe default).
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_rls_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         SET LOCAL app.role = 'inspector';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM notes) = 0, 'no user claim denies all'; END $$;\n\
         COMMIT;\n",
    );
    script.push_str("DROP SCHEMA wamn_rls_test CASCADE;\n");

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

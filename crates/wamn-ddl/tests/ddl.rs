//! Compiler tests over the canonical POC catalog (reused from wamn-catalog's
//! fixtures): the CREATE plan is all-additive and tenant-safe, diffs classify
//! additive vs destructive, and the safety gate refuses unconfirmed destructive
//! DDL. An optional live-apply test runs the emitted SQL against a throwaway
//! Postgres when `WAMN_DDL_PG_URL` is set.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use wamn_catalog::{Catalog, Field, FieldType, Index};
use wamn_ddl::{CompileError, Confirmation, Migration};

/// The POC catalog fixture lives in the sibling wamn-catalog crate.
fn poc_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../wamn-catalog/tests/fixtures/poc-receiving.catalog.json")
}

fn poc() -> Catalog {
    let raw = std::fs::read_to_string(poc_fixture()).expect("read POC fixture");
    Catalog::from_json(&raw).expect("POC fixture parses")
}

fn text_field(id: &str) -> Field {
    Field {
        id: id.into(),
        name: id.into(),
        field_type: FieldType::Text { max_len: None },
        nullable: true,
        default: None,
        sensitive: false,
        is_system: false,
        label: None,
        description: None,
    }
}

#[test]
fn create_plan_is_additive_and_tenant_safe() {
    let plan = Migration::create(&poc()).expect("compiles");
    assert!(!plan.is_empty());
    assert!(plan.is_additive(), "a fresh CREATE has no destructive ops");
    assert!(!plan.requires_confirmation());

    let sql = plan
        .sql(Confirmation::None)
        .expect("additive needs no confirmation");
    // Tenant floor + managed PK.
    assert!(sql.contains("CREATE TABLE \"receipts\""));
    assert!(sql.contains("id uuid PRIMARY KEY DEFAULT gen_random_uuid()"));
    assert!(sql.contains("tenant_id text NOT NULL"));
    assert!(sql.contains("FORCE ROW LEVEL SECURITY"));
    assert!(sql.contains("current_setting('app.tenant', true)"));
    assert!(sql.contains("GRANT SELECT, INSERT, UPDATE, DELETE ON \"receipts\" TO wamn_app"));
    // Composite unique is tenant-scoped.
    assert!(sql.contains("UNIQUE (tenant_id, \"receipt_no\", \"supplier_id\")"));
    // Exact-decimal + unit comment; enum -> text + CHECK.
    assert!(sql.contains("numeric(5,2)"));
    assert!(sql.contains("IS 'unit: pct'"));
    assert!(sql.contains("CHECK (\"status\" IN ('open', 'disposed', 'escalated'))"));
    // Reference -> uuid column + FK.
    assert!(sql.contains("FOREIGN KEY (\"supplier_id\") REFERENCES \"suppliers\" (id)"));
}

#[test]
fn added_column_is_additive() {
    let v1 = poc();
    let mut v2 = v1.clone();
    v2.version = 2;
    let materials = v2
        .entities
        .iter_mut()
        .find(|e| e.id == "materials")
        .unwrap();
    materials.fields.push(text_field("grade"));
    materials.indexes.push(Index {
        name: "materials_grade_idx".into(),
        fields: vec!["grade".into()],
        unique: false,
    });

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    assert!(plan.is_additive(), "report: {}", plan.report());
    let sql = plan.sql(Confirmation::None).expect("additive");
    assert!(sql.contains("ALTER TABLE \"materials\" ADD COLUMN \"grade\" text"));
    assert!(sql.contains("CREATE INDEX \"materials_grade_idx\""));
}

#[test]
fn dropped_column_is_gated_destructive() {
    let v1 = poc();
    let mut v2 = v1.clone();
    v2.version = 2;
    let suppliers = v2
        .entities
        .iter_mut()
        .find(|e| e.id == "suppliers")
        .unwrap();
    suppliers.fields.retain(|f| f.id != "contact_email");

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    assert!(plan.requires_confirmation());
    assert_eq!(plan.destructive().count(), 1);

    // Refused without confirmation…
    let err = plan.sql(Confirmation::None).unwrap_err();
    assert!(err.destructive.iter().any(|s| s.contains("drop column")));

    // …allowed with confirmation + backup, and marked.
    let sql = plan
        .sql(Confirmation::ConfirmedWithBackup)
        .expect("confirmed");
    assert!(sql.contains("BACKUP CHECKPOINT REQUIRED"));
    assert!(sql.contains("ALTER TABLE \"suppliers\" DROP COLUMN \"contact_email\""));
}

#[test]
fn renamed_field_is_destructive() {
    // The 11.8 impact case: staging quality_holds.status -> hold_status.
    let v1 = poc();
    let mut v2 = v1.clone();
    v2.version = 2;
    let holds = v2
        .entities
        .iter_mut()
        .find(|e| e.id == "quality_holds")
        .unwrap();
    holds
        .fields
        .iter_mut()
        .find(|f| f.id == "status")
        .unwrap()
        .name = "hold_status".into();

    let plan = Migration::migrate(&v1, &v2).expect("compiles");
    assert!(plan.requires_confirmation());
    let op = plan
        .operations
        .iter()
        .find(|o| o.summary.contains("rename column"))
        .expect("a rename op");
    assert!(
        op.sql
            .contains("RENAME COLUMN \"status\" TO \"hold_status\"")
    );
    assert_eq!(op.entity, "quality_holds");
    assert_eq!(op.field.as_deref(), Some("status"));
}

#[test]
fn empty_diff_is_empty_plan() {
    let c = poc();
    let plan = Migration::migrate(&c, &c).expect("compiles");
    assert!(plan.is_empty());
    assert!(plan.is_additive());
    assert_eq!(plan.report(), "no changes\n");
}

#[test]
fn reserved_column_is_rejected() {
    let mut c = poc();
    c.entities[1].fields.push(text_field("tenant_id"));
    match Migration::create(&c) {
        Err(CompileError::ReservedColumn { field, .. }) => assert_eq!(field, "tenant_id"),
        other => panic!("expected ReservedColumn, got {other:?}"),
    }
}

#[test]
fn invalid_catalog_is_rejected() {
    let mut c = poc();
    c.entities.push(c.entities[0].clone()); // duplicate entity id
    match Migration::create(&c) {
        Err(CompileError::InvalidCatalog(issues)) => {
            assert!(issues.iter().any(|i| i.code == "duplicate-entity-id"))
        }
        other => panic!("expected InvalidCatalog, got {other:?}"),
    }
}

/// Live verification: apply the emitted DDL to a throwaway Postgres. Gated on
/// `WAMN_DDL_PG_URL` (a superuser URL — the harness provisions the wamn_app role
/// and an ephemeral schema). Skips cleanly when unset.
#[test]
fn emitted_sql_applies_on_postgres() {
    let Ok(url) = std::env::var("WAMN_DDL_PG_URL") else {
        eprintln!("skipping emitted_sql_applies_on_postgres (set WAMN_DDL_PG_URL to run)");
        return;
    };

    let v1 = poc();
    let create = Migration::create(&v1).unwrap();

    // Additive evolution: add a nullable column + index.
    let mut v2 = v1.clone();
    v2.version = 2;
    let materials = v2
        .entities
        .iter_mut()
        .find(|e| e.id == "materials")
        .unwrap();
    materials.fields.push(text_field("grade"));
    let add = Migration::migrate(&v1, &v2).unwrap();

    // Destructive evolution: drop a column (confirmed + backup).
    let mut v3 = v2.clone();
    v3.version = 3;
    let suppliers = v3
        .entities
        .iter_mut()
        .find(|e| e.id == "suppliers")
        .unwrap();
    suppliers.fields.retain(|f| f.id != "contact_email");
    let drop = Migration::migrate(&v2, &v3).unwrap();

    let mut script = String::new();
    // Provision role + isolate in a fresh schema, then apply the three plans.
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS wamn_ddl_test CASCADE;\n\
         CREATE SCHEMA wamn_ddl_test AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA wamn_ddl_test TO wamn_app;\n\
         SET search_path TO wamn_ddl_test;\n",
    );
    script.push_str(&create.sql(Confirmation::None).unwrap());
    script.push_str(&add.sql(Confirmation::None).unwrap());
    script.push_str(&drop.sql(Confirmation::ConfirmedWithBackup).unwrap());
    script.push_str("DROP SCHEMA wamn_ddl_test CASCADE;\n");

    let mut child = Command::new("psql")
        .arg(&url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    use std::io::Write;
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

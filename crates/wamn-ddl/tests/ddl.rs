//! Compiler tests over the canonical POC catalog (reused from wamn-catalog's
//! fixtures): the CREATE plan is all-additive and tenant-safe, diffs classify
//! additive vs destructive, and the safety gate refuses unconfirmed destructive
//! DDL. An optional live-apply test runs the emitted SQL against a throwaway
//! Postgres when `WAMN_DDL_PG_URL` is set.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use wamn_catalog::{Catalog, Entity, Field, FieldType, Index};
use wamn_ddl::{CompileError, Confirmation, Migration, OutboxOptions};

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

#[test]
fn outbox_triggers_cover_every_table_and_are_additive() {
    let c = poc();
    let plan = Migration::outbox_triggers(&c, &OutboxOptions::default()).expect("compiles");
    // One shared function + one trigger per entity table.
    assert_eq!(plan.operations.len(), c.entities.len() + 1);
    assert!(plan.is_additive());
    let sql = plan.sql(Confirmation::None).expect("additive");

    // The shared function: event vocabulary = lower(TG_OP) pinned to the "C"
    // collation (a Turkish/Azeri database default would lowercase INSERT to
    // 'ınsert', fail the outbox CHECK, and abort the user write), tenant from
    // the ROW on both the NEW and OLD paths, payload via to_jsonb, default
    // schema qualified.
    assert!(sql.contains("CREATE OR REPLACE FUNCTION wamn_outbox_event()"));
    assert!(
        sql.contains("INSERT INTO \"wamn_run\".\"outbox\" (tenant_id, table_name, event, payload)")
    );
    assert!(sql.contains("lower(TG_OP COLLATE \"C\")"));
    let f = &plan.operations[0];
    assert!(
        !f.sql.contains("current_setting"),
        "tenant comes from the row, not the claim (superuser seeds carry no claim)"
    );
    assert_eq!(f.entity, "", "function op is catalog-scoped");
    // The runtime precondition (the outbox must exist) and the target schema
    // are surfaced on the review surface — a mis-targeted or schema-drifted
    // apply must not read as a clean no-caveat plan.
    assert!(f.summary.contains("\"wamn_run\".\"outbox\""));
    assert!(f.note.as_deref().unwrap_or("").contains("fails at runtime"));

    // Branch-to-payload binding: the DELETE branch carries OLD, the
    // insert/update fall-through carries NEW — a swapped mutant must not pass.
    let body = &f.sql;
    let delete_branch = body
        .find("IF TG_OP = 'DELETE' THEN")
        .expect("delete branch");
    let end_if = body.find("END IF").expect("end of delete branch");
    let old_pos = body.find("to_jsonb(OLD)").expect("OLD payload");
    let new_pos = body.find("to_jsonb(NEW)").expect("NEW payload");
    assert!(
        delete_branch < old_pos && old_pos < end_if,
        "DELETE branch must carry OLD"
    );
    assert!(
        end_if < new_pos,
        "insert/update fall-through must carry NEW"
    );
    let old_tenant = body.find("OLD.tenant_id").expect("OLD tenant");
    let new_tenant = body.find("NEW.tenant_id").expect("NEW tenant");
    assert!(delete_branch < old_tenant && old_tenant < end_if);
    assert!(end_if < new_tenant);

    // One CONSTANT-named trigger per table: per-table trigger namespace makes
    // the name collision-free, and CREATE OR REPLACE + the constant name keep
    // re-apply idempotent and table renames from stacking a second trigger.
    assert!(sql.contains(
        "CREATE OR REPLACE TRIGGER wamn_outbox_event\n    \
         AFTER INSERT OR UPDATE OR DELETE ON \"receipts\"\n    \
         FOR EACH ROW EXECUTE FUNCTION wamn_outbox_event()"
    ));
    let trig = plan
        .operations
        .iter()
        .find(|o| o.sql.contains("ON \"receipts\""))
        .expect("receipts trigger op");
    assert_eq!(trig.entity, "receipts");
}

#[test]
fn outbox_schema_is_configurable_and_validated() {
    let c = poc();
    let plan = Migration::outbox_triggers(
        &c,
        &OutboxOptions {
            schema: "wamn_dispatch_demo".into(),
        },
    )
    .expect("compiles");
    assert!(
        plan.operations[0]
            .sql
            .contains("INSERT INTO \"wamn_dispatch_demo\".\"outbox\"")
    );

    // The schema is embedded inside the function body's dollar-quoted block, so
    // anything beyond a bare identifier is refused (quoting cannot protect
    // against a value containing the dollar tag).
    for bad in ["", "bad-name", "1st", "wamn.run", "a$wamn_outbox$b", "x y"] {
        match Migration::outbox_triggers(
            &c,
            &OutboxOptions {
                schema: bad.to_string(),
            },
        ) {
            Err(CompileError::InvalidOutboxSchema { schema }) => assert_eq!(schema, bad),
            other => panic!("expected InvalidOutboxSchema for {bad:?}, got {other:?}"),
        }
    }

    // The catalog is still validated on this entry point.
    let mut dup = c.clone();
    dup.entities.push(dup.entities[0].clone());
    assert!(matches!(
        Migration::outbox_triggers(&dup, &OutboxOptions::default()),
        Err(CompileError::InvalidCatalog(_))
    ));
}

#[test]
fn drop_outbox_triggers_is_gated_destructive() {
    let c = poc();
    let plan = Migration::drop_outbox_triggers(&c).expect("compiles");
    assert_eq!(plan.operations.len(), c.entities.len() + 1);
    assert!(plan.requires_confirmation());

    let err = plan.sql(Confirmation::None).unwrap_err();
    assert!(
        err.destructive
            .iter()
            .any(|s| s.contains("stop emitting row events"))
    );

    let sql = plan
        .sql(Confirmation::ConfirmedWithBackup)
        .expect("confirmed");
    assert!(sql.contains("DROP TRIGGER IF EXISTS wamn_outbox_event ON \"receipts\""));
    assert!(sql.contains("DROP FUNCTION IF EXISTS wamn_outbox_event()"));
    // Triggers drop before the function they reference.
    assert!(sql.find("DROP TRIGGER").unwrap() < sql.find("DROP FUNCTION").unwrap());
}

/// Drift guard: the columns and event vocabulary the emitted trigger writes
/// must exist in the production outbox (deploy/run-queue.sql) — and the
/// default [`OutboxOptions`] schema must be the one the deploy file creates.
#[test]
fn outbox_trigger_shape_matches_run_queue_deploy_file() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/run-queue.sql");
    let ddl = std::fs::read_to_string(path).expect("read deploy/run-queue.sql");

    let start = ddl
        .find("CREATE TABLE wamn_run.outbox")
        .expect("outbox table in deploy file");
    let block = &ddl[start..start + ddl[start..].find(");").expect("outbox block ends") + 2];
    for col in ["tenant_id", "table_name", "event", "payload"] {
        assert!(block.contains(col), "outbox column {col} missing:\n{block}");
    }
    assert!(
        block.contains("event IN ('insert', 'update', 'delete')"),
        "event CHECK literals must match lower(TG_OP)"
    );
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
         BEGIN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; END IF; END $$;\n\
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

/// Live behavioral verification of the outbox triggers on a throwaway Postgres
/// (gated on `WAMN_DDL_PG_URL`, skips cleanly when unset): a `wamn_app` write
/// emits exactly one event row **in the same transaction** (D4) with the exact
/// event vocabulary, the exact-decimal payload preserved byte-for-byte (no
/// float round trip), tenant taken from the row (superuser seeds fire too),
/// outbox RLS isolating tenants, an `ON CONFLICT DO NOTHING` no-op emitting
/// nothing, a re-applied plan not stacking a duplicate trigger, and the drop
/// plan silencing emission.
#[test]
fn outbox_triggers_fire_on_postgres() {
    let Ok(url) = std::env::var("WAMN_DDL_PG_URL") else {
        eprintln!("skipping outbox_triggers_fire_on_postgres (set WAMN_DDL_PG_URL to run)");
        return;
    };

    // A minimal one-table catalog with an exact-decimal column.
    let catalog = Catalog {
        schema_version: "0.1".into(),
        catalog_id: "wp4-receipts".into(),
        version: 1,
        name: None,
        entities: vec![Entity {
            id: "receipts".into(),
            name: "receipts".into(),
            is_system: false,
            label: None,
            description: None,
            fields: vec![
                Field {
                    id: "qty".into(),
                    name: "qty".into(),
                    field_type: FieldType::Numeric {
                        precision: 10,
                        scale: 2,
                        unit: Some("kg".into()),
                    },
                    nullable: false,
                    default: None,
                    sensitive: false,
                    is_system: false,
                    label: None,
                    description: None,
                },
                text_field("note"),
            ],
            indexes: vec![],
            constraints: vec![],
        }],
        relations: vec![],
    };
    let floor = Migration::create(&catalog).unwrap();
    // The test outbox lives in the ephemeral schema itself — which also
    // exercises the schema-qualified reference the production 'wamn_run'
    // default relies on.
    let opts = OutboxOptions {
        schema: "wamn_ddl_outbox_test".into(),
    };
    let triggers = Migration::outbox_triggers(&catalog, &opts).unwrap();

    // Rename evolution: receipts -> receipts2 (same entity id). The trigger
    // follows the rename; re-applying the v2 outbox plan must REPLACE it (the
    // constant name), not stack a second one.
    let mut v2 = catalog.clone();
    v2.version = 2;
    v2.entities[0].name = "receipts2".into();
    let rename = Migration::migrate(&catalog, &v2).unwrap();
    let triggers_v2 = Migration::outbox_triggers(&v2, &opts).unwrap();
    let drop = Migration::drop_outbox_triggers(&v2).unwrap();

    const R1: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    let mut script = String::new();
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         BEGIN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS wamn_ddl_outbox_test CASCADE;\n\
         CREATE SCHEMA wamn_ddl_outbox_test AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA wamn_ddl_outbox_test TO wamn_app;\n\
         SET search_path TO wamn_ddl_outbox_test;\n",
    );
    // Inline replica of the production outbox (deploy/run-queue.sql,
    // drift-guarded by outbox_trigger_shape_matches_run_queue_deploy_file).
    script.push_str(
        "CREATE TABLE outbox (\n\
             tenant_id     text NOT NULL,\n\
             seq           bigint GENERATED ALWAYS AS IDENTITY,\n\
             table_name    text NOT NULL,\n\
             event         text NOT NULL CHECK (event IN ('insert', 'update', 'delete')),\n\
             payload       jsonb,\n\
             created_at    timestamptz NOT NULL DEFAULT now(),\n\
             dispatched_at timestamptz,\n\
             PRIMARY KEY (tenant_id, seq)\n\
         );\n\
         ALTER TABLE outbox ENABLE ROW LEVEL SECURITY;\n\
         ALTER TABLE outbox FORCE ROW LEVEL SECURITY;\n\
         CREATE POLICY outbox_tenant ON outbox\n\
             USING (tenant_id = current_setting('app.tenant', true))\n\
             WITH CHECK (tenant_id = current_setting('app.tenant', true));\n\
         GRANT SELECT, INSERT, UPDATE, DELETE ON outbox TO wamn_app;\n",
    );
    script.push_str(&floor.sql(Confirmation::None).unwrap());
    script.push_str(&triggers.sql(Confirmation::None).unwrap());
    // Re-apply the whole trigger plan: CREATE OR REPLACE + the constant trigger
    // name must not stack a second trigger (asserted by the counts below).
    script.push_str(&triggers.sql(Confirmation::None).unwrap());

    // 1) A wamn_app INSERT emits exactly one 'insert' event in the SAME
    //    transaction (D4), with the exact-decimal payload preserved.
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_ddl_outbox_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         INSERT INTO receipts (id, tenant_id, qty, note) VALUES ('{R1}', 't1', 12.50, 'first');\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox) = 1, 'exactly one event (no duplicate trigger from the re-applied plan)';\n\
             ASSERT (SELECT count(*) FROM outbox WHERE event = 'insert' AND table_name = 'receipts' AND tenant_id = 't1') = 1, 'insert event shape';\n\
             ASSERT (SELECT payload->>'qty' FROM outbox WHERE event = 'insert') = '12.50', 'exact-decimal preserved';\n\
             ASSERT (SELECT payload::text FROM outbox WHERE event = 'insert') LIKE '%12.50%', 'payload text not float-rounded';\n\
             ASSERT (SELECT payload->>'note' FROM outbox WHERE event = 'insert') = 'first', 'full row in payload';\n\
         END $$;\n\
         COMMIT;\n"
    ));
    // 2) UPDATE emits an 'update' event carrying NEW values.
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_ddl_outbox_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         UPDATE receipts SET qty = 13.00 WHERE id = '{R1}';\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox WHERE event = 'update') = 1, 'one update event';\n\
             ASSERT (SELECT payload->>'qty' FROM outbox WHERE event = 'update') = '13.00', 'update carries NEW';\n\
         END $$;\n\
         COMMIT;\n"
    ));
    // 3) An ON CONFLICT DO NOTHING no-op (a 3.6 re-seed) emits nothing.
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_ddl_outbox_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         INSERT INTO receipts (id, tenant_id, qty, note) VALUES ('{R1}', 't1', 99.99, 'dup') ON CONFLICT (id) DO NOTHING;\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox) = 2, 'no event from a conflict no-op';\n\
         END $$;\n\
         COMMIT;\n"
    ));
    // 4) DELETE emits a 'delete' event carrying OLD.
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_ddl_outbox_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         DELETE FROM receipts WHERE id = '{R1}';\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox WHERE event = 'delete') = 1, 'one delete event';\n\
             ASSERT (SELECT payload->>'qty' FROM outbox WHERE event = 'delete') = '13.00', 'delete carries OLD';\n\
         END $$;\n\
         COMMIT;\n"
    ));
    // 5) A superuser seed (BYPASSRLS, no app.tenant claim) fires too — tenant
    //    comes from the ROW, not the claim.
    script.push_str(
        "INSERT INTO receipts (tenant_id, qty, note) VALUES ('t2', 1.00, 'seed');\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox WHERE tenant_id = 't2' AND event = 'insert') = 1, 'superuser seed fires with row tenant';\n\
         END $$;\n",
    );
    // 6) Outbox RLS isolates tenants: t1 sees only its own 3 events.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_ddl_outbox_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox) = 3, 't1 sees only t1 events';\n\
         END $$;\n\
         COMMIT;\n",
    );
    // 7) Rename-safety: apply the v2 rename migration, re-apply the v2 outbox
    //    plan, and prove a write to the renamed table fires EXACTLY ONCE — the
    //    renamed table kept its trigger, and the constant-named CREATE OR
    //    REPLACE replaced it rather than stacking a second.
    script.push_str(&rename.sql(Confirmation::ConfirmedWithBackup).unwrap());
    script.push_str(&triggers_v2.sql(Confirmation::None).unwrap());
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_ddl_outbox_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         INSERT INTO receipts2 (tenant_id, qty, note) VALUES ('t1', 7.25, 'renamed');\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox WHERE table_name = 'receipts2') = 1, 'exactly one event after rename + re-apply (no stacked trigger)';\n\
         END $$;\n\
         COMMIT;\n",
    );
    // 8) The (confirmed) drop plan silences emission.
    script.push_str(&drop.sql(Confirmation::ConfirmedWithBackup).unwrap());
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_ddl_outbox_test;\n\
         SET LOCAL app.tenant = 't1';\n\
         INSERT INTO receipts2 (tenant_id, qty, note) VALUES ('t1', 5.00, 'after-drop');\n\
         DO $$ BEGIN\n\
             ASSERT (SELECT count(*) FROM outbox WHERE tenant_id = 't1') = 4, 'no event after the drop plan';\n\
         END $$;\n\
         COMMIT;\n",
    );
    script.push_str("DROP SCHEMA wamn_ddl_outbox_test CASCADE;\n");

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

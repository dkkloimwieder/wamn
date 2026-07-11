//! Seed-tooling tests. Deterministic emission + validation over the POC catalog,
//! plus an optional live-apply test that loads a compiled seed into a throwaway
//! Postgres and asserts foreign-key resolution and idempotent re-apply.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use wamn_catalog::{Catalog, Entity, Field, FieldType};
use wamn_seed::{CompileError, Confirmation, Dataset, EntitySeed, SeedRow, compile};

fn poc_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../wamn-catalog/tests/fixtures/poc-receiving.catalog.json")
}

fn poc() -> Catalog {
    let raw = std::fs::read_to_string(poc_fixture()).expect("read POC fixture");
    Catalog::from_json(&raw).expect("POC fixture parses")
}

fn row(key: &str, values: &[(&str, Value)]) -> SeedRow {
    SeedRow {
        key: key.into(),
        values: values
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    }
}

fn dataset(entities: Vec<EntitySeed>) -> Dataset {
    Dataset {
        schema_version: "0.1".into(),
        catalog_id: "poc-material-receiving".into(),
        entities,
    }
}

/// A small POC dataset: a supplier + a site, and a receipt referencing both.
fn poc_dataset() -> Dataset {
    dataset(vec![
        EntitySeed {
            entity: "suppliers".into(),
            rows: vec![row(
                "acme",
                &[
                    ("name", json!("Acme Corp")),
                    ("contact_email", json!("qa@acme.test")),
                    ("standard_cost", json!("12.50")),
                ],
            )],
        },
        EntitySeed {
            entity: "sites".into(),
            rows: vec![row("hq", &[("name", json!("HQ")), ("code", json!("HQ01"))])],
        },
        EntitySeed {
            entity: "receipts".into(),
            rows: vec![row(
                "r1",
                &[
                    ("receipt_no", json!("R-001")),
                    ("supplier_id", json!("acme")), // reference by key
                    ("site_id", json!("hq")),
                    ("received_at", json!("2026-07-11T00:00:00Z")),
                ],
            )],
        },
    ])
}

// --- emission --------------------------------------------------------------

#[test]
fn compiles_fk_safe_idempotent_inserts() {
    let plan = compile(&poc_dataset(), &poc(), "t1").expect("compiles");
    assert!(plan.is_additive());
    let sql = plan.sql(Confirmation::None).expect("additive");

    // Referenced entities are emitted before the referencing one.
    let sup = sql.find("INTO \"suppliers\"").unwrap();
    let site = sql.find("INTO \"sites\"").unwrap();
    let rec = sql.find("INTO \"receipts\"").unwrap();
    assert!(sup < rec && site < rec, "suppliers/sites before receipts");

    // Tenant floor: managed id + tenant literal; idempotent conflict clause.
    assert!(sql.contains("(id, tenant_id, "));
    assert!(sql.contains("'t1'"));
    assert!(sql.contains("ON CONFLICT (id) DO NOTHING"));
    // Exact-decimal emitted unquoted (no float).
    assert!(sql.contains("12.50"));
    assert!(!sql.contains("'12.50'"));
}

#[test]
fn references_resolve_to_the_target_row_id() {
    let plan = compile(&poc_dataset(), &poc(), "t1").unwrap();
    // The supplier's own id is the deterministic id for suppliers:acme…
    let supplier_op = plan
        .operations
        .iter()
        .find(|o| o.entity == "suppliers")
        .unwrap();
    let receipt_op = plan
        .operations
        .iter()
        .find(|o| o.entity == "receipts")
        .unwrap();
    // …extract the literal id the supplier INSERT sets, and assert the receipt
    // INSERT references that exact uuid for supplier_id.
    let sup_id = first_uuid_literal(&supplier_op.sql);
    assert!(
        receipt_op.sql.contains(&sup_id),
        "receipt should reference the supplier's id {sup_id}"
    );
}

#[test]
fn ids_are_deterministic_across_compiles() {
    let a = compile(&poc_dataset(), &poc(), "t1").unwrap();
    let b = compile(&poc_dataset(), &poc(), "t1").unwrap();
    assert_eq!(
        a.sql(Confirmation::None).unwrap(),
        b.sql(Confirmation::None).unwrap()
    );
    // A different tenant yields different ids.
    let c = compile(&poc_dataset(), &poc(), "t2").unwrap();
    assert_ne!(
        a.sql(Confirmation::None).unwrap(),
        c.sql(Confirmation::None).unwrap()
    );
}

// --- validation ------------------------------------------------------------

fn expect_codes(d: &Dataset, catalog: &Catalog, codes: &[&str]) {
    match compile(d, catalog, "t1") {
        Err(CompileError::InvalidDataset(issues)) => {
            for code in codes {
                assert!(
                    issues.iter().any(|i| i.code == *code),
                    "expected issue {code:?}, got {:?}",
                    issues.iter().map(|i| i.code).collect::<Vec<_>>()
                );
            }
        }
        other => panic!("expected InvalidDataset with {codes:?}, got {other:?}"),
    }
}

#[test]
fn rejects_unknown_entity_field_and_reference() {
    expect_codes(
        &dataset(vec![EntitySeed {
            entity: "nope".into(),
            rows: vec![row("x", &[])],
        }]),
        &poc(),
        &["unknown-entity"],
    );
    expect_codes(
        &dataset(vec![EntitySeed {
            entity: "sites".into(),
            rows: vec![row(
                "s",
                &[
                    ("name", json!("S")),
                    ("code", json!("C")),
                    ("bogus", json!(1)),
                ],
            )],
        }]),
        &poc(),
        &["unknown-field"],
    );
    // A receipt referencing a supplier key that was never seeded.
    expect_codes(
        &dataset(vec![EntitySeed {
            entity: "receipts".into(),
            rows: vec![row(
                "r",
                &[
                    ("receipt_no", json!("R")),
                    ("supplier_id", json!("ghost")),
                    ("site_id", json!("ghost")),
                    ("received_at", json!("2026-07-11T00:00:00Z")),
                ],
            )],
        }]),
        &poc(),
        &["unknown-reference"],
    );
}

#[test]
fn rejects_reserved_columns_and_missing_required_fields() {
    expect_codes(
        &dataset(vec![EntitySeed {
            entity: "sites".into(),
            rows: vec![row(
                "s",
                &[
                    ("id", json!("x")),
                    ("name", json!("S")),
                    ("code", json!("C")),
                ],
            )],
        }]),
        &poc(),
        &["reserved-field"],
    );
    // sites.name / code are non-nullable with no default.
    expect_codes(
        &dataset(vec![EntitySeed {
            entity: "sites".into(),
            rows: vec![row("s", &[("code", json!("C"))])],
        }]),
        &poc(),
        &["missing-required-field"],
    );
}

#[test]
fn rejects_bad_types_floats_and_bad_enums() {
    // moisture_max_pct is numeric(5,2) — a float value violates the no-float rule.
    expect_codes(
        &dataset(vec![EntitySeed {
            entity: "materials".into(),
            rows: vec![row(
                "m",
                &[
                    ("name", json!("M")),
                    ("moisture_max_pct", json!(12.5)),
                    ("weight_tolerance_kg", json!("1.000")),
                ],
            )],
        }]),
        &poc(),
        &["numeric-not-exact"],
    );
    // quality_holds.status enum only allows open/disposed/escalated.
    expect_codes(
        &dataset(vec![EntitySeed {
            entity: "quality_holds".into(),
            rows: vec![row(
                "h",
                &[
                    ("line_id", json!("l")),
                    ("site_id", json!("s")),
                    ("status", json!("bogus")),
                    ("opened_at", json!("2026-07-11T00:00:00Z")),
                ],
            )],
        }]),
        &poc(),
        &["enum-not-a-variant"],
    );
}

#[test]
fn rejects_duplicate_keys_and_catalog_mismatch() {
    let mut d = dataset(vec![EntitySeed {
        entity: "sites".into(),
        rows: vec![
            row("dup", &[("name", json!("A")), ("code", json!("A"))]),
            row("dup", &[("name", json!("B")), ("code", json!("B"))]),
        ],
    }]);
    d.catalog_id = "other".into();
    expect_codes(&d, &poc(), &["duplicate-key", "catalog-id-mismatch"]);
}

// --- model round-trip ------------------------------------------------------

#[test]
fn json_round_trips() {
    let d = poc_dataset();
    let back = Dataset::from_json(&d.to_json()).expect("round-trips");
    assert_eq!(d, back);
}

// --- storage drift guard ---------------------------------------------------

#[test]
fn seed_storage_table_exists_in_catalog_schema_sql() {
    let sql = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/catalog-schema.sql"),
    )
    .expect("read catalog-schema.sql");
    assert!(sql.contains("CREATE TABLE catalog.seed_datasets"));
    assert!(sql.contains("CREATE POLICY seed_datasets_tenant"));
}

// --- live apply (gated) ----------------------------------------------------

/// A minimal two-entity catalog exercising a cross-entity reference.
fn suppliers_receipts_catalog() -> Catalog {
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
    let entity = |id: &str, fields: Vec<Field>| Entity {
        id: id.into(),
        name: id.into(),
        is_system: false,
        label: None,
        description: None,
        fields,
        indexes: vec![],
        constraints: vec![],
    };
    Catalog {
        schema_version: "0.1".into(),
        catalog_id: "sr".into(),
        version: 1,
        name: None,
        entities: vec![
            entity(
                "suppliers",
                vec![f("name", FieldType::Text { max_len: None }, false)],
            ),
            entity(
                "receipts",
                vec![
                    f("receipt_no", FieldType::Text { max_len: None }, false),
                    f(
                        "supplier_id",
                        FieldType::Reference {
                            entity: "suppliers".into(),
                        },
                        false,
                    ),
                ],
            ),
        ],
        relations: vec![],
    }
}

/// Apply the tenant floor + the compiled seed to a throwaway Postgres, then
/// re-apply it, asserting the FK resolved and the second load is a no-op.
/// Gated on `WAMN_SEED_PG_URL` (a superuser URL). Skips cleanly when unset.
#[test]
fn seed_applies_and_reapply_is_idempotent() {
    let Ok(url) = std::env::var("WAMN_SEED_PG_URL") else {
        eprintln!("skipping seed_applies_and_reapply_is_idempotent (set WAMN_SEED_PG_URL to run)");
        return;
    };

    let catalog = suppliers_receipts_catalog();
    let floor = wamn_ddl::Migration::create(&catalog).unwrap();
    let ds = Dataset {
        schema_version: "0.1".into(),
        catalog_id: "sr".into(),
        entities: vec![
            EntitySeed {
                entity: "suppliers".into(),
                rows: vec![SeedRow {
                    key: "acme".into(),
                    values: one("name", json!("Acme")),
                }],
            },
            EntitySeed {
                entity: "receipts".into(),
                rows: vec![SeedRow {
                    key: "r1".into(),
                    values: two("receipt_no", json!("R-001"), "supplier_id", json!("acme")),
                }],
            },
        ],
    };
    let seed = compile(&ds, &catalog, "t1").unwrap();

    let mut script = String::new();
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS wamn_seed_test CASCADE;\n\
         CREATE SCHEMA wamn_seed_test AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA wamn_seed_test TO wamn_app;\n\
         SET search_path TO wamn_seed_test;\n",
    );
    script.push_str(&floor.sql(Confirmation::None).unwrap());
    // Load twice — the second is a no-op thanks to ON CONFLICT DO NOTHING.
    script.push_str(&seed.sql(Confirmation::None).unwrap());
    script.push_str(&seed.sql(Confirmation::None).unwrap());
    // The FK resolved (the join finds the supplier) and each table has one row.
    script.push_str(
        "DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM suppliers) = 1, 'one supplier after re-apply';\n\
           ASSERT (SELECT count(*) FROM receipts) = 1, 'one receipt after re-apply';\n\
           ASSERT (SELECT count(*) FROM receipts r JOIN suppliers s ON s.id = r.supplier_id) = 1, 'fk resolves';\n\
         END $$;\n",
    );
    script.push_str("DROP SCHEMA wamn_seed_test CASCADE;\n");

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

// --- helpers ---------------------------------------------------------------

fn one(k: &str, v: Value) -> BTreeMap<String, Value> {
    let mut m = BTreeMap::new();
    m.insert(k.to_string(), v);
    m
}

fn two(k1: &str, v1: Value, k2: &str, v2: Value) -> BTreeMap<String, Value> {
    let mut m = one(k1, v1);
    m.insert(k2.to_string(), v2);
    m
}

/// The first single-quoted literal in an INSERT — the row's `id` uuid.
fn first_uuid_literal(sql: &str) -> String {
    let start = sql.find('\'').expect("a literal");
    let end = sql[start + 1..].find('\'').expect("closing quote") + start + 1;
    sql[start..=end].to_string()
}

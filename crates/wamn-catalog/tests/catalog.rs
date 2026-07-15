//! Integration tests over the canonical example catalogs (the POC data model +
//! a hierarchical/closure genealogy model): import round-trips, structural
//! validation passes, each conforms to the published JSON Schema, the committed
//! schema matches the types, and the diff detects real changes.

use std::path::{Path, PathBuf};

use boon::{Compiler, Schemas};
use wamn_catalog::Catalog;

const FIXTURES: &[&str] = &["poc-receiving.catalog.json", "genealogy.catalog.json"];

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load(name: &str) -> Catalog {
    let raw = std::fs::read_to_string(fixture_dir().join(name)).expect("read fixture");
    Catalog::from_json(&raw).unwrap_or_else(|e| panic!("{name} parses: {e}"))
}

#[test]
fn fixtures_parse_and_validate() {
    for name in FIXTURES {
        let cat = load(name);
        assert!(
            cat.is_valid(),
            "{name} should validate; issues: {:?}",
            cat.issues()
        );
        // No warnings either — the example catalogs are clean (no empty
        // entities, no many-to-many without a join entity).
        assert!(
            cat.issues().is_empty(),
            "{name} has unexpected issues: {:?}",
            cat.issues()
        );
    }
}

#[test]
fn fixtures_round_trip() {
    for name in FIXTURES {
        let cat = load(name);
        let reparsed = Catalog::from_json(&cat.to_json()).expect("re-parse export");
        assert_eq!(cat, reparsed, "{name} does not round-trip");
    }
}

#[test]
fn fixtures_conform_to_published_schema() {
    // The language-neutral contract must accept every example catalog — this
    // ties docs/catalog-model.schema.json to the real models the API/designer
    // will send.
    let schema = wamn_catalog::json_schema();
    let mut compiler = Compiler::new();
    compiler
        .add_resource("mem://catalog-model.json", schema)
        .expect("add schema resource");
    let mut schemas = Schemas::new();
    let sch = compiler
        .compile("mem://catalog-model.json", &mut schemas)
        .expect("compile schema");

    for name in FIXTURES {
        let raw = std::fs::read_to_string(fixture_dir().join(name)).expect("read fixture");
        let instance: serde_json::Value = serde_json::from_str(&raw).expect("fixture is json");
        if let Err(e) = schemas.validate(&instance, sch) {
            panic!("{name} does not conform to the published schema:\n{e}");
        }
    }
}

#[test]
fn committed_schema_matches_types() {
    // Drift guard: regenerate with
    //   cargo run -p wamn-catalog --example print-schema > docs/catalog-model.schema.json
    let committed = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/catalog-model.schema.json"),
    )
    .expect("read committed schema");
    assert_eq!(
        committed,
        wamn_catalog::json_schema_string(),
        "docs/catalog-model.schema.json is stale — regenerate it (see print-schema example)"
    );
}

#[test]
fn diff_detects_changes() {
    let v1 = load("poc-receiving.catalog.json");

    let mut v2 = v1.clone();
    v2.version = 2;

    // 1) rename a field (the 11.8 impact demo: stage quality_holds.status rename)
    {
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
        // 2) retype a field (widen the quantity precision on a different entity)
    }
    {
        let lines = v2
            .entities
            .iter_mut()
            .find(|e| e.id == "receipt_lines")
            .unwrap();
        lines
            .fields
            .iter_mut()
            .find(|f| f.id == "quantity")
            .unwrap()
            .field_type = wamn_catalog::FieldType::Numeric {
            precision: 14,
            scale: 3,
            unit: Some("kg".into()),
        };
    }
    // 3) add a new entity + relation
    v2.entities.push(wamn_catalog::Entity {
        id: "audit_log".into(),
        name: "audit_log".into(),
        is_system: false,
        label: None,
        description: None,
        fields: vec![wamn_catalog::Field {
            id: "message".into(),
            name: "message".into(),
            field_type: wamn_catalog::FieldType::Text { max_len: None },
            nullable: false,
            default: None,
            sensitive: false,
            is_system: false,
            label: None,
            description: None,
        }],
        indexes: vec![],
        constraints: vec![],
    });

    let d = wamn_catalog::diff(&v1, &v2);
    assert!(!d.is_empty());
    assert!(d.entities_added.iter().any(|e| e == "audit_log"));
    assert!(d.entities_removed.is_empty());

    let holds_change = d
        .entities_changed
        .iter()
        .find(|c| c.id == "quality_holds")
        .expect("quality_holds changed");
    let status_change = holds_change
        .fields_changed
        .iter()
        .find(|f| f.id == "status")
        .expect("status field changed");
    assert!(
        status_change.name_changed.is_some(),
        "the staged rename should surface for impact analysis"
    );

    let lines_change = d
        .entities_changed
        .iter()
        .find(|c| c.id == "receipt_lines")
        .expect("receipt_lines changed");
    assert!(
        lines_change
            .fields_changed
            .iter()
            .any(|f| f.id == "quantity" && f.type_changed.is_some())
    );

    // A catalog diffed against itself is empty.
    assert!(wamn_catalog::diff(&v1, &v1).is_empty());
}

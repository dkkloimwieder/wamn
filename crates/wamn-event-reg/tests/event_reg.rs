//! Validation + round-trip tests for the event-registration model (EVT-REG,
//! D19 v3 §5).
//!
//! Mutation-style discipline: each load-bearing validation rule fails a NAMED
//! test (flip the rule and exactly one test goes red).

use wamn_catalog::Catalog;
use wamn_event_reg::{EventRegistration, Op, SCHEMA_VERSION, validate};

/// A two-entity catalog. The entity **id** `sales_orders` deliberately differs
/// from its table **name** `orders`, so a registration proven to resolve is
/// resolving by id (the rename-proof key), never echoing a table name.
const CATALOG: &str = r#"{
  "schema-version": "0.1",
  "catalog-id": "shop",
  "version": 1,
  "entities": [
    { "id": "sales_orders", "name": "orders",
      "fields": [ { "id": "status", "name": "status", "type": { "kind": "text" } } ] },
    { "id": "line_items", "name": "lines",
      "fields": [ { "id": "qty", "name": "qty", "type": { "kind": "int" } } ] }
  ]
}"#;

fn catalog() -> Catalog {
    Catalog::from_json(CATALOG).expect("fixture parses")
}

/// A valid registration on `sales_orders`, insert+update, with a "changed-to"
/// condition and a partition key.
fn reg() -> EventRegistration {
    EventRegistration {
        schema_version: SCHEMA_VERSION.to_string(),
        registration_id: "on-order-shipped".into(),
        catalog_id: "shop".into(),
        flow_id: "notify".into(),
        entity: "sales_orders".into(),
        ops: vec![Op::Insert, Op::Update],
        condition: Some("new.status == 'shipped' && old.status != 'shipped'".into()),
        partition_key: Some("new.status".into()),
    }
}

#[test]
fn a_well_formed_registration_validates() {
    assert!(validate(&reg(), &catalog()).is_ok());
}

#[test]
fn entity_is_resolved_by_id_not_table_name() {
    // The id `sales_orders` resolves; the TABLE name `orders` does NOT — proof
    // the check keys on the rename-proof entity id.
    let mut r = reg();
    r.entity = "orders".into();
    let issues = validate(&r, &catalog()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "unknown-entity"));
}

#[test]
fn an_empty_op_set_is_inert_and_rejected() {
    let mut r = reg();
    r.ops.clear();
    let issues = validate(&r, &catalog()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "empty-ops"));
}

#[test]
fn a_duplicate_op_is_rejected() {
    let mut r = reg();
    r.ops = vec![Op::Insert, Op::Insert];
    let issues = validate(&r, &catalog()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "duplicate-op"));
}

#[test]
fn a_syntactically_broken_condition_is_rejected() {
    let mut r = reg();
    r.condition = Some("new.status ==".into()); // trailing operator: not JMESPath
    let issues = validate(&r, &catalog()).unwrap_err();
    assert!(
        issues
            .iter()
            .any(|i| i.code == "invalid-jmespath" && i.path == "condition")
    );
}

#[test]
fn a_syntactically_broken_partition_key_is_rejected() {
    let mut r = reg();
    r.partition_key = Some("new[".into()); // unterminated index
    let issues = validate(&r, &catalog()).unwrap_err();
    assert!(
        issues
            .iter()
            .any(|i| i.code == "invalid-jmespath" && i.path == "partition-key")
    );
}

#[test]
fn a_present_but_empty_expression_is_rejected() {
    // Empty is NOT "match everything" — omit the field (None) for that.
    let mut r = reg();
    r.condition = Some("   ".into());
    let issues = validate(&r, &catalog()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "empty-expression"));
}

#[test]
fn a_registration_with_no_condition_or_key_is_fine() {
    let mut r = reg();
    r.condition = None;
    r.partition_key = None;
    assert!(validate(&r, &catalog()).is_ok());
}

#[test]
fn an_incompatible_schema_version_is_rejected() {
    let mut r = reg();
    r.schema_version = "0.2".into();
    let issues = validate(&r, &catalog()).unwrap_err();
    assert!(
        issues
            .iter()
            .any(|i| i.code == "unsupported-schema-version")
    );
}

#[test]
fn a_catalog_id_mismatch_is_rejected() {
    let mut r = reg();
    r.catalog_id = "other".into();
    let issues = validate(&r, &catalog()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "catalog-id-mismatch"));
}

#[test]
fn an_empty_registration_id_or_flow_id_is_rejected() {
    let mut r = reg();
    r.registration_id = "".into();
    r.flow_id = " ".into();
    let issues = validate(&r, &catalog()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "empty-registration-id"));
    assert!(issues.iter().any(|i| i.code == "empty-flow-id"));
}

#[test]
fn round_trips_through_canonical_json_with_kebab_case_fields() {
    let r = reg();
    let json = r.to_json();
    // Field spellings are kebab-case (catalog/flow/rls convention); the entity
    // is a bare string (transparent EntityId); ops are lowercase.
    assert!(json.contains("\"schema-version\""));
    assert!(json.contains("\"registration-id\""));
    assert!(json.contains("\"partition-key\""));
    assert!(json.contains("\"entity\": \"sales_orders\""));
    assert!(json.contains("\"insert\""));
    let back = EventRegistration::from_json(&json).unwrap();
    assert_eq!(back, r);
}

#[test]
fn optional_fields_are_omitted_when_absent() {
    let mut r = reg();
    r.condition = None;
    r.partition_key = None;
    let json = r.to_json();
    assert!(!json.contains("condition"));
    assert!(!json.contains("partition-key"));
}

#[test]
fn unknown_fields_are_rejected_on_import() {
    // deny_unknown_fields: a smuggled key is not silently dropped.
    let json = r#"{"schema-version":"0.1","registration-id":"x","catalog-id":"shop",
        "flow-id":"f","entity":"sales_orders","ops":["insert"],"surprise":1}"#;
    assert!(EventRegistration::from_json(json).is_err());
}

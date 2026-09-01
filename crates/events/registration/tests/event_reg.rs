//! Validation + round-trip tests for the event-registration model (EVT-REG,
//! D19 v3 §5).
//!
//! Mutation-style discipline: each load-bearing validation rule fails a NAMED
//! test (flip the rule and exactly one test goes red).

use std::collections::BTreeSet;

use wamn_event_reg::{EventRegistration, Op, RegistrationInput, SCHEMA_VERSION, validate};

fn model_keys() -> BTreeSet<String> {
    BTreeSet::from(["sales_orders".into(), "line_items".into()])
}

/// A valid registration on `sales_orders`, insert+update, with a "changed-to"
/// condition.
fn reg() -> EventRegistration {
    EventRegistration {
        schema_version: SCHEMA_VERSION.to_string(),
        registration_id: "on-order-shipped".into(),
        package_id: "shop".into(),
        source_package_id: "shop".into(),
        flow_id: "notify".into(),
        entity: "sales_orders".into(),
        ops: vec![Op::Insert, Op::Update],
        input: RegistrationInput::Event,
        condition: Some("new.status == 'shipped' && old.status != 'shipped'".into()),
    }
}

#[test]
fn a_well_formed_registration_validates() {
    assert!(validate(&reg(), "shop", "shop", &model_keys()).is_ok());
}

#[test]
fn entity_is_resolved_by_id_not_table_name() {
    // The id `sales_orders` resolves; the TABLE name `orders` does NOT — proof
    // the check keys on the rename-proof entity id.
    let mut r = reg();
    r.entity = "orders".into();
    let issues = validate(&r, "shop", "shop", &model_keys()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "unknown-entity"));
}

#[test]
fn an_empty_op_set_is_inert_and_rejected() {
    let mut r = reg();
    r.ops.clear();
    let issues = validate(&r, "shop", "shop", &model_keys()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "empty-ops"));
}

#[test]
fn a_duplicate_op_is_rejected() {
    let mut r = reg();
    r.ops = vec![Op::Insert, Op::Insert];
    let issues = validate(&r, "shop", "shop", &model_keys()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "duplicate-op"));
}

#[test]
fn a_syntactically_broken_condition_is_rejected() {
    let mut r = reg();
    r.condition = Some("new.status ==".into()); // trailing operator: not JMESPath
    let issues = validate(&r, "shop", "shop", &model_keys()).unwrap_err();
    assert!(
        issues
            .iter()
            .any(|i| i.code == "invalid-jmespath" && i.path == "condition")
    );
}

#[test]
fn a_present_but_empty_expression_is_rejected() {
    // Empty is NOT "match everything" — omit the field (None) for that.
    let mut r = reg();
    r.condition = Some("   ".into());
    let issues = validate(&r, "shop", "shop", &model_keys()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "empty-expression"));
}

#[test]
fn a_registration_with_no_condition_is_fine() {
    let mut r = reg();
    r.condition = None;
    assert!(validate(&r, "shop", "shop", &model_keys()).is_ok());
}

#[test]
fn an_incompatible_schema_version_is_rejected() {
    let mut r = reg();
    r.schema_version = "0.2".into();
    let issues = validate(&r, "shop", "shop", &model_keys()).unwrap_err();
    assert!(
        issues
            .iter()
            .any(|i| i.code == "unsupported-schema-version")
    );
}

#[test]
fn a_package_id_mismatch_is_rejected() {
    let mut r = reg();
    r.package_id = "other".into();
    let issues = validate(&r, "shop", "shop", &model_keys()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "package-id-mismatch"));
}

#[test]
fn source_package_is_independent_from_the_registration_owner() {
    let mut r = reg();
    r.package_id = "client_acme_receiving".into();
    r.source_package_id = "wamn_receiving".into();
    assert!(validate(&r, "client_acme_receiving", "wamn_receiving", &model_keys()).is_ok());
    let issues = validate(&r, "client_acme_receiving", "other", &model_keys()).unwrap_err();
    assert!(
        issues
            .iter()
            .any(|issue| issue.code == "source-package-id-mismatch")
    );
    assert_eq!(r.qualified_id(), "client_acme_receiving::on-order-shipped");
}

#[test]
fn an_empty_registration_id_or_flow_id_is_rejected() {
    let mut r = reg();
    r.registration_id = "".into();
    r.flow_id = " ".into();
    let issues = validate(&r, "shop", "shop", &model_keys()).unwrap_err();
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
    assert!(json.contains("\"entity\": \"sales_orders\""));
    assert!(json.contains("\"insert\""));
    let back = EventRegistration::from_json(&json).unwrap();
    assert_eq!(back, r);
}

#[test]
fn frozen_wire_shape_is_the_exact_field_order_and_spellings() {
    // The freeze golden (wamn-l5i9.30): a field rename/removal or reordering
    // breaks THIS string. Compact form pins the canonical field ORDER
    // (declaration order) and the kebab-case spellings deterministically.
    let full = serde_json::to_string(&reg()).unwrap();
    assert_eq!(
        full,
        r#"{"schema-version":"0.1","registration-id":"on-order-shipped","package-id":"shop","source-package-id":"shop","flow-id":"notify","entity":"sales_orders","ops":["insert","update"],"input":"event","condition":"new.status == 'shipped' && old.status != 'shipped'"}"#
    );
    // Minimal: the optional condition is OMITTED (not null).
    let mut r = reg();
    r.condition = None;
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        r#"{"schema-version":"0.1","registration-id":"on-order-shipped","package-id":"shop","source-package-id":"shop","flow-id":"notify","entity":"sales_orders","ops":["insert","update"],"input":"event"}"#
    );
}

#[test]
fn input_is_closed_to_event_or_batch_and_legacy_rows_mean_event() {
    let mut batch = reg();
    batch.input = RegistrationInput::Batch;
    assert!(batch.to_json().contains("\"input\": \"batch\""));

    let legacy = r#"{"schema-version":"0.1","registration-id":"x","package-id":"shop","source-package-id":"shop","flow-id":"f","entity":"sales_orders","ops":["insert"]}"#;
    assert_eq!(
        EventRegistration::from_json(legacy).unwrap().input,
        RegistrationInput::Event
    );

    let invalid = r#"{"schema-version":"0.1","registration-id":"x","package-id":"shop","source-package-id":"shop","flow-id":"f","entity":"sales_orders","ops":["insert"],"input":"stream"}"#;
    assert!(EventRegistration::from_json(invalid).is_err());
}

#[test]
fn optional_fields_are_omitted_when_absent() {
    let mut r = reg();
    r.condition = None;
    let json = r.to_json();
    assert!(!json.contains("condition"));
}

#[test]
fn unknown_fields_are_rejected_on_import() {
    // deny_unknown_fields: a smuggled key is not silently dropped.
    let json = r#"{"schema-version":"0.1","registration-id":"x","package-id":"shop",
        "source-package-id":"shop","flow-id":"f","entity":"sales_orders","ops":["insert"],"surprise":1}"#;
    assert!(EventRegistration::from_json(json).is_err());
}

#[test]
fn a_legacy_state_key_is_rejected_on_import() {
    // EVT-TEARDOWN (l5i9.19, owner decision 2026-07-20): registration
    // `state: shadow|live` was removed WITH the outbox path — no permanent dual
    // mode. `deny_unknown_fields` makes a stored document still carrying a
    // `state` key fail parse, so the materializer HOLDS it (delayed-never-lost,
    // alertable) instead of silently activating an observe-only registration;
    // the migration is one jsonb key strip (the l5i9.19 runbook).
    for legacy in ["shadow", "live"] {
        let json = format!(
            r#"{{"schema-version":"0.1","registration-id":"x","package-id":"shop",
        "source-package-id":"shop","flow-id":"f","entity":"sales_orders","ops":["insert"],"state":"{legacy}"}}"#
        );
        assert!(EventRegistration::from_json(&json).is_err());
    }
}

#[test]
fn a_retired_partition_key_is_rejected_on_import() {
    let json = r#"{"schema-version":"0.1","registration-id":"x","package-id":"shop",
        "source-package-id":"shop","flow-id":"f","entity":"sales_orders","ops":["insert"],"partition-key":"new.id"}"#;
    assert!(
        EventRegistration::from_json(json).is_err(),
        "retired ordering vocabulary must fail closed, not be silently ignored"
    );
}

#[test]
fn source_package_identity_is_structurally_required() {
    let json = r#"{"schema-version":"0.1","registration-id":"x","package-id":"shop",
        "flow-id":"f","entity":"sales_orders","ops":["insert"]}"#;
    assert!(EventRegistration::from_json(json).is_err());
}

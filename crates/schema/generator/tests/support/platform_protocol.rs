use std::collections::BTreeMap;

use serde_json::{Value, json};
use wamn_schema_generator::{GenerateErrorKind, PackageManifest};

use super::{artifact, fixture};

fn cases(
    package: &wamn_schema_generator::GeneratedPackage,
    action: &str,
) -> BTreeMap<String, Value> {
    artifact(
        package,
        &format!("generated/contracts/widget/{action}.errors.json"),
    )["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| (case["literal"].as_str().unwrap().to_owned(), case.clone()))
        .collect()
}

#[test]
fn crud_errors_and_details_are_derived_from_action_and_catalog() {
    let package = fixture::generate_fixture();
    let common = [
        "internal_error",
        "invalid_input",
        "permission_denied",
        "retry",
        "timeout",
    ];
    for (action, extra) in [
        (
            "create",
            vec![
                "check_violation",
                // widget.maker_id names another model, so a write that points
                // at no maker is refused by the catalog's own foreign key.
                "foreign_key_violation",
                "idempotency_conflict",
                "unique_violation",
            ],
        ),
        ("get", vec!["not_found"]),
        ("query", vec![]),
        (
            "update",
            vec![
                "concurrency_conflict",
                "foreign_key_violation",
                "not_found",
                "unique_violation",
            ],
        ),
        ("delete", vec!["concurrency_conflict", "not_found"]),
    ] {
        let actual = cases(&package, action);
        let mut expected = common.into_iter().chain(extra).collect::<Vec<_>>();
        expected.sort_unstable();
        assert_eq!(
            actual.keys().map(String::as_str).collect::<Vec<_>>(),
            expected,
            "{action}"
        );

        assert_eq!(
            actual["permission_denied"]["detail"],
            json!({"required": ["operation"]})
        );
        assert_eq!(actual["retry"]["detail"], json!({}));
        assert_eq!(actual["timeout"]["detail"], json!({}));
        assert_eq!(actual["internal_error"]["detail"], json!({}));
        if action == "query" {
            assert_eq!(
                actual["invalid_input"]["detail"],
                json!({
                    "required": ["field"],
                    "optional": ["minimum", "maximum", "observed"]
                })
            );
        } else {
            assert_eq!(
                actual["invalid_input"]["detail"],
                json!({"required": ["field"]})
            );
        }
        if let Some(case) = actual.get("not_found") {
            assert_eq!(case["detail"], json!({"required": ["field", "id"]}));
        }
        if let Some(case) = actual.get("concurrency_conflict") {
            assert_eq!(
                case["detail"],
                json!({"required": ["expected_row_version", "observed_row_version"]})
            );
        }
        if let Some(case) = actual.get("idempotency_conflict") {
            assert_eq!(case["detail"], json!({"required": ["field"]}));
        }
        if let Some(case) = actual.get("check_violation") {
            assert_eq!(case["detail"], json!({"required": ["constraint"]}));
        }
    }
}

/// A refusal of a constraint that guards one written field names that field,
/// as an invalid input does, so a client marks its control (wamn-1dov).
#[test]
fn a_constraint_refusal_names_the_field_it_guards() {
    let package = fixture::generate_fixture();
    for (action, expected) in [
        (
            "create",
            vec![
                ("widget_code_check", None),
                ("widget_code_key", Some("code")),
                ("widget_id_pkey", None),
                ("widget_maker_id_fkey", Some("maker_id")),
            ],
        ),
        (
            "update",
            vec![
                ("widget_code_key", Some("change.code")),
                ("widget_maker_id_fkey", Some("change.maker_id")),
            ],
        ),
    ] {
        let contract = artifact(
            &package,
            &format!("generated/contracts/widget/{action}.errors.json"),
        );
        let named = contract["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|case| {
                let constraint = case["constraint"].as_str()?;
                let required = case["detail"]["required"].clone();
                Some((constraint.to_owned(), required))
            })
            .collect::<BTreeMap<_, _>>();
        let codec = String::from_utf8(
            package
                .file(&format!("generated/wit/widget_{action}_codec.rs"))
                .unwrap()
                .bytes()
                .to_vec(),
        )
        .unwrap();
        for (constraint, field) in &expected {
            let required = match field {
                Some(_) => json!(["field", "constraint"]),
                None => json!(["constraint"]),
            };
            assert_eq!(named[*constraint], required, "{action} {constraint}");
            let arm = format!("{constraint:?} => Some(");
            assert_eq!(
                codec.contains(&arm),
                field.is_some(),
                "{action} codec for {constraint}"
            );
            if let Some(field) = field {
                assert!(
                    codec.contains(&format!("{constraint:?} => Some({field:?})")),
                    "{action} codec names {field}"
                );
            }
        }
        assert_eq!(named.len(), expected.len(), "{action}");
    }
}

#[test]
fn crud_refuses_authored_protocol_metadata_but_custom_business_errors_remain_authored() {
    for (action, field, value) in [
        (
            "query",
            "filters",
            json!([{"field": "code", "binding": "json_array"}]),
        ),
        (
            "query",
            "sort",
            json!({
                "fields": ["created_at"], "directions": ["ascending", "descending"], "max_fields": 1
            }),
        ),
        (
            "query",
            "pagination",
            json!({
                "kind": "keyset",
                "cursor": {"version": 1, "payload": "canonical_compact_json",
                    "encoding": "base64url_unpadded", "opaque": true, "invalid": "invalid_input"},
                "default_sort": {"field": "created_at", "direction": "ascending"},
                "tie_breaker": {"field": "id"}
            }),
        ),
        (
            "query",
            "limit",
            json!({
                "default": 100, "minimum": 1, "maximum": 100, "invalid": "invalid_input"
            }),
        ),
        (
            "update",
            "error_details",
            json!({
                "not_found": {"required": ["field", "id"]}
            }),
        ),
    ] {
        let mut manifest = fixture::manifest();
        manifest["models"]["widget"]["operations"][action][field] = value;
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let error = PackageManifest::from_slice(&bytes)
            .expect_err("obsolete CRUD protocol metadata was accepted");
        assert_eq!(
            error.kind(),
            GenerateErrorKind::InvalidManifest,
            "{action}.{field}"
        );
    }

    let mut envelope = fixture::manifest();
    envelope["custom_operations"]["widget.archive"]["input"]["raw_body_maximum"] = json!(1_048_576);
    envelope["custom_operations"]["widget.archive"]["input"]["envelope"] =
        json!({"minimum": 1, "maximum": 100, "invalid": "invalid_input"});
    envelope["custom_operations"]["widget.archive"]["input"]["item_semantics"] = json!("per_input");
    assert_eq!(
        PackageManifest::from_slice(&serde_json::to_vec(&envelope).unwrap())
            .expect_err("obsolete fixed envelope protocol was accepted")
            .kind(),
        GenerateErrorKind::InvalidManifest
    );

    let package = fixture::generate_fixture();
    let custom = artifact(&package, "generated/contracts/widget/archive.errors.json");
    let business = custom["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["literal"] == "already_archived")
        .unwrap();
    assert_eq!(business["detail"]["required"], json!(["field"]));
    assert_eq!(business["from"], "transaction_invariant");
}

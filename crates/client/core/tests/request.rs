//! Request bytes and response validation through the shared client boundary.

use serde_json::{Value, json};
use wamn_client::descriptor::{FieldDescriptor, FieldSchema};
use wamn_client::request::{RequestErrorKind, build_request, validate_result, validate_schema};

const fn scalar(
    path: &'static str,
    type_name: &'static str,
    required: bool,
    nullable: bool,
) -> FieldSchema {
    FieldSchema {
        field: FieldDescriptor {
            path,
            type_name,
            nullable,
            values: &[],
        },
        required,
        children: &[],
        minimum: None,
        maximum: None,
    }
}

#[test]
fn required_and_nullable_form_four_independent_presence_contracts() {
    for (required, nullable) in [(true, false), (true, true), (false, false), (false, true)] {
        let fields = [scalar("field", "text", required, nullable)];
        let absent = build_request(&fields, &json!({}), None);
        if required {
            assert_eq!(
                absent.expect_err("required field is absent").kind(),
                RequestErrorKind::RequiredField
            );
        } else {
            assert_eq!(absent.expect("optional field stays absent").body(), b"[{}]");
        }
        let null = build_request(&fields, &json!({"field": null}), None);
        if nullable {
            assert_eq!(
                null.expect("nullable field admits null").body(),
                br#"[{"field":null}]"#
            );
        } else {
            assert_eq!(
                null.expect_err("null is not allowed").kind(),
                RequestErrorKind::NullNotAllowed
            );
        }
        assert_eq!(
            build_request(&fields, &json!({"field": "value"}), None)
                .expect("a present text value satisfies all four contracts")
                .body(),
            br#"[{"field":"value"}]"#
        );
    }
}

#[test]
fn supplier_omission_stays_absent_while_explicit_null_is_refused() {
    const FIELDS: &[FieldSchema] = &[
        scalar("request_id", "string", true, false),
        scalar("id", "uuid", true, false),
        scalar("expected_row_version", "int64", true, false),
        FieldSchema {
            children: &[scalar("change.supplier_id", "uuid", false, false)],
            ..scalar("change", "object", true, false)
        },
    ];
    let attachments: Value = serde_json::from_str(include_str!(
        "../../../../apps/wamn_receiving/publication/attachments.json"
    ))
    .expect("parse Receiving publication");
    let schema = &attachments["purchase-order-update-http"]["definition"]["input-schema"];
    let mut item = json!({"request_id":"r1", "id":"00000000-0000-0000-0000-000000000001",
        "expected_row_version": 4, "change": {}});
    assert_eq!(build_request(FIELDS, &item, Some(schema)).expect("omit supplier without clearing it").body(),
        br#"[{"change":{},"expected_row_version":"4","id":"00000000-0000-0000-0000-000000000001","request_id":"r1"}]"#);
    item["change"]["supplier_id"] = Value::Null;
    let error =
        build_request(FIELDS, &item, Some(schema)).expect_err("explicit null is invalid_input");
    assert_eq!(error.kind(), RequestErrorKind::NullNotAllowed);
    assert_eq!(error.path(), "$.change.supplier_id");
}

const RECEIPT_FIELDS: &[FieldSchema] = &[
    scalar("request_id", "string", true, false),
    FieldSchema {
        children: &[
            scalar("value.idempotency_key", "text", true, false),
            scalar("value.occurred_at", "timestamptz", true, false),
            FieldSchema {
                children: &[
                    scalar("value.line[].purchase_order_line_id", "uuid", true, false),
                    scalar("value.line[].quantity", "numeric", true, false),
                ],
                minimum: Some(1),
                maximum: Some(2),
                ..scalar("value.line[]", "array", true, false)
            },
        ],
        ..scalar("value", "object", true, false)
    },
];

fn receipt() -> Value {
    json!({"request_id": "r1", "value": {"idempotency_key": "intent-1",
    "occurred_at": "2026-09-08T08:30:00.123456-04:00", "line": [{
        "purchase_order_line_id": "ABCDEF00000000000000000000000001", "quantity": "+0005.000"
    }]}})
}

fn receipt_schema() -> Value {
    json!({"type":"array","minItems":1,"maxItems":1,"items":{
        "type":"object", "required":["request_id","value"],"additionalProperties":false,
        "properties":{"request_id":{"type":"string"},"value":{
            "type":"object", "required":["idempotency_key","occurred_at","line"], "additionalProperties":false,
            "properties": {
                "idempotency_key":{"type":"string","minLength":1},
                "occurred_at":{"type":"string","pattern":"Z$"},
                "line":{"type":"array","minItems":1,"maxItems":2,"items":{
                    "type":"object","required":["purchase_order_line_id","quantity"],"additionalProperties":false,
                    "properties":{
                        "purchase_order_line_id":{"type":"string","pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"},
                        "quantity":{"type":"string","pattern":"^[0-9]+(\\.[0-9]+)?$"}
                    }
                }}
            }
        }}
    }})
}

#[test]
fn nested_request_bytes_canonicalize_uuid_time_and_numeric_once() {
    let input = receipt();
    let built =
        build_request(RECEIPT_FIELDS, &input, Some(&receipt_schema())).expect("canonical request");
    assert_eq!(built.body(), br#"[{"request_id":"r1","value":{"idempotency_key":"intent-1","line":[{"purchase_order_line_id":"abcdef00-0000-0000-0000-000000000001","quantity":"5.000"}],"occurred_at":"2026-09-08T12:30:00.123456Z"}}]"#);
    assert_eq!(
        input,
        receipt(),
        "building never changes the editable draft"
    );
    assert_eq!(
        build_request(RECEIPT_FIELDS, built.item(), Some(&receipt_schema()))
            .expect("canonical values stay canonical")
            .body(),
        built.body()
    );
}

#[test]
fn nested_arrays_enforce_bounds_and_required_child_fields() {
    let mut item = receipt();
    item["value"]["line"] = json!([]);
    assert_eq!(
        build_request(RECEIPT_FIELDS, &item, None)
            .expect_err("too few lines")
            .kind(),
        RequestErrorKind::Bounds
    );
    item["value"]["line"] = json!([{}, {}, {}]);
    assert_eq!(
        build_request(RECEIPT_FIELDS, &item, None)
            .expect_err("too many lines")
            .kind(),
        RequestErrorKind::Bounds
    );
    item["value"]["line"] = json!([{}]);
    let error = build_request(RECEIPT_FIELDS, &item, None).expect_err("missing nested field");
    assert_eq!(error.kind(), RequestErrorKind::RequiredField);
    assert_eq!(error.path(), "$.value.line[0].purchase_order_line_id");
}

#[test]
fn integer_carriers_follow_the_served_schema() {
    let fields = [
        scalar("expected_row_version", "int64", true, false),
        scalar("limit", "int64", true, false),
    ];
    let schema = json!({"type":"array","items":{"type":"object","properties":{
        "expected_row_version":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100}
    }}});
    assert_eq!(
        build_request(
            &fields,
            &json!({"expected_row_version":4,"limit":"+0010"}),
            Some(&schema)
        )
        .expect("revision is text and limit is an integer")
        .body(),
        br#"[{"expected_row_version":"4","limit":10}]"#
    );
    assert_eq!(
        build_request(
            &fields,
            &json!({"expected_row_version":"4","limit":"101"}),
            Some(&schema)
        )
        .expect_err("served maximum is enforced")
        .kind(),
        RequestErrorKind::SchemaViolation
    );
    assert!(
        build_request(
            &[scalar("count", "int32", true, false)],
            &json!({"count":"2147483648"}),
            None
        )
        .is_err()
    );
}

#[test]
fn numeric_canonicalization_preserves_scale_without_a_precision_limit() {
    let fields = [scalar("quantity", "numeric", true, false)];
    for (input, expected) in [
        (
            "+000123456789012345678901234567890.000000",
            "123456789012345678901234567890.000000",
        ),
        ("-000.000", "0.000"),
        ("-.0500", "-0.0500"),
        ("0005.", "5"),
    ] {
        let built =
            build_request(&fields, &json!({"quantity":input}), None).expect("lexical numeric");
        assert_eq!(built.item()["quantity"], expected);
    }
    for input in [".", "+", "-", "1e3", "NaN", "Infinity", "1.2.3"] {
        assert!(
            build_request(&fields, &json!({"quantity":input}), None).is_err(),
            "accepted {input}"
        );
    }
    assert!(
        build_request(&fields, &json!({"quantity": 5.0}), None).is_err(),
        "numeric must preserve original text scale"
    );
}

#[test]
fn optional_unknown_types_block_and_unknown_keys_are_not_silently_dropped() {
    for name in [
        "bytes", "json", "opaque", "object", "array", "int16", "float32",
    ] {
        let error = build_request(
            &[scalar("unsupported", name, false, true)],
            &json!({}),
            None,
        )
        .expect_err("an omitted unsupported input still requires composition");
        assert_eq!(error.kind(), RequestErrorKind::UnsupportedType);
    }
    let error = build_request(
        &[scalar("known", "text", false, false)],
        &json!({"extra": "value"}),
        None,
    )
    .expect_err("unknown fields are refused");
    assert_eq!(error.kind(), RequestErrorKind::UnknownField);
    assert_eq!(error.path(), "$.extra");
}

#[test]
fn repeated_scalar_fields_enforce_their_closed_domain() {
    const FIELDS: &[FieldSchema] = &[FieldSchema {
        children: &[FieldSchema {
            field: FieldDescriptor {
                values: &["open", "closed"],
                ..scalar("status[]", "text", true, false).field
            },
            ..scalar("status[]", "text", true, false)
        }],
        minimum: Some(1),
        maximum: Some(2),
        ..scalar("status[]", "array", true, false)
    }];
    assert_eq!(
        build_request(FIELDS, &json!({"status":["open","closed"]}), None)
            .expect("valid scalar array")
            .body(),
        br#"[{"status":["open","closed"]}]"#
    );
    assert_eq!(
        build_request(FIELDS, &json!({"status":["other"]}), None)
            .expect_err("unknown choice")
            .kind(),
        RequestErrorKind::ClosedValue
    );
}

#[test]
fn schema_validation_enforces_constraints_and_never_loads_external_resources() {
    let schema = json!({"$defs":{"name":{"type":"string","minLength":3}},"$ref":"#/$defs/name"});
    validate_schema(&schema, &json!("known")).expect("document-local reference resolves");
    assert_eq!(
        validate_schema(&schema, &json!("no"))
            .expect_err("minLength is enforced")
            .kind(),
        RequestErrorKind::SchemaViolation
    );
    for reference in ["https://example.invalid/schema.json", "file:///etc/passwd"] {
        assert_eq!(
            validate_schema(&json!({"$ref":reference}), &Value::Null)
                .expect_err("external resource loading is disabled")
                .kind(),
            RequestErrorKind::InvalidSchema
        );
    }
    assert_eq!(
        validate_schema(&json!({"type":"not-a-type"}), &Value::Null)
            .expect_err("invalid schema is not a value refusal")
            .kind(),
        RequestErrorKind::InvalidSchema
    );
}

#[test]
fn results_preserve_opaque_fields_but_reject_corrupt_known_carriers() {
    const FIELDS: &[FieldSchema] = &[
        scalar("row_version", "int64", true, false),
        scalar("active", "boolean", true, false),
        scalar("extension", "opaque", false, true),
    ];
    let value = json!({"row_version":"4","active":true});
    assert!(!validate_result(FIELDS, &value).expect("known result"));
    for revision in [json!(4), json!(i64::MIN), json!(i64::MAX)] {
        let result = json!({"row_version":revision,"active":true});
        assert!(!validate_result(FIELDS, &result).expect("command result carries a numeric i64"));
        assert_eq!(
            result["row_version"], revision,
            "validation preserves the numeric result"
        );
    }
    assert!(
        validate_result(
            FIELDS,
            &json!({"row_version":"4","active":true,"extension":{"future":1}})
        )
        .expect("opaque declared field")
    );
    assert!(
        validate_result(
            FIELDS,
            &json!({"row_version":"4","active":true,"new_field":1})
        )
        .expect("additive result field")
    );
    for corrupt in [
        json!({"row_version":4.5,"active":true}),
        json!({"row_version":u64::MAX,"active":true}),
        json!({"row_version":"9223372036854775808","active":true}),
        json!({"row_version":"4","active":"true"}),
        json!({"row_version":null,"active":true}),
        json!({"active":true}),
    ] {
        assert!(
            validate_result(FIELDS, &corrupt).is_err(),
            "accepted corrupt result {corrupt}"
        );
    }
    assert_eq!(
        value,
        json!({"row_version":"4","active":true}),
        "validation never changes evidence"
    );
}

#[test]
fn opaque_result_items_still_enforce_the_declared_array_shape() {
    let fields = [FieldSchema {
        maximum: Some(2),
        ..scalar("items[]", "array", true, false)
    }];
    assert!(validate_result(&fields, &json!({"items": [1]})).expect("opaque item"));
    assert!(validate_result(&fields, &json!({"items": 1})).is_err());
    assert!(validate_result(&fields, &json!({"items": [1, 2, 3]})).is_err());
}

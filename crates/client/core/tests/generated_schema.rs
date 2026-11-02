//! The client agrees with the current generated fixture route schema.

use serde_json::{Value, json};
use wamn_client::descriptor::{FieldDescriptor, FieldSchema};
use wamn_client::request::{RequestErrorType, build_request};

const fn scalar(path: &'static str, type_name: &'static str) -> FieldSchema {
    FieldSchema {
        field: FieldDescriptor {
            path,
            type_name,
            nullable: false,
            values: &[],
        },
        required: true,
        children: &[],
        minimum: None,
        maximum: None,
    }
}

#[test]
#[ignore = "requires: env:WAMN_GENERATED_WIDGET_UPDATE_SCHEMA"]
fn generated_update_schema_agrees_with_client_presence_and_null_behavior() {
    const CHANGE: &[FieldSchema] = &[
        FieldSchema {
            required: false,
            ..scalar("change.code", "text")
        },
        FieldSchema {
            field: FieldDescriptor {
                nullable: true,
                ..scalar("change.note", "text").field
            },
            required: false,
            ..scalar("change.note", "text")
        },
    ];
    const FIELDS: &[FieldSchema] = &[
        scalar("request_id", "string"),
        scalar("id", "uuid"),
        scalar("expected_edit_version", "int32"),
        FieldSchema {
            children: CHANGE,
            ..scalar("change", "object")
        },
    ];
    let path = std::env::var_os("WAMN_GENERATED_WIDGET_UPDATE_SCHEMA")
        .filter(|value| !value.is_empty())
        .expect(
            "WAMN_GENERATED_WIDGET_UPDATE_SCHEMA names the current generated widget update schema",
        );
    let schema: Value =
        serde_json::from_slice(&std::fs::read(path).expect("read generated schema"))
            .expect("parse generated schema");
    let properties = &schema["items"]["properties"]["change"]["properties"];
    for (field, behavior) in [("code", "invalid_input"), ("note", "accepted")] {
        assert_eq!(properties[field]["x-wamn-explicit-null"], behavior);
    }
    let mut input = json!({"request_id":"r1","id":"00000000-0000-0000-0000-000000000001",
        "expected_edit_version":4,"change":{}});
    let absent = build_request(FIELDS, &input, Some(&schema)).expect("omit optional fields");
    assert_eq!(absent.request_element()["change"], json!({}));
    input["change"]["code"] = Value::Null;
    let error =
        build_request(FIELDS, &input, Some(&schema)).expect_err("refuse explicit code null");
    assert_eq!(error.error_type(), RequestErrorType::NullNotAllowed);
    assert_eq!(error.path(), "$.change.code");
    input["change"] = json!({"code":"priority","note":null});
    let present =
        build_request(FIELDS, &input, Some(&schema)).expect("accept code and nullable note");
    assert_eq!(present.request_element()["change"], input["change"]);
}

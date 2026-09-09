use serde_json::{Value, json};
use wamn_client::descriptor::{FieldDescriptor, FieldSchema};
use wamn_client_tui::draft::{Draft, DraftErrorKind, FieldState, InputKind, input_kind};

const fn field(path: &'static str, type_name: &'static str) -> FieldSchema {
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

const FIELDS: &[FieldSchema] = &[
    field("request_id", "string"),
    FieldSchema {
        children: &[
            field("value.idempotency_key", "text"),
            field("value.occurred_at", "timestamptz"),
            field("value.expected_revision", "int64"),
            field("value.supplier_id", "uuid"),
            FieldSchema {
                required: false,
                field: FieldDescriptor {
                    nullable: true,
                    ..field("value.note", "text").field
                },
                ..field("value.note", "text")
            },
            FieldSchema {
                minimum: Some(1),
                maximum: Some(3),
                children: &[
                    field("value.line[].code", "text"),
                    FieldSchema {
                        required: false,
                        field: FieldDescriptor {
                            nullable: true,
                            ..field("value.line[].note", "text").field
                        },
                        ..field("value.line[].note", "text")
                    },
                ],
                ..field("value.line[]", "array")
            },
        ],
        ..field("value", "object")
    },
];

#[test]
fn nested_and_repeated_fields_keep_absent_null_and_value_distinct() {
    let mut draft = Draft::new(FIELDS);
    assert_eq!(draft.state("/value/note").unwrap(), FieldState::Absent);
    draft.edit("/value/note", FieldState::Absent).unwrap();
    assert_eq!(draft.item(), &json!({}));
    draft.edit("/value/note", FieldState::Null).unwrap();
    assert_eq!(draft.state("/value/note").unwrap(), FieldState::Null);
    draft
        .edit("/value/note", FieldState::Value(json!("checked")))
        .unwrap();
    assert_eq!(
        draft.state("/value/note").unwrap(),
        FieldState::Value(json!("checked"))
    );
    draft.edit("/value/note", FieldState::Absent).unwrap();
    draft.insert_row("/value/line", 0, json!({})).unwrap();
    assert_eq!(
        draft.state("/value/line/0/note").unwrap(),
        FieldState::Absent
    );
    draft.edit("/value/line/0/note", FieldState::Null).unwrap();
    assert_eq!(draft.state("/value/line/0/note").unwrap(), FieldState::Null);
    draft
        .edit("/value/line/0/note", FieldState::Value(json!("counted")))
        .unwrap();
    assert_eq!(
        draft.item(),
        &json!({"value": {"line": [{"note": "counted"}]}})
    );
    draft
        .edit("/value/line/0/note", FieldState::Absent)
        .unwrap();
    assert_eq!(draft.item(), &json!({"value": {"line": [{}]}}));
}

#[test]
fn repeated_edits_enforce_bounds_and_fail_without_changing_the_draft() {
    let mut draft = Draft::new(FIELDS);
    draft
        .insert_row("/value/line", 0, json!({"code": "A"}))
        .unwrap();
    assert_eq!(
        draft.remove_row("/value/line", 0).unwrap_err().kind(),
        DraftErrorKind::RowBounds
    );
    draft
        .insert_row("/value/line", 1, json!({"code": "C"}))
        .unwrap();
    draft
        .insert_row("/value/line", 1, json!({"code": "B"}))
        .unwrap();
    let full = draft.item().clone();
    assert_eq!(
        draft
            .insert_row("/value/line", 3, json!({}))
            .unwrap_err()
            .kind(),
        DraftErrorKind::RowBounds
    );
    assert_eq!(draft.item(), &full);
    assert_eq!(
        draft.remove_row("/value/line", 5).unwrap_err().kind(),
        DraftErrorKind::RowIndex
    );
    assert_eq!(draft.item(), &full);
    draft.remove_row("/value/line", 1).unwrap();
    assert_eq!(
        draft.item().pointer("/value/line"),
        Some(&json!([{"code": "A"}, {"code": "C"}]))
    );

    let mut empty = Draft::new(FIELDS);
    assert_eq!(
        empty
            .edit("/value/line/2/code", FieldState::Value(json!("A")))
            .unwrap_err()
            .kind(),
        DraftErrorKind::RowIndex
    );
    assert_eq!(empty.item(), &json!({}));
}

#[test]
fn explicit_bindings_protect_only_declared_paths_and_their_containers() {
    let mut draft = Draft::new(FIELDS);
    for pointer in [
        "/request_id",
        "/value/idempotency_key",
        "/value/occurred_at",
        "/value/expected_revision",
    ] {
        draft.protect(pointer).unwrap();
        assert_eq!(
            draft
                .edit(pointer, FieldState::Value(json!("user value")))
                .unwrap_err()
                .kind(),
            DraftErrorKind::Protected
        );
    }
    draft.bind("/request_id", json!("request-1")).unwrap();
    draft
        .bind("/value/idempotency_key", json!("command-1"))
        .unwrap();
    draft
        .bind("/value/occurred_at", json!("2026-09-08T12:00:00Z"))
        .unwrap();
    draft.bind("/value/expected_revision", json!("4")).unwrap();
    let bound = draft.item().clone();
    for state in [
        FieldState::Absent,
        FieldState::Null,
        FieldState::Value(json!({})),
    ] {
        assert_eq!(
            draft.edit("/value", state).unwrap_err().kind(),
            DraftErrorKind::Protected
        );
        assert_eq!(draft.item(), &bound);
    }
    draft
        .edit(
            "/value/supplier_id",
            FieldState::Value(json!("123e4567-e89b-12d3-a456-426614174000")),
        )
        .unwrap();
    draft.bind("/value/expected_revision", json!("5")).unwrap();
    assert_eq!(
        draft.state("/value/expected_revision").unwrap(),
        FieldState::Value(json!("5"))
    );
}

#[test]
fn repeated_edits_preserve_the_identity_of_bound_rows() {
    let mut draft = Draft::new(FIELDS);
    draft
        .insert_row("/value/line", 0, json!({"code": "A"}))
        .unwrap();
    draft
        .insert_row("/value/line", 1, json!({"code": "B"}))
        .unwrap();
    draft.bind("/value/line/1/code", json!("B-bound")).unwrap();
    let bound = draft.item().clone();
    for index in [0, 1] {
        assert_eq!(
            draft
                .insert_row("/value/line", index, json!({}))
                .unwrap_err()
                .kind(),
            DraftErrorKind::Protected
        );
        assert_eq!(
            draft.remove_row("/value/line", index).unwrap_err().kind(),
            DraftErrorKind::Protected
        );
        assert_eq!(draft.item(), &bound);
    }
    draft
        .insert_row("/value/line", 2, json!({"code": "C"}))
        .unwrap();
    draft.remove_row("/value/line", 2).unwrap();
    assert_eq!(draft.item(), &bound);
    assert_eq!(
        draft
            .edit("/value/line/1", FieldState::Value(json!({})))
            .unwrap_err()
            .kind(),
        DraftErrorKind::Protected
    );
    draft
        .edit("/value/line/0/code", FieldState::Value(json!("A-edited")))
        .unwrap();
    assert_eq!(
        draft.state("/value/line/1/code").unwrap(),
        FieldState::Value(json!("B-bound"))
    );
}

#[test]
fn unsupported_fields_do_not_offer_or_accept_an_unrestricted_text_editor() {
    const INPUT: &[FieldSchema] = &[
        field("opaque", "opaque"),
        field("data", "bytes"),
        field("small", "int16"),
        field("untyped_array[]", "array"),
        FieldSchema {
            field: FieldDescriptor {
                values: &["open", "closed"],
                ..field("status", "text").field
            },
            ..field("status", "text")
        },
    ];
    let mut draft = Draft::new(INPUT);
    for schema in &INPUT[..4] {
        assert_eq!(input_kind(schema), InputKind::Unsupported);
        let pointer = format!("/{}", schema.field.path.trim_end_matches("[]"));
        assert_eq!(
            draft
                .edit(&pointer, FieldState::Value(json!("anything")))
                .unwrap_err()
                .kind(),
            DraftErrorKind::UnsupportedField
        );
    }
    assert_eq!(input_kind(&INPUT[4]), InputKind::Choice);
    assert_eq!(
        draft
            .insert_row("/untyped_array", 0, json!("anything"))
            .unwrap_err()
            .kind(),
        DraftErrorKind::UnsupportedField
    );
    assert_eq!(draft.item(), &json!({}));
}

#[test]
fn build_checks_presence_nullability_and_types_before_producing_request_bytes() {
    const INPUT: &[FieldSchema] = &[
        field("request_id", "string"),
        field("count", "int64"),
        FieldSchema {
            required: false,
            ..field("optional", "text")
        },
        FieldSchema {
            field: FieldDescriptor {
                nullable: true,
                ..field("nullable", "text").field
            },
            ..field("nullable", "text")
        },
        FieldSchema {
            required: false,
            field: FieldDescriptor {
                nullable: true,
                ..field("optional_nullable", "text").field
            },
            ..field("optional_nullable", "text")
        },
    ];
    let mut draft = Draft::new(INPUT);
    draft.protect("/request_id").unwrap();
    draft.bind("/request_id", json!("request-1")).unwrap();
    assert!(draft.build(None).is_err());
    draft.edit("/count", FieldState::Null).unwrap();
    draft.edit("/nullable", FieldState::Null).unwrap();
    assert!(draft.build(None).is_err());
    draft
        .edit("/count", FieldState::Value(json!("bad integer")))
        .unwrap();
    assert!(draft.build(None).is_err());
    draft.edit("/count", FieldState::Value(json!("7"))).unwrap();
    let built = draft.build(None).unwrap();
    assert_eq!(
        built.item(),
        &json!({"request_id": "request-1", "count": "7", "nullable": null})
    );
    assert_eq!(
        serde_json::from_slice::<Value>(built.body()).unwrap(),
        json!([built.item()])
    );
    assert!(
        draft
            .build(Some(&json!({"type": "array", "maxItems": 0})))
            .is_err()
    );
    draft.edit("/optional", FieldState::Null).unwrap();
    assert!(draft.build(None).is_err());
    draft.edit("/optional", FieldState::Absent).unwrap();
    draft.edit("/optional_nullable", FieldState::Null).unwrap();
    assert_eq!(
        draft.build(None).unwrap().item().get("optional_nullable"),
        Some(&Value::Null)
    );
    draft.edit("/nullable", FieldState::Absent).unwrap();
    assert!(draft.build(None).is_err());
}

#[test]
fn json_pointer_escaping_and_scalar_array_items_are_editable() {
    const INPUT: &[FieldSchema] = &[
        field("a/b~c", "text"),
        FieldSchema {
            children: &[field("tags[]", "text")],
            ..field("tags[]", "array")
        },
    ];
    let mut draft = Draft::new(INPUT);
    draft
        .edit("/a~1b~0c", FieldState::Value(json!("escaped")))
        .unwrap();
    assert_eq!(draft.item().get("a/b~c"), Some(&json!("escaped")));
    assert_eq!(
        draft.edit("/a~2b", FieldState::Null).unwrap_err().kind(),
        DraftErrorKind::InvalidPointer
    );
    draft.insert_row("/tags", 0, json!("first")).unwrap();
    draft
        .edit("/tags/0", FieldState::Value(json!("changed")))
        .unwrap();
    assert_eq!(
        draft.state("/tags/0").unwrap(),
        FieldState::Value(json!("changed"))
    );
    assert_eq!(
        draft
            .edit("/tags/0", FieldState::Absent)
            .unwrap_err()
            .kind(),
        DraftErrorKind::InvalidShape
    );
    draft.remove_row("/tags", 0).unwrap();
    assert_eq!(draft.item().get("tags"), Some(&json!([])));
}

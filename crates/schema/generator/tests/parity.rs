use serde_json::{Value, json};
use wamn_schema_generator::{ParityErrorKind, validate_parity_json};

fn matrix() -> Value {
    json!({
        "model": "parity_subject",
        "rule": "same_sql_file_two_projection_structs",
        "fields": [
            {
                "field": "active",
                "postgres": "boolean",
                "wamn_sql_value": "boolean",
                "native_rust": "bool",
                "wamn_rust": "bool",
                "nullable": false
            },
            {
                "field": "sequence",
                "postgres": "int4",
                "wamn_sql_value": "int32",
                "native_rust": "Option<i32>",
                "wamn_rust": "Option<i32>",
                "nullable": true
            },
            {
                "field": "revision",
                "postgres": "int8",
                "wamn_sql_value": "int64",
                "native_rust": "i64",
                "wamn_rust": "i64",
                "nullable": false
            },
            {
                "field": "ratio",
                "postgres": "float8",
                "wamn_sql_value": "float64",
                "native_rust": "f64",
                "wamn_rust": "f64",
                "nullable": false
            },
            {
                "field": "label",
                "postgres": "text",
                "wamn_sql_value": "text",
                "native_rust": "String",
                "wamn_rust": "String",
                "nullable": false
            },
            {
                "field": "payload",
                "postgres": "bytea",
                "wamn_sql_value": "bytes",
                "native_rust": "Vec<u8>",
                "wamn_rust": "Vec<u8>",
                "nullable": false
            },
            {
                "field": "quantity",
                "postgres": "numeric",
                "wamn_sql_value": "numeric",
                "native_rust": "rust_decimal::Decimal",
                "wamn_rust": "wamn_postgres_sqlx::Numeric",
                "nullable": false
            },
            {
                "field": "occurred_at",
                "postgres": "timestamptz",
                "wamn_sql_value": "timestamptz",
                "native_rust": "chrono::DateTime<chrono::Utc>",
                "wamn_rust": "wamn_postgres_sqlx::TimestampTz",
                "nullable": false
            },
            {
                "field": "document",
                "postgres": "jsonb",
                "wamn_sql_value": "json",
                "native_rust": "serde_json::Value",
                "wamn_rust": "wamn_postgres_sqlx::Json",
                "nullable": false
            },
            {
                "field": "id",
                "postgres": "uuid",
                "wamn_sql_value": "uuid",
                "native_rust": "uuid::Uuid",
                "wamn_rust": "wamn_postgres_sqlx::Uuid",
                "nullable": false
            }
        ],
        "accessor_binds": [
            {
                "accessor": "get",
                "parameter": "id",
                "postgres": "uuid",
                "nullable": false,
                "native_rust": "uuid::Uuid",
                "wamn_rust": "wamn_postgres_sqlx::Uuid"
            },
            {
                "accessor": "query_created_at_ascending",
                "parameter": "cursor_key",
                "postgres": "timestamptz",
                "nullable": true,
                "native_rust": "Option<chrono::DateTime<chrono::Utc>>",
                "wamn_rust": "Option<wamn_postgres_sqlx::TimestampTz>"
            },
            {
                "accessor": "update",
                "parameter": "supplier_id_present",
                "postgres": "boolean",
                "nullable": false,
                "native_rust": "bool",
                "wamn_rust": "bool"
            }
        ]
    })
}

fn validate(value: &Value) -> Result<(), wamn_schema_generator::ParityError> {
    validate_parity_json(&serde_json::to_vec(value).unwrap())
}

#[test]
fn frozen_postgres_native_and_wamn_mappings_validate() {
    validate(&matrix()).unwrap();
}

#[test]
fn distinct_wamn_carrier_mutants_fail_with_field_context() {
    for (index, field) in [
        (6, "parity_subject.quantity"),
        (7, "parity_subject.occurred_at"),
        (8, "parity_subject.document"),
        (9, "parity_subject.id"),
    ] {
        let mut mutant = matrix();
        mutant["fields"][index]["wamn_rust"] = json!("String");

        let error = validate(&mutant).unwrap_err();
        assert_eq!(error.kind(), ParityErrorKind::TypeMismatch);
        assert_eq!(error.field(), Some(field));
    }
}

#[test]
fn nullability_mutant_fails_with_field_context() {
    let mut mutant = matrix();
    mutant["fields"][9]["nullable"] = json!(true);

    let error = validate(&mutant).unwrap_err();
    assert_eq!(error.kind(), ParityErrorKind::TypeMismatch);
    assert_eq!(error.field(), Some("parity_subject.id"));
}

#[test]
fn accessor_bind_mutants_fail_with_parameter_context() {
    let mut wrong_wamn = matrix();
    wrong_wamn["accessor_binds"][1]["wamn_rust"] = json!("Option<String>");
    let error = validate(&wrong_wamn).unwrap_err();
    assert_eq!(error.kind(), ParityErrorKind::TypeMismatch);
    assert_eq!(
        error.field(),
        Some("parity_subject.query_created_at_ascending(cursor_key)")
    );

    let mut wrong_native = matrix();
    wrong_native["accessor_binds"][0]["native_rust"] = json!("String");
    let error = validate(&wrong_native).unwrap_err();
    assert_eq!(error.kind(), ParityErrorKind::TypeMismatch);
    assert_eq!(error.field(), Some("parity_subject.get(id)"));

    let mut duplicate = matrix();
    let repeated = duplicate["accessor_binds"][0].clone();
    duplicate["accessor_binds"]
        .as_array_mut()
        .unwrap()
        .push(repeated);
    let error = validate(&duplicate).unwrap_err();
    assert_eq!(error.kind(), ParityErrorKind::InvalidMatrix);
    assert_eq!(error.field(), Some("parity_subject.get(id)"));
}

#[test]
fn unknown_matrix_members_and_postgres_types_fail_closed() {
    let mut unknown_member = matrix();
    unknown_member["fields"][0]["future"] = json!(true);
    assert_eq!(
        validate(&unknown_member).unwrap_err().kind(),
        ParityErrorKind::InvalidJson
    );

    let mut unknown_type = matrix();
    unknown_type["fields"][0]["postgres"] = json!("varchar");
    let error = validate(&unknown_type).unwrap_err();
    assert_eq!(error.kind(), ParityErrorKind::UnsupportedPostgresType);
    assert_eq!(error.field(), Some("parity_subject.active"));
}

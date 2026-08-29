use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use wamn_schema_generator::{
    AuthoredSql, GenerateErrorKind, GeneratedPackage, GenerationInput, GenerationProvenance,
    corpus_sha256, generate, validate_parity_json,
};
use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnDefault, ColumnType, Constraint, ForeignKeyAction, ForeignKeyColumn,
    Table,
};

const QUERY_SOURCES: [AuthoredSql<'static>; 6] = [
    AuthoredSql::new(
        "query/open_purchase_order_by_purchase_order_number_ascending.sql",
        b"SELECT 1 /* purchase_order_number ascending */;\n",
    ),
    AuthoredSql::new(
        "query/open_purchase_order_by_purchase_order_number_descending.sql",
        b"SELECT 1 /* purchase_order_number descending */;\n",
    ),
    AuthoredSql::new(
        "query/open_purchase_order_by_status_ascending.sql",
        b"SELECT 1 /* status ascending */;\n",
    ),
    AuthoredSql::new(
        "query/open_purchase_order_by_status_descending.sql",
        b"SELECT 1 /* status descending */;\n",
    ),
    AuthoredSql::new(
        "query/open_purchase_order.sql",
        b"SELECT 1 /* created_at ascending */;\n",
    ),
    AuthoredSql::new(
        "query/open_purchase_order_by_created_at_descending.sql",
        b"SELECT 1 /* created_at descending */;\n",
    ),
];

const RECEIVING_MANIFEST: &[u8] = include_bytes!("../../../../packages/receiving/wamn.json");
const RECEIVING_SOURCES: [AuthoredSql<'static>; 6] = [
    AuthoredSql::new(
        "query/open_purchase_order_by_purchase_order_number_ascending.sql",
        include_bytes!(
            "../../../../packages/receiving/query/open_purchase_order_by_purchase_order_number_ascending.sql"
        ),
    ),
    AuthoredSql::new(
        "query/open_purchase_order_by_purchase_order_number_descending.sql",
        include_bytes!(
            "../../../../packages/receiving/query/open_purchase_order_by_purchase_order_number_descending.sql"
        ),
    ),
    AuthoredSql::new(
        "query/open_purchase_order_by_status_ascending.sql",
        include_bytes!(
            "../../../../packages/receiving/query/open_purchase_order_by_status_ascending.sql"
        ),
    ),
    AuthoredSql::new(
        "query/open_purchase_order_by_status_descending.sql",
        include_bytes!(
            "../../../../packages/receiving/query/open_purchase_order_by_status_descending.sql"
        ),
    ),
    AuthoredSql::new(
        "query/open_purchase_order.sql",
        include_bytes!("../../../../packages/receiving/query/open_purchase_order.sql"),
    ),
    AuthoredSql::new(
        "query/open_purchase_order_by_created_at_descending.sql",
        include_bytes!(
            "../../../../packages/receiving/query/open_purchase_order_by_created_at_descending.sql"
        ),
    ),
];

fn catalog(add_unused_table: bool) -> CatalogIr {
    let purchase_order = Table::new(
        "receiving",
        "purchase_order",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("purchase_order_number", ColumnType::Text, false, None, None),
            Column::new("supplier_id", ColumnType::Uuid, false, None, None),
            Column::new(
                "status",
                ColumnType::Text,
                false,
                Some(ColumnDefault::TextOpen),
                None,
            ),
            Column::new(
                "row_version",
                ColumnType::Int64,
                false,
                Some(ColumnDefault::Int64One),
                None,
            ),
            Column::new(
                "created_at",
                ColumnType::Timestamptz,
                false,
                Some(ColumnDefault::CurrentTimestamp),
                None,
            ),
        ],
        vec![
            Constraint::primary_key("purchase_order_id_pkey", ["id"]).unwrap(),
            Constraint::unique(
                "purchase_order_purchase_order_number_key",
                ["purchase_order_number"],
            )
            .unwrap(),
            Constraint::check(
                "purchase_order_status_check",
                "status = ANY (ARRAY['open'::text, 'complete'::text, 'cancelled'::text])",
            )
            .unwrap(),
        ],
        Vec::new(),
    );
    let mut tables = vec![purchase_order];
    if add_unused_table {
        tables.push(Table::new(
            "receiving",
            "unused",
            vec![Column::new("id", ColumnType::Uuid, false, None, None)],
            vec![Constraint::primary_key("unused_id_pkey", ["id"]).unwrap()],
            Vec::new(),
        ));
    }
    CatalogIr::new(tables)
}

fn receiving_catalog() -> CatalogIr {
    let purchase_order = Table::new(
        "receiving",
        "purchase_order",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("purchase_order_number", ColumnType::Text, false, None, None),
            Column::new("supplier_id", ColumnType::Uuid, false, None, None),
            Column::new(
                "status",
                ColumnType::Text,
                false,
                Some(ColumnDefault::TextOpen),
                None,
            ),
            Column::new(
                "row_version",
                ColumnType::Int64,
                false,
                Some(ColumnDefault::Int64One),
                None,
            ),
            Column::new(
                "created_at",
                ColumnType::Timestamptz,
                false,
                Some(ColumnDefault::CurrentTimestamp),
                None,
            ),
            Column::new(
                "updated_at",
                ColumnType::Timestamptz,
                false,
                Some(ColumnDefault::CurrentTimestamp),
                None,
            ),
        ],
        vec![
            Constraint::primary_key("purchase_order_id_pkey", ["id"]).unwrap(),
            Constraint::unique(
                "purchase_order_purchase_order_number_key",
                ["purchase_order_number"],
            )
            .unwrap(),
            Constraint::check(
                "purchase_order_status_check",
                "status = ANY (ARRAY['open'::text, 'complete'::text, 'cancelled'::text])",
            )
            .unwrap(),
        ],
        Vec::new(),
    );
    let receipt = Table::new(
        "receiving",
        "receipt",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("idempotency_key", ColumnType::Text, false, None, None),
            Column::new("purchase_order_id", ColumnType::Uuid, false, None, None),
            Column::new("receipt_reference", ColumnType::Text, false, None, None),
            Column::new("occurred_at", ColumnType::Timestamptz, false, None, None),
            Column::new(
                "created_at",
                ColumnType::Timestamptz,
                false,
                Some(ColumnDefault::CurrentTimestamp),
                None,
            ),
        ],
        vec![
            Constraint::primary_key("receipt_id_pkey", ["id"]).unwrap(),
            Constraint::unique("receipt_idempotency_key_key", ["idempotency_key"]).unwrap(),
            Constraint::foreign_key(
                "receipt_purchase_order_id_fkey",
                vec![ForeignKeyColumn::new("purchase_order_id", "id")],
                "receiving",
                "purchase_order",
                ForeignKeyAction::NoAction,
                ForeignKeyAction::NoAction,
            )
            .unwrap(),
            Constraint::unique(
                "receipt_purchase_order_id_receipt_reference_key",
                ["purchase_order_id", "receipt_reference"],
            )
            .unwrap(),
        ],
        Vec::new(),
    );
    CatalogIr::new(vec![purchase_order, receipt])
}

fn manifest() -> Value {
    json!({
        "package": {"id": "wamn_receiving", "version": "1.0.0"},
        "required_platform_policy_contract": {
            "id": "receiving_data_access",
            "state": "unsatisfied"
        },
        "models": {
            "purchase_order": {
                "schema": "receiving",
                "table": "purchase_order",
                "owner": "wamn_receiving",
                "server_owned_fields": [
                    "id", "purchase_order_number", "status", "row_version", "created_at"
                ],
                "enum_fields": {"status": ["open", "complete", "cancelled"]},
                "operations": {
                    "get": {"permission": "purchase_order.get", "result": "one"},
                    "query": {
                        "permission": "purchase_order.query",
                        "authored_sql": {
                            "default": "query/open_purchase_order.sql",
                            "variants": [
                                {"field": "purchase_order_number", "direction": "ascending", "path": "query/open_purchase_order_by_purchase_order_number_ascending.sql"},
                                {"field": "purchase_order_number", "direction": "descending", "path": "query/open_purchase_order_by_purchase_order_number_descending.sql"},
                                {"field": "status", "direction": "ascending", "path": "query/open_purchase_order_by_status_ascending.sql"},
                                {"field": "status", "direction": "descending", "path": "query/open_purchase_order_by_status_descending.sql"},
                                {"field": "created_at", "direction": "ascending", "path": "query/open_purchase_order.sql"},
                                {"field": "created_at", "direction": "descending", "path": "query/open_purchase_order_by_created_at_descending.sql"}
                            ]
                        },
                        "filters": [
                            {"field": "supplier_id", "binding": "json_array"},
                            {"field": "status", "binding": "json_array"}
                        ],
                        "sort": {
                            "fields": ["purchase_order_number", "status", "created_at"],
                            "directions": ["ascending", "descending"],
                            "max_fields": 1
                        },
                        "pagination": {
                            "kind": "keyset",
                            "cursor": {
                                "version": 1,
                                "payload": "canonical_compact_json",
                                "encoding": "base64url_unpadded",
                                "opaque": true,
                                "invalid": "invalid_input"
                            },
                            "default_sort": {"field": "created_at", "direction": "ascending"},
                            "tie_breaker": {"field": "id"}
                        },
                        "limit": {
                            "default": 100,
                            "minimum": 1,
                            "maximum": 100,
                            "invalid": "invalid_input"
                        },
                        "result": "page"
                    },
                    "update": {
                        "permission": "purchase_order.update",
                        "writable_fields": ["supplier_id"],
                        "revision_field": "row_version",
                        "result": "one"
                    }
                }
            }
        },
        "connections": {"postgres": {"interface": "wamn:postgres@0.1.0"}},
        "components": {
            "receiving_data": {
                "operations": [
                    "purchase_order.get", "purchase_order.query", "purchase_order.update"
                ],
                "connections": ["postgres"]
            }
        }
    })
}

fn run(
    catalog: &CatalogIr,
    manifest: &Value,
    sources: &[AuthoredSql<'_>],
) -> Result<GeneratedPackage, wamn_schema_generator::GenerateError> {
    let bytes = serde_json::to_vec(manifest).unwrap();
    generate(&GenerationInput::new(
        catalog,
        &bytes,
        sources,
        GenerationProvenance::new(
            "0123456789abcdef",
            "wamn-schema-generator/0.1.0",
            "rust-1.89",
        ),
    ))
}

fn artifact_json(package: &GeneratedPackage, path: &str) -> Value {
    serde_json::from_slice(package.file(path).unwrap().bytes()).unwrap()
}

fn wamn_accessor<'a>(source_map: &'a Value, name: &str) -> &'a Value {
    source_map["wamn_api"]["accessors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|accessor| accessor["name"] == name)
        .unwrap()
}

fn accessor_bind(
    parameter: &str,
    postgres: &str,
    nullable: bool,
    native_rust: &str,
    wamn_rust: &str,
) -> Value {
    json!({
        "parameter": parameter,
        "postgres": postgres,
        "nullable": nullable,
        "native_rust": native_rust,
        "wamn_rust": wamn_rust,
    })
}

fn assert_native_fixtures_match_parity(package: &GeneratedPackage, model: &str) {
    let source_map = artifact_json(package, &format!("generated/source-map/{model}.json"));
    let parity = artifact_json(package, &format!("generated/parity/{model}.json"));
    let parity_binds = parity["accessor_binds"].as_array().unwrap();
    let fixtures = source_map["native_bind_fixtures"].as_array().unwrap();
    assert_eq!(fixtures.len(), parity_binds.len());
    for bind in parity_binds {
        let accessor = bind["accessor"].as_str().unwrap();
        let parameter = bind["parameter"].as_str().unwrap();
        let fixture = fixtures
            .iter()
            .find(|fixture| fixture["accessor"] == accessor && fixture["parameter"] == parameter)
            .unwrap();
        assert_eq!(
            fixture,
            &json!({
                "accessor": accessor,
                "parameter": parameter,
                "function": format!("{accessor}_{parameter}_bind_fixture"),
                "visibility": "crate",
                "type": bind["native_rust"],
            })
        );
    }
}

#[test]
fn strict_manifest_and_ir_references_fail_loudly() {
    let ir = catalog(false);
    let mut unknown = manifest();
    unknown["future"] = json!(true);
    assert_eq!(
        run(&ir, &unknown, &QUERY_SOURCES).unwrap_err().kind(),
        GenerateErrorKind::InvalidManifest
    );

    let mut mismatch = manifest();
    mismatch["models"]["purchase_order"]["table"] = json!("missing");
    let error = run(&ir, &mismatch, &QUERY_SOURCES).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::UnknownRelation);
    assert_eq!(error.object(), Some("receiving.missing"));

    let mut unknown_action = manifest();
    unknown_action["models"]["purchase_order"]["operations"]["merge"] =
        json!({"permission": "purchase_order.merge", "result": "one"});
    assert_eq!(
        run(&ir, &unknown_action, &QUERY_SOURCES)
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidManifest
    );
}

#[test]
fn mutation_contract_refuses_server_owned_and_nonnullable_null() {
    let ir = catalog(false);
    let mut server_owned = manifest();
    server_owned["models"]["purchase_order"]["operations"]["update"]["writable_fields"] =
        json!(["status"]);
    assert_eq!(
        run(&ir, &server_owned, &QUERY_SOURCES).unwrap_err().kind(),
        GenerateErrorKind::InvalidOperation
    );

    let package = run(&ir, &manifest(), &QUERY_SOURCES).unwrap();
    let input = artifact_json(
        &package,
        "generated/contracts/purchase_order/update.input.json",
    );
    assert_eq!(input["writable_fields"][0]["field"], "supplier_id");
    assert_eq!(
        input["writable_fields"][0]["explicit_null"],
        "invalid_input"
    );
    assert_eq!(input["server_owned_fields"]["if_supplied"], "invalid_input");
}

#[test]
fn operation_identity_errors_and_constraint_names_are_closed() {
    let package = run(&catalog(false), &manifest(), &QUERY_SOURCES).unwrap();
    let operation = artifact_json(
        &package,
        "generated/contracts/purchase_order/update.operation.json",
    );
    assert_eq!(operation["permission_token"], "purchase_order.update");
    assert_eq!(
        operation["grant"],
        "wamn_receiving@1.0.0::purchase_order.update"
    );
    assert_eq!(operation["automatic_retry"], false);

    let errors = artifact_json(
        &package,
        "generated/contracts/purchase_order/update.errors.json",
    );
    assert_eq!(errors["closed"], true);
    let cases = errors["cases"].as_array().unwrap();
    assert!(cases.iter().any(|case| case["literal"] == "invalid_input"));
    assert!(
        cases
            .iter()
            .any(|case| case["literal"] == "concurrency_conflict")
    );
    assert!(cases.iter().any(|case| {
        case["literal"] == "retry"
            && case["from"] == json!(["serialization_failure", "connection_unavailable"])
            && case["automatic"] == false
    }));
    assert!(
        cases
            .iter()
            .any(|case| { case["literal"] == "timeout" && case["from"] == "statement_timeout" })
    );
    assert!(cases.iter().any(|case| {
        case["literal"] == "permission_denied" && case["from"] == "permission_denied"
    }));
    assert!(cases.iter().any(|case| {
        case["literal"] == "internal_error"
            && case["from"] == json!(["query_error", "row_limit_exceeded"])
            && case["detail"] == "opaque"
    }));
    let named_constraint_cases = cases
        .iter()
        .filter(|case| case.get("constraint").is_some())
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(named_constraint_cases, Vec::<Value>::new());
}

#[test]
fn generation_is_byte_stable_and_emits_both_projection_siblings() {
    let ir = catalog(false);
    let first = run(&ir, &manifest(), &QUERY_SOURCES).unwrap();
    let second = run(&ir, &manifest(), &QUERY_SOURCES).unwrap();
    assert_eq!(first, second);
    assert!(
        first
            .files()
            .windows(2)
            .all(|pair| pair[0].path() < pair[1].path())
    );

    first
        .file("generated/native-verifier/purchase_order.rs")
        .unwrap();
    first.file("generated/wamn/purchase_order.rs").unwrap();
    assert!(first.file("query/open_purchase_order.sql").is_none());

    let parity_file = first.file("generated/parity/purchase_order.json").unwrap();
    validate_parity_json(parity_file.bytes()).unwrap();
    let parity: Value = serde_json::from_slice(parity_file.bytes()).unwrap();
    assert_eq!(parity["rule"], "same_sql_file_two_projection_structs");
    let source_map = artifact_json(&first, "generated/source-map/purchase_order.json");
    assert_eq!(
        source_map["relation"],
        "catalog-ir://receiving.purchase_order"
    );
}

#[test]
fn wamn_accessors_are_structurally_derived_from_operations_and_ir() {
    let package = run(&catalog(false), &manifest(), &QUERY_SOURCES).unwrap();
    let source_map = artifact_json(&package, "generated/source-map/purchase_order.json");
    let api = &source_map["wamn_api"];
    assert_eq!(api["sql_constant_visibility"], "crate");
    assert_eq!(
        api["mutation_constraints"],
        json!([{
            "operation": "update",
            "unique": {
                "constant": "UPDATE_UNIQUE_CONSTRAINTS",
                "visibility": "crate",
                "names": []
            },
            "foreign_key": {
                "constant": "UPDATE_FOREIGN_KEY_CONSTRAINTS",
                "visibility": "crate",
                "names": []
            },
            "check": {
                "constant": "UPDATE_CHECK_CONSTRAINTS",
                "visibility": "crate",
                "names": []
            }
        }])
    );
    assert_eq!(api["accessors"].as_array().unwrap().len(), 8);
    assert_eq!(
        wamn_accessor(&source_map, "get"),
        &json!({
            "name": "get",
            "visibility": "crate",
            "operation": "get",
            "sql_constant": "GET_SQL",
            "row": "PurchaseOrderRow",
            "fetch": "optional",
            "binds": [
                accessor_bind(
                    "id",
                    "uuid",
                    false,
                    "uuid::Uuid",
                    "wamn_postgres_sqlx::Uuid"
                )
            ]
        })
    );

    for (name, sql_constant, cursor_postgres, native_cursor, wamn_cursor) in [
        (
            "query_purchase_order_number_ascending",
            "QUERY_0_SQL",
            "text",
            "Option<String>",
            "Option<String>",
        ),
        (
            "query_purchase_order_number_descending",
            "QUERY_1_SQL",
            "text",
            "Option<String>",
            "Option<String>",
        ),
        (
            "query_status_ascending",
            "QUERY_2_SQL",
            "text",
            "Option<String>",
            "Option<String>",
        ),
        (
            "query_status_descending",
            "QUERY_3_SQL",
            "text",
            "Option<String>",
            "Option<String>",
        ),
        (
            "query_created_at_ascending",
            "QUERY_4_SQL",
            "timestamptz",
            "Option<chrono::DateTime<chrono::Utc>>",
            "Option<wamn_postgres_sqlx::TimestampTz>",
        ),
        (
            "query_created_at_descending",
            "QUERY_5_SQL",
            "timestamptz",
            "Option<chrono::DateTime<chrono::Utc>>",
            "Option<wamn_postgres_sqlx::TimestampTz>",
        ),
    ] {
        assert_eq!(
            wamn_accessor(&source_map, name),
            &json!({
                "name": name,
                "visibility": "crate",
                "operation": "query",
                "sql_constant": sql_constant,
                "row": "PurchaseOrderRow",
                "fetch": "all",
                "binds": [
                    accessor_bind(
                        "supplier_id_filter",
                        "jsonb",
                        true,
                        "Option<serde_json::Value>",
                        "Option<wamn_postgres_sqlx::Json>"
                    ),
                    accessor_bind(
                        "status_filter",
                        "jsonb",
                        true,
                        "Option<serde_json::Value>",
                        "Option<wamn_postgres_sqlx::Json>"
                    ),
                    accessor_bind("cursor_key", cursor_postgres, true, native_cursor, wamn_cursor),
                    accessor_bind(
                        "cursor_id",
                        "uuid",
                        true,
                        "Option<uuid::Uuid>",
                        "Option<wamn_postgres_sqlx::Uuid>"
                    ),
                    accessor_bind("limit", "int8", false, "i64", "i64")
                ]
            })
        );
    }

    assert_eq!(
        wamn_accessor(&source_map, "update"),
        &json!({
            "name": "update",
            "visibility": "crate",
            "operation": "update",
            "sql_constant": "UPDATE_SQL",
            "row": "PurchaseOrderUpdateRow",
            "fetch": "one",
            "binds": [
                accessor_bind("id", "uuid", false, "uuid::Uuid", "wamn_postgres_sqlx::Uuid"),
                accessor_bind("expected_row_version", "int8", false, "i64", "i64"),
                accessor_bind("supplier_id_present", "boolean", false, "bool", "bool"),
                accessor_bind(
                    "supplier_id_value",
                    "uuid",
                    true,
                    "Option<uuid::Uuid>",
                    "Option<wamn_postgres_sqlx::Uuid>"
                )
            ]
        })
    );
    assert_eq!(
        api["operation_rows"],
        json!([{
            "name": "PurchaseOrderUpdateRow",
            "visibility": "public",
            "fields": [
                {"name": "outcome", "type": "Option<String>"},
                {"name": "created_at", "type": "Option<wamn_postgres_sqlx::TimestampTz>"},
                {"name": "id", "type": "Option<wamn_postgres_sqlx::Uuid>"},
                {"name": "purchase_order_number", "type": "Option<String>"},
                {"name": "row_version", "type": "Option<i64>"},
                {"name": "status", "type": "Option<String>"},
                {"name": "supplier_id", "type": "Option<wamn_postgres_sqlx::Uuid>"}
            ]
        }])
    );
    assert_eq!(
        source_map["native_operation_rows"],
        json!([{
            "name": "PurchaseOrderUpdateRow",
            "visibility": "public",
            "fields": [
                {"name": "outcome", "type": "Option<String>"},
                {"name": "created_at", "type": "Option<chrono::DateTime<chrono::Utc>>"},
                {"name": "id", "type": "Option<uuid::Uuid>"},
                {"name": "purchase_order_number", "type": "Option<String>"},
                {"name": "row_version", "type": "Option<i64>"},
                {"name": "status", "type": "Option<String>"},
                {"name": "supplier_id", "type": "Option<uuid::Uuid>"}
            ]
        }])
    );

    assert_native_fixtures_match_parity(&package, "purchase_order");
}

#[test]
fn weld_hashes_exact_ir_and_sql_but_contract_ignores_unused_tables() {
    let base_ir = catalog(false);
    let additive_ir = catalog(true);
    let base = run(&base_ir, &manifest(), &QUERY_SOURCES).unwrap();
    let additive = run(&additive_ir, &manifest(), &QUERY_SOURCES).unwrap();
    let base_weld = artifact_json(&base, "generated/package-weld.json");
    let additive_weld = artifact_json(&additive, "generated/package-weld.json");

    let expected_schema = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(base_ir.canonical_json_bytes()))
    );
    assert_eq!(base_weld["verified_schema_state_id"], expected_schema);
    assert_ne!(
        base_weld["verified_schema_state_id"],
        additive_weld["verified_schema_state_id"]
    );
    assert_eq!(
        base_weld["required_schema_contract"],
        additive_weld["required_schema_contract"]
    );
    assert_eq!(
        base_weld["required_platform_policy_contract"],
        json!({"id": "receiving_data_access", "state": "unsatisfied"})
    );
    assert_eq!(
        base_weld["promotion_state"],
        "blocked_unsatisfied_policy_contract"
    );
    assert!(!base.weld().promotion_eligible());
}

#[test]
fn corpus_hash_uses_sorted_unambiguous_framing() {
    let first = corpus_sha256([("a", b"bc".as_slice()), ("ab", b"c".as_slice())]);
    let reversed = corpus_sha256([("ab", b"c".as_slice()), ("a", b"bc".as_slice())]);
    let ambiguous_without_lengths = corpus_sha256([("abc", b"".as_slice())]);
    assert_eq!(first, reversed);
    assert_ne!(first, ambiguous_without_lengths);
    assert_eq!(
        first,
        "sha256:c29ceb2bae87e8e60215cf078b051f981c665f88aa75ca510eaa99dbc0b3d00f"
    );
}

#[test]
fn ordered_filters_and_query_variants_remain_structural_and_finite() {
    let ir = catalog(false);
    let mut generated_manifest = manifest();
    generated_manifest["models"]["purchase_order"]["operations"]["query"]
        .as_object_mut()
        .unwrap()
        .remove("authored_sql");
    let package = run(&ir, &generated_manifest, &[]).unwrap();
    package
        .file("generated/sql/purchase_order/query_purchase_order_number_ascending.sql")
        .unwrap();
    package
        .file("generated/sql/purchase_order/query_created_at_descending.sql")
        .unwrap();
    let input = artifact_json(
        &package,
        "generated/contracts/purchase_order/query.input.json",
    );
    assert_eq!(
        input["filters"],
        json!([
            {"field": "supplier_id", "binding": "json_array"},
            {"field": "status", "binding": "json_array"}
        ])
    );

    let mut default_only = generated_manifest;
    default_only["models"]["purchase_order"]["operations"]["query"]
        .as_object_mut()
        .unwrap()
        .remove("sort");
    let package = run(&ir, &default_only, &[]).unwrap();
    let query_paths = package
        .files()
        .iter()
        .filter(|file| {
            file.path()
                .starts_with("generated/sql/purchase_order/query_")
        })
        .collect::<Vec<_>>();
    assert_eq!(query_paths.len(), 1);
    assert_eq!(
        query_paths[0].path(),
        "generated/sql/purchase_order/query_created_at_ascending.sql"
    );
}

#[test]
fn duplicate_filters_and_schema_qualified_authored_sql_refuse() {
    let ir = catalog(false);
    let mut duplicate = manifest();
    duplicate["models"]["purchase_order"]["operations"]["query"]["filters"] = json!([
        {"field": "status", "binding": "json_array"},
        {"field": "status", "binding": "json_array"}
    ]);
    assert_eq!(
        run(&ir, &duplicate, &QUERY_SOURCES).unwrap_err().kind(),
        GenerateErrorKind::InvalidOperation
    );

    let qualified = QUERY_SOURCES.map(|source| {
        if source.path() == "query/open_purchase_order.sql" {
            AuthoredSql::new(source.path(), b"SELECT * FROM receiving.purchase_order;\n")
        } else {
            source
        }
    });
    let error = run(&ir, &manifest(), &qualified).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::SchemaQualifiedSql);
    assert_eq!(error.path(), Some("query/open_purchase_order.sql"));

    let quoted = QUERY_SOURCES.map(|source| {
        if source.path() == "query/open_purchase_order.sql" {
            AuthoredSql::new(
                source.path(),
                b"SELECT * FROM \"receiving\".\"purchase_order\";\n",
            )
        } else {
            source
        }
    });
    assert_eq!(
        run(&ir, &manifest(), &quoted).unwrap_err().kind(),
        GenerateErrorKind::SchemaQualifiedSql
    );

    let inert = QUERY_SOURCES.map(|source| {
        if source.path() == "query/open_purchase_order.sql" {
            AuthoredSql::new(
                source.path(),
                b"-- receiving.purchase_order\nSELECT 'receiving.purchase_order', $$\"receiving\".\"purchase_order\"$$ /* receiving.purchase_order */;\n",
            )
        } else {
            source
        }
    });
    run(&ir, &manifest(), &inert).unwrap();
}

#[test]
fn shipped_receiving_manifest_and_authored_corpus_generate_without_drift() {
    let ir = receiving_catalog();
    let package = generate(&GenerationInput::new(
        &ir,
        RECEIVING_MANIFEST,
        &RECEIVING_SOURCES,
        GenerationProvenance::new(
            "0123456789abcdef",
            "wamn-schema-generator/0.1.0",
            "rust-1.89",
        ),
    ))
    .unwrap();

    let purchase_query = artifact_json(
        &package,
        "generated/contracts/purchase_order/query.operation.json",
    );
    assert_eq!(purchase_query["sql_files"].as_array().unwrap().len(), 6);
    let receipt_query = artifact_json(&package, "generated/contracts/receipt/query.operation.json");
    assert_eq!(
        receipt_query["sql_files"],
        json!(["generated/sql/receipt/query_created_at_ascending.sql"])
    );
    package
        .file("generated/sql/receipt/query_created_at_ascending.sql")
        .unwrap();

    let receipt_source_map = artifact_json(&package, "generated/source-map/receipt.json");
    assert_eq!(
        receipt_source_map["wamn_api"],
        json!({
            "sql_constant_visibility": "crate",
            "mutation_constraints": [],
            "operation_rows": [],
            "accessors": [
                {
                    "name": "get",
                    "visibility": "crate",
                    "operation": "get",
                    "sql_constant": "GET_SQL",
                    "row": "ReceiptRow",
                    "fetch": "optional",
                    "binds": [
                        accessor_bind(
                            "id",
                            "uuid",
                            false,
                            "uuid::Uuid",
                            "wamn_postgres_sqlx::Uuid"
                        )
                    ]
                },
                {
                    "name": "query_created_at_ascending",
                    "visibility": "crate",
                    "operation": "query",
                    "sql_constant": "QUERY_SQL",
                    "row": "ReceiptRow",
                    "fetch": "all",
                    "binds": [
                        accessor_bind(
                            "cursor_key",
                            "timestamptz",
                            true,
                            "Option<chrono::DateTime<chrono::Utc>>",
                            "Option<wamn_postgres_sqlx::TimestampTz>"
                        ),
                        accessor_bind(
                            "cursor_id",
                            "uuid",
                            true,
                            "Option<uuid::Uuid>",
                            "Option<wamn_postgres_sqlx::Uuid>"
                        ),
                        accessor_bind("limit", "int8", false, "i64", "i64")
                    ]
                }
            ]
        })
    );
    assert_native_fixtures_match_parity(&package, "receipt");
}

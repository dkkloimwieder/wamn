use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use wamn_execution_contract::canonical_json_bytes;
use wamn_schema_generator::StatementTransactionality;
use wamn_schema_generator::{
    AuthoredSql, CrudAction, DATA_ACCESS_OVERLAY_PATH, DataAccessOverlay, GenerateErrorKind,
    GeneratedPackage, GenerationInput, GenerationProvenance, OperationVisibility, PackageManifest,
    PackageWeld, canonical_operation_identity, canonical_operation_prefix, corpus_sha256, generate,
    validate_operation_vocabulary, validate_parity_json,
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
const RECEIVING_SOURCES: [AuthoredSql<'static>; 17] = [
    AuthoredSql::new(
        "command/record_receipt/claim_command.sql",
        include_bytes!("../../../../packages/receiving/command/record_receipt/claim_command.sql"),
    ),
    AuthoredSql::new(
        "command/record_receipt/finalize_command.sql",
        include_bytes!(
            "../../../../packages/receiving/command/record_receipt/finalize_command.sql"
        ),
    ),
    AuthoredSql::new(
        "command/record_receipt/find_replay.sql",
        include_bytes!("../../../../packages/receiving/command/record_receipt/find_replay.sql"),
    ),
    AuthoredSql::new(
        "command/record_receipt/finish_purchase_order.sql",
        include_bytes!(
            "../../../../packages/receiving/command/record_receipt/finish_purchase_order.sql"
        ),
    ),
    AuthoredSql::new(
        "command/record_receipt/insert_receipt.sql",
        include_bytes!("../../../../packages/receiving/command/record_receipt/insert_receipt.sql"),
    ),
    AuthoredSql::new(
        "command/record_receipt/insert_receipt_line.sql",
        include_bytes!(
            "../../../../packages/receiving/command/record_receipt/insert_receipt_line.sql"
        ),
    ),
    AuthoredSql::new(
        "command/record_receipt/lock_purchase_order.sql",
        include_bytes!(
            "../../../../packages/receiving/command/record_receipt/lock_purchase_order.sql"
        ),
    ),
    AuthoredSql::new(
        "command/record_receipt/update_purchase_order_line.sql",
        include_bytes!(
            "../../../../packages/receiving/command/record_receipt/update_purchase_order_line.sql"
        ),
    ),
    AuthoredSql::new(
        "command/record_receipt/validate_receipt_line.sql",
        include_bytes!(
            "../../../../packages/receiving/command/record_receipt/validate_receipt_line.sql"
        ),
    ),
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
    AuthoredSql::new(
        "query/load_receipt_screen.sql",
        include_bytes!("../../../../packages/receiving/query/load_receipt_screen.sql"),
    ),
    AuthoredSql::new(
        "query/location.sql",
        include_bytes!("../../../../packages/receiving/query/location.sql"),
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
    let item = Table::new(
        "receiving",
        "item",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("item_number", ColumnType::Text, false, None, None),
        ],
        vec![
            Constraint::primary_key("item_id_pkey", ["id"]).unwrap(),
            Constraint::unique("item_item_number_key", ["item_number"]).unwrap(),
        ],
        Vec::new(),
    );
    let location = Table::new(
        "receiving",
        "location",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("location_code", ColumnType::Text, false, None, None),
        ],
        vec![Constraint::primary_key("location_id_pkey", ["id"]).unwrap()],
        Vec::new(),
    );
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
    let purchase_order_line = Table::new(
        "receiving",
        "purchase_order_line",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("purchase_order_id", ColumnType::Uuid, false, None, None),
            Column::new("line_number", ColumnType::Int32, false, None, None),
            Column::new("item_id", ColumnType::Uuid, false, None, None),
            Column::new("ordered_quantity", ColumnType::Numeric, false, None, None),
            Column::new(
                "received_quantity",
                ColumnType::Numeric,
                false,
                Some(ColumnDefault::NumericZero),
                None,
            ),
        ],
        vec![Constraint::primary_key("purchase_order_line_id_pkey", ["id"]).unwrap()],
        Vec::new(),
    );
    let record_receipt_command = Table::new(
        "receiving",
        "record_receipt_command",
        vec![
            Column::new("idempotency_key", ColumnType::Text, false, None, None),
            Column::new("canonical_command", ColumnType::Bytes, false, None, None),
            Column::new(
                "receipt_id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("purchase_order_id", ColumnType::Uuid, false, None, None),
            Column::new("purchase_order_status", ColumnType::Text, true, None, None),
            Column::new("row_version", ColumnType::Int64, true, None, None),
        ],
        vec![
            Constraint::primary_key(
                "record_receipt_command_idempotency_key_pkey",
                ["idempotency_key"],
            )
            .unwrap(),
            Constraint::unique("record_receipt_command_receipt_id_key", ["receipt_id"]).unwrap(),
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
    let receipt_line = Table::new(
        "receiving",
        "receipt_line",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("receipt_id", ColumnType::Uuid, false, None, None),
            Column::new(
                "purchase_order_line_id",
                ColumnType::Uuid,
                false,
                None,
                None,
            ),
            Column::new("quantity", ColumnType::Numeric, false, None, None),
            Column::new("location_id", ColumnType::Uuid, false, None, None),
        ],
        vec![Constraint::primary_key("receipt_line_id_pkey", ["id"]).unwrap()],
        Vec::new(),
    );
    CatalogIr::new(vec![
        item,
        location,
        purchase_order,
        purchase_order_line,
        record_receipt_command,
        receipt,
        receipt_line,
    ])
}

fn shipped_manifest() -> Value {
    serde_json::from_slice(RECEIVING_MANIFEST).unwrap()
}

fn projection_operation() -> Value {
    json!({
        "kind": "projection",
        "visibility": "public",
        "permission": "quality.load_purchase_order_detail",
        "connection": "postgres",
        "input": {
            "fields": [
                {"path": "request_id", "type": "text", "nullable": false},
                {"path": "purchase_order_id", "type": "uuid", "nullable": false}
            ]
        },
        "result": {
            "class": "one",
            "fields": [{"path": "id", "type": "uuid", "nullable": false}]
        },
        "errors": [
            "invalid_input", "not_found", "retry", "timeout", "permission_denied",
            "internal_error"
        ],
        "error_details": {
            "invalid_input": {"required": ["field"]},
            "not_found": {"required": ["field", "id"]},
            "retry": {},
            "timeout": {},
            "permission_denied": {"required": ["operation"]},
            "internal_error": {}
        },
        "relations": [{
            "schema": "receiving",
            "table": "purchase_order",
            "select_fields": ["id"],
            "insert_fields": [],
            "update_fields": [],
            "lock": false,
            "constraints": []
        }],
        "statements": {
            "load_purchase_order_detail": {
                "path": "query/quality_purchase_order_detail.sql",
                "fetch": "optional_one",
                "parameters": [
                    {"name": "purchase_order_id", "type": "uuid", "nullable": false}
                ],
                "row": [{"name": "id", "type": "uuid", "nullable": false}]
            }
        }
    })
}

fn event_handler_operation() -> Value {
    json!({
        "kind": "event_handler",
        "visibility": "private",
        "connection": "postgres",
        "input": {
            "fields": [
                {"path": "event", "type": "text", "nullable": false, "values": ["insert"]},
                {"path": "new.id", "type": "uuid", "nullable": false}
            ]
        },
        "errors": ["invalid_input", "retry", "timeout", "internal_error"],
        "error_details": {
            "invalid_input": {"required": ["field"]},
            "retry": {},
            "timeout": {},
            "internal_error": {}
        },
        "relations": [{
            "schema": "receiving",
            "table": "location",
            "select_fields": ["id"],
            "insert_fields": [],
            "update_fields": [],
            "lock": false,
            "constraints": []
        }],
        "statements": {
            "load_location": {
                "path": "command/create_inspection/load_location.sql",
                "fetch": "optional_one",
                "parameters": [{"name": "id", "type": "uuid", "nullable": false}],
                "row": [{"name": "id", "type": "uuid", "nullable": false}]
            }
        },
        "registration": {
            "source_package": "wamn_receiving",
            "entity": "receipt",
            "ops": ["insert"]
        }
    })
}

fn generic_operation_sources() -> Vec<AuthoredSql<'static>> {
    let mut sources = RECEIVING_SOURCES
        .iter()
        .copied()
        .filter(|source| source.path().starts_with("query/open_purchase_order"))
        .collect::<Vec<_>>();
    sources.push(AuthoredSql::new(
        "query/quality_purchase_order_detail.sql",
        b"SELECT id FROM purchase_order WHERE id = $1;\n",
    ));
    sources.push(AuthoredSql::new(
        "command/create_inspection/load_location.sql",
        b"SELECT id FROM location WHERE id = $1;\n",
    ));
    sources
}

fn generic_custom_operation_manifest() -> Value {
    let mut manifest = shipped_manifest();
    manifest["custom_operations"] = json!({
        "quality.load_purchase_order_detail": projection_operation(),
        "quality.create_inspection": event_handler_operation(),
    });
    manifest
}

fn shipped_generation(
    catalog: &CatalogIr,
    manifest: &Value,
) -> Result<GeneratedPackage, wamn_schema_generator::GenerateError> {
    run(catalog, manifest, &RECEIVING_SOURCES)
}

fn table<'a>(catalog: &'a CatalogIr, name: &str) -> &'a Table {
    catalog
        .tables()
        .iter()
        .find(|table| table.schema() == "receiving" && table.name() == name)
        .unwrap()
}

fn rebuilt_table(table: &Table, columns: Vec<Column>, constraints: Vec<Constraint>) -> Table {
    Table::new(
        table.schema(),
        table.name(),
        columns,
        constraints,
        table.indexes().to_vec(),
    )
}

fn replacing_table(catalog: &CatalogIr, replacement: Table) -> CatalogIr {
    let mut tables = catalog
        .tables()
        .iter()
        .filter(|table| {
            table.schema() != replacement.schema() || table.name() != replacement.name()
        })
        .cloned()
        .collect::<Vec<_>>();
    tables.push(replacement);
    CatalogIr::new(tables)
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
                    "get": {
                        "permission": "purchase_order.get",
                        "error_details": {
                            "invalid_input": {"required": ["field"]},
                            "not_found": {"required": ["field", "id"]},
                            "retry": {},
                            "timeout": {},
                            "permission_denied": {"required": ["operation"]},
                            "internal_error": {}
                        },
                        "result": "one"
                    },
                    "query": {
                        "permission": "purchase_order.query",
                        "error_details": {
                            "invalid_input": {
                                "required": ["field"],
                                "optional": ["minimum", "maximum", "observed"]
                            },
                            "retry": {},
                            "timeout": {},
                            "permission_denied": {"required": ["operation"]},
                            "internal_error": {}
                        },
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
                        "error_details": {
                            "invalid_input": {"required": ["field"]},
                            "not_found": {"required": ["field", "id"]},
                            "concurrency_conflict": {
                                "required": ["expected_row_version", "observed_row_version"]
                            },
                            "retry": {},
                            "timeout": {},
                            "permission_denied": {"required": ["operation"]},
                            "internal_error": {}
                        },
                        "writable_fields": ["supplier_id"],
                        "revision_field": "row_version",
                        "result": "one"
                    }
                }
            }
        },
        "connections": {"postgres": {"interface": "wamn:postgres@0.1.0"}},
        "components": {
            "receiving": {
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
        GenerationProvenance::new("wamn-schema-generator/0.1.0", "rust-1.89"),
        &StatementTransactionality::default(),
    ))
}

fn artifact_json(package: &GeneratedPackage, path: &str) -> Value {
    serde_json::from_slice(package.file(path).unwrap().bytes()).unwrap()
}

fn statement_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
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

fn object_named<'a>(values: &'a [Value], field: &str, name: &str) -> &'a Value {
    values.iter().find(|value| value[field] == name).unwrap()
}

#[test]
fn strict_manifest_and_ir_references_fail_loudly() {
    let ir = catalog(false);
    let mut predecessor = manifest();
    predecessor["package"]["predecessor_version"] = json!("0.9.0");
    run(&ir, &predecessor, &QUERY_SOURCES)
        .expect("the optional predecessor version is in the closed manifest vocabulary");

    predecessor["package"]["predecessor_version"] = json!("1.0.0");
    assert_eq!(
        run(&ir, &predecessor, &QUERY_SOURCES)
            .expect_err("a package version cannot name itself as predecessor")
            .kind(),
        GenerateErrorKind::InvalidManifest
    );

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

fn overlay_manifest() -> Value {
    let mut overlay = manifest();
    let mut composition =
        shipped_manifest()["custom_operations"]["receiving.record_receipt"].clone();
    let composition_object = composition.as_object_mut().unwrap();
    for field in [
        "connection",
        "relations",
        "statements",
        "transaction",
        "automatic_retry",
        "canonicalization",
        "constraint_errors",
    ] {
        composition_object.remove(field);
    }

    overlay["package"] = json!({"id": "client_acme_receiving", "version": "3.0.0"});
    overlay["base_dependencies"] = json!({
        "base_receiving": {
            "package": "wamn_receiving",
            "version": "1.0.0",
            "digest": format!("sha256:{}", "a".repeat(64)),
            "operations": ["receiving.record_receipt"]
        }
    });
    overlay["models"]["purchase_order"]["owner"] = json!("wamn_receiving");
    overlay["models"]["purchase_order"]["field_owners"] = json!({
        "supplier_id": "client_acme_receiving"
    });
    overlay["models"]["purchase_order"]["constraint_owners"] = json!({
        "purchase_order_status_check": "wamn_receiving"
    });
    overlay["custom_operations"] = json!({
        "receiving.record_receipt": composition,
        "quality.load_purchase_order_detail": projection_operation(),
        "quality.create_inspection": event_handler_operation(),
    });
    overlay["components"] = json!({
        "client_acme_receiving": {
            "connections": ["postgres"]
        }
    });
    overlay
}

#[test]
fn overlay_vocabulary_is_exact_at_dependency_definition_and_operation_grain() {
    let mut base = manifest();
    base["models"]["purchase_order"]["client_field_extensible"] = json!(true);
    let base = parsed_manifest(&base);
    assert!(base.models["purchase_order"].client_field_extensible);

    let overlay = overlay_manifest();
    let parsed = parsed_manifest(&overlay);
    let dependency = &parsed.base_dependencies["base_receiving"];
    assert_eq!(dependency.package, "wamn_receiving");
    assert_eq!(dependency.version, "1.0.0");
    assert_eq!(
        dependency.operations,
        vec!["receiving.record_receipt".to_owned()]
    );
    let model = &parsed.models["purchase_order"];
    assert_eq!(model.owner, "wamn_receiving");
    assert_eq!(model.field_owner("supplier_id"), "client_acme_receiving");
    assert_eq!(model.field_owner("status"), "wamn_receiving");
    assert_eq!(
        model.constraint_owner("purchase_order_status_check"),
        "wamn_receiving"
    );
    let projection = &parsed.custom_operations["quality.load_purchase_order_detail"];
    assert_eq!(projection.visibility(), OperationVisibility::Public);
    assert_eq!(projection.component(), None);
    assert_eq!(
        projection.permission(),
        Some("quality.load_purchase_order_detail")
    );
    let handler = &parsed.custom_operations["quality.create_inspection"];
    assert_eq!(handler.visibility(), OperationVisibility::Private);
    assert_eq!(handler.permission(), None);
    let registration = handler.registration().expect("event registration");
    assert_eq!(registration.source_package, "wamn_receiving");
    assert_eq!(registration.entity, "receipt");
    assert_eq!(registration.ops, [wamn_event_wire::Op::Insert]);

    let generated = run(&receiving_catalog(), &overlay, &generic_operation_sources())
        .expect("generic overlay custom operations must generate from one strict declaration");
    for suffix in ["operation", "input", "result", "errors"] {
        generated
            .file(&format!(
                "generated/contracts/receiving/record_receipt.{suffix}.json"
            ))
            .unwrap();
    }
    for path in [
        "generated/native-verifier/receiving_record_receipt.rs",
        "generated/wamn/receiving_record_receipt.rs",
        "generated/parity/receiving_record_receipt.json",
    ] {
        assert!(
            generated.file(path).is_none(),
            "composition-only command emitted a local SQL artifact: {path}"
        );
    }
    let composition = artifact_json(
        &generated,
        "generated/source-map/receiving_record_receipt.json",
    );
    assert_eq!(composition["composition"]["alias"], "base_receiving");
    assert_eq!(composition["composition"]["package"], "wamn_receiving");
    assert_eq!(composition["composition"]["version"], "1.0.0");
    assert_eq!(
        composition["composition"]["digest"],
        format!("sha256:{}", "a".repeat(64))
    );
    assert!(
        generated
            .file("generated/contracts/quality/create_inspection.result.json")
            .is_none()
    );

    let mut unbacked_composition = overlay;
    unbacked_composition["base_dependencies"] = json!({});
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&unbacked_composition))
            .expect_err("an unbacked SQL-less command was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut ambiguous_composition = overlay_manifest();
    ambiguous_composition["base_dependencies"]["second_base"] = json!({
        "package": "other_receiving",
        "version": "1.0.0",
        "digest": format!("sha256:{}", "b".repeat(64)),
        "operations": ["receiving.record_receipt"]
    });
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&ambiguous_composition))
            .expect_err("an ambiguous composition dependency was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn dependency_composition_may_add_one_declared_post_call_projection() {
    let mut overlay = overlay_manifest();
    let projection = projection_operation();
    let operation = overlay["custom_operations"]["receiving.record_receipt"]
        .as_object_mut()
        .unwrap();
    operation.insert("connection".to_owned(), json!("postgres"));
    operation.insert("transaction".to_owned(), json!("explicit_per_input"));
    operation.insert("automatic_retry".to_owned(), json!(false));
    operation.insert("relations".to_owned(), projection["relations"].clone());
    operation.insert("statements".to_owned(), projection["statements"].clone());

    let generated = run(&receiving_catalog(), &overlay, &generic_operation_sources())
        .expect("an exact dependency may be followed by declared local projection SQL");
    let contract = artifact_json(
        &generated,
        "generated/contracts/receiving/record_receipt.operation.json",
    );
    let statements = contract["statements"].as_array().unwrap();
    assert_eq!(statements.len(), 1);
    assert_eq!(
        statements[0]["path"],
        "query/quality_purchase_order_detail.sql"
    );
    assert_eq!(statements[0]["name"], "load_purchase_order_detail");
    let source = generic_operation_sources()
        .into_iter()
        .find(|source| source.path() == "query/quality_purchase_order_detail.sql")
        .unwrap();
    assert_eq!(statements[0]["digest"], statement_digest(source.bytes()));
    assert_eq!(contract["dependency"]["alias"], "base_receiving");
    let source_map = artifact_json(
        &generated,
        "generated/source-map/receiving_record_receipt.json",
    );
    assert_eq!(source_map["composition"]["package"], "wamn_receiving");
    assert_eq!(source_map["command"], "receiving.record_receipt");
    generated
        .file("generated/wamn/receiving_record_receipt.rs")
        .expect("declared post-call SQL generates its Wamn accessor");
}

#[test]
fn overlay_vocabulary_refuses_ranges_opaque_digests_and_unknown_definitions() {
    let mut range = overlay_manifest();
    range["base_dependencies"]["base_receiving"]["version"] = json!("^1.0");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&range))
            .expect_err("a base version range was accepted")
            .kind(),
        GenerateErrorKind::InvalidIdentity
    );

    let mut digest = overlay_manifest();
    digest["base_dependencies"]["base_receiving"]["digest"] = json!("latest");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&digest))
            .expect_err("an opaque base artifact identity was accepted")
            .kind(),
        GenerateErrorKind::InvalidIdentity
    );

    let mut field = overlay_manifest();
    field["models"]["purchase_order"]["field_owners"] = json!({
        "unknown_field": "client_acme_receiving"
    });
    assert_eq!(
        run(&catalog(false), &field, &QUERY_SOURCES)
            .expect_err("ownership of an unknown field was accepted")
            .kind(),
        GenerateErrorKind::UnknownColumn
    );

    let mut constraint = overlay_manifest();
    constraint["models"]["purchase_order"]["constraint_owners"] = json!({
        "unknown_constraint": "client_acme_receiving"
    });
    assert_eq!(
        run(&catalog(false), &constraint, &QUERY_SOURCES)
            .expect_err("ownership of an unknown constraint was accepted")
            .kind(),
        GenerateErrorKind::InvalidModel
    );

    let mut owner = overlay_manifest();
    owner["models"]["purchase_order"]["field_owners"] = json!({
        "supplier_id": "undeclared_package"
    });
    assert_eq!(
        run(&catalog(false), &owner, &QUERY_SOURCES)
            .expect_err("an undeclared definition owner was accepted")
            .kind(),
        GenerateErrorKind::InvalidModel
    );

    let mut restated_extensibility = overlay_manifest();
    restated_extensibility["models"]["purchase_order"]["client_field_extensible"] = json!(true);
    assert_eq!(
        run(&catalog(false), &restated_extensibility, &QUERY_SOURCES)
            .expect_err("an overlay restated base extensibility")
            .kind(),
        GenerateErrorKind::InvalidModel
    );
}

#[test]
fn custom_operation_kinds_visibility_permissions_and_registration_are_closed() {
    let mut empty_component = overlay_manifest();
    empty_component["custom_operations"]["quality.load_purchase_order_detail"]["component"] =
        json!("");
    let error = validate_operation_vocabulary(&parsed_manifest(&empty_component))
        .expect_err("an empty custom-operation component was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "operation quality.load_purchase_order_detail component must not be empty"
    );

    let mut private_permission = overlay_manifest();
    private_permission["custom_operations"]["quality.create_inspection"]["permission"] =
        json!("quality.create_inspection");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&private_permission))
            .expect_err("a private operation permission was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut public_permission = overlay_manifest();
    public_permission["custom_operations"]["quality.load_purchase_order_detail"]["permission"] =
        json!("quality.other_projection");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&public_permission))
            .expect_err("an inexact public operation permission was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut write_projection = overlay_manifest();
    write_projection["custom_operations"]["quality.load_purchase_order_detail"]["relations"][0]["update_fields"] =
        json!(["id"]);
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&write_projection))
            .expect_err("a write-capable projection was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut missing_registration = overlay_manifest();
    missing_registration["custom_operations"]["quality.create_inspection"]
        .as_object_mut()
        .expect("event handler object")
        .remove("registration");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&missing_registration))
            .expect_err("an event handler without a registration was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut projection_registration = overlay_manifest();
    projection_registration["custom_operations"]["quality.load_purchase_order_detail"]["registration"] =
        json!({"source_package": "wamn_receiving", "entity": "receipt", "ops": ["insert"]});
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&projection_registration))
            .expect_err("a projection registration was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    for ops in [json!([]), json!(["insert", "insert"])] {
        let mut invalid_ops = overlay_manifest();
        invalid_ops["custom_operations"]["quality.create_inspection"]["registration"]["ops"] = ops;
        assert_eq!(
            validate_operation_vocabulary(&parsed_manifest(&invalid_ops))
                .expect_err("an empty or repeated registration op set was accepted")
                .kind(),
            GenerateErrorKind::InvalidOperation
        );
    }

    let mut handler_permission_error = overlay_manifest();
    handler_permission_error["custom_operations"]["quality.create_inspection"]["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!("permission_denied"));
    handler_permission_error["custom_operations"]["quality.create_inspection"]["error_details"]["permission_denied"] =
        json!({"required": ["operation"]});
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&handler_permission_error))
            .expect_err("a private handler permission error was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut unknown_kind = overlay_manifest();
    unknown_kind["custom_operations"]["quality.load_purchase_order_detail"]["kind"] =
        json!("workflow");
    assert!(
        PackageManifest::from_slice(
            &serde_json::to_vec(&unknown_kind).expect("serialize manifest")
        )
        .is_err()
    );

    let mut old_grammar = overlay_manifest();
    old_grammar["commands"] = json!({});
    assert!(
        PackageManifest::from_slice(&serde_json::to_vec(&old_grammar).expect("serialize manifest"))
            .is_err()
    );

    let mut model_collision = manifest();
    let mut operation = projection_operation();
    operation["permission"] = json!("purchase.order");
    model_collision["custom_operations"]["purchase.order"] = operation;
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&model_collision))
            .expect_err("a custom artifact collided with a model artifact")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut custom_collision = manifest();
    for operation_name in ["a_b.c", "a.b_c"] {
        let mut operation = projection_operation();
        operation["permission"] = json!(operation_name);
        custom_collision["custom_operations"][operation_name] = operation;
    }
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&custom_collision))
            .expect_err("two custom operations shared one flattened artifact identity")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn custom_sql_generated_rust_symbols_are_unique_before_emission() {
    let mut row_collision = manifest();
    let mut operation = projection_operation();
    let statement = operation["statements"]["load_purchase_order_detail"].clone();
    operation["statements"] = json!({
        "foo1": statement,
        "foo_1": {
            "path": "query/other_purchase_order_detail.sql",
            "fetch": "optional_one",
            "parameters": [{"name": "id", "type": "uuid", "nullable": false}],
            "row": [{"name": "id", "type": "uuid", "nullable": false}]
        }
    });
    row_collision["custom_operations"]["quality.load_purchase_order_detail"] = operation;
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&row_collision))
            .expect_err("two statements collapsed to one Rust row symbol")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut fixture_collision = manifest();
    let mut operation = projection_operation();
    operation["statements"] = json!({
        "foo": {
            "path": "query/foo.sql",
            "fetch": "optional_one",
            "parameters": [{"name": "bar_baz", "type": "uuid", "nullable": false}],
            "row": [{"name": "id", "type": "uuid", "nullable": false}]
        },
        "foo_bar": {
            "path": "query/foo_bar.sql",
            "fetch": "optional_one",
            "parameters": [{"name": "baz", "type": "uuid", "nullable": false}],
            "row": [{"name": "id", "type": "uuid", "nullable": false}]
        }
    });
    fixture_collision["custom_operations"]["quality.load_purchase_order_detail"] = operation;
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&fixture_collision))
            .expect_err("two statement parameters collapsed to one Rust fixture symbol")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

fn parsed_manifest(value: &Value) -> PackageManifest {
    PackageManifest::from_slice(&serde_json::to_vec(value).expect("serialize manifest"))
        .expect("parse strict manifest")
}

#[test]
fn operation_error_details_are_required_closed_and_exact() {
    let mut missing_declaration = manifest();
    missing_declaration["models"]["purchase_order"]["operations"]["get"]
        .as_object_mut()
        .unwrap()
        .remove("error_details");
    assert!(
        PackageManifest::from_slice(&serde_json::to_vec(&missing_declaration).unwrap()).is_err(),
        "an operation without its error-detail declaration was accepted"
    );

    let mut unknown_code = manifest();
    unknown_code["models"]["purchase_order"]["operations"]["get"]["error_details"]["database_error"] =
        json!({});
    assert!(
        PackageManifest::from_slice(&serde_json::to_vec(&unknown_code).unwrap()).is_err(),
        "an undeclared error code was accepted"
    );

    let mut unknown_schema_key = manifest();
    unknown_schema_key["models"]["purchase_order"]["operations"]["get"]["error_details"]["invalid_input"]
        ["sqlstate"] = json!(true);
    assert!(
        PackageManifest::from_slice(&serde_json::to_vec(&unknown_schema_key).unwrap()).is_err(),
        "an open-ended detail declaration was accepted"
    );

    let mut missing_code = manifest();
    missing_code["models"]["purchase_order"]["operations"]["get"]["error_details"]
        .as_object_mut()
        .unwrap()
        .remove("not_found");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&missing_code))
            .expect_err("an incomplete error-code set was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut wrong_detail = manifest();
    wrong_detail["models"]["purchase_order"]["operations"]["update"]["error_details"]["concurrency_conflict"]
        ["required"] = json!(["expected_row_version", "id"]);
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&wrong_detail))
            .expect_err("incorrect concurrency detail keys were accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut repeated_detail = manifest();
    repeated_detail["models"]["purchase_order"]["operations"]["query"]["error_details"]["invalid_input"]
        ["optional"] = json!(["minimum", "maximum", "observed", "observed"]);
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&repeated_detail))
            .expect_err("a repeated detail key was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut command_detail = shipped_manifest();
    command_detail["custom_operations"]["receiving.record_receipt"]["error_details"]["purchase_order_not_open"]
        ["required"] = json!(["id"]);
    validate_operation_vocabulary(&parsed_manifest(&command_detail))
        .expect("package-local error detail keys are authored contract facts");

    command_detail["custom_operations"]["receiving.record_receipt"]["error_details"]["purchase_order_not_open"]
        ["required"] = json!(["id", "id"]);
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&command_detail))
            .expect_err("repeated package-local detail keys were accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut constraint_detail = shipped_manifest();
    constraint_detail["custom_operations"]["receiving.record_receipt"]["error_details"]["receipt_reference_conflict"]
        ["required"] = json!(["field"]);
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&constraint_detail))
            .expect_err("a constraint error without constraint identity was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut repeated_constraint_target = shipped_manifest();
    repeated_constraint_target["custom_operations"]["receiving.record_receipt"]["constraint_errors"]
        ["receipt_idempotency_key_key"] = json!("receipt_reference_conflict");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&repeated_constraint_target))
            .expect_err("multiple constraints silently collapsed to one error case")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut reserved_constraint_target = shipped_manifest();
    reserved_constraint_target["custom_operations"]["receiving.record_receipt"]["constraint_errors"]
        ["receipt_purchase_order_id_receipt_reference_key"] = json!("retry");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&reserved_constraint_target))
            .expect_err("a constraint redefined a reserved error meaning")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut public_without_permission_refusal = shipped_manifest();
    public_without_permission_refusal["custom_operations"]["receiving.record_receipt"]["errors"]
        .as_array_mut()
        .unwrap()
        .retain(|error| error.as_str() != Some("permission_denied"));
    public_without_permission_refusal["custom_operations"]["receiving.record_receipt"]
        ["error_details"]
        .as_object_mut()
        .unwrap()
        .remove("permission_denied");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&public_without_permission_refusal))
            .expect_err("a public operation omitted its permission refusal")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut transactionless_sql = shipped_manifest();
    let command = transactionless_sql["custom_operations"]["receiving.record_receipt"]
        .as_object_mut()
        .unwrap();
    command.remove("transaction");
    command.remove("automatic_retry");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&transactionless_sql))
            .expect_err("a local SQL command without an explicit transaction was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut missing_canonical_quantity = shipped_manifest();
    missing_canonical_quantity["custom_operations"]["receiving.record_receipt"]["input"]["fields"]
        .as_array_mut()
        .unwrap()
        .retain(|field| field["path"].as_str() != Some("value.line[].quantity"));
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&missing_canonical_quantity))
            .expect_err("canonical line semantics omitted their positive numeric quantity")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn component_grouping_defaults_one_group_and_refuses_invalid_splits() {
    let single = parsed_manifest(&manifest());
    assert_eq!(
        validate_operation_vocabulary(&single).unwrap(),
        BTreeSet::from([
            "purchase_order.get".to_owned(),
            "purchase_order.query".to_owned(),
            "purchase_order.update".to_owned(),
        ])
    );
    assert!(
        single.models["purchase_order"].operations[&CrudAction::Get]
            .component
            .is_none()
    );

    let mut empty = manifest();
    empty["models"]["purchase_order"]["operations"]["get"]["component"] = json!("");
    let error = validate_operation_vocabulary(&parsed_manifest(&empty))
        .expect_err("an empty component name was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "operation purchase_order.get component must not be empty"
    );

    let mut unknown = manifest();
    unknown["models"]["purchase_order"]["operations"]["get"]["component"] = json!("missing");
    let error = validate_operation_vocabulary(&parsed_manifest(&unknown))
        .expect_err("an unknown component was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "operation purchase_order.get references unknown component missing"
    );

    let mut identical = manifest();
    identical["components"]["duplicate"] = json!({"connections": ["postgres"]});
    for operation in ["get", "query", "update"] {
        identical["models"]["purchase_order"]["operations"][operation]["component"] =
            json!(if operation == "query" {
                "duplicate"
            } else {
                "receiving"
            });
    }
    let error = validate_operation_vocabulary(&parsed_manifest(&identical))
        .expect_err("an identical-requirement split was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "components duplicate and receiving have identical requirement sets"
    );

    let mut distinct = manifest();
    distinct["connections"]["reporting"] = json!({"interface": "wamn:postgres@0.1.0"});
    distinct["components"]["reporting"] = json!({"connections": ["reporting"]});
    distinct["models"]["purchase_order"]["operations"]["query"]["component"] = json!("reporting");
    let error = validate_operation_vocabulary(&parsed_manifest(&distinct))
        .expect_err("a multi-component manifest omitted explicit operation grouping");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "operation purchase_order.get must name a component when the manifest declares multiple components"
    );

    for operation in ["get", "update"] {
        distinct["models"]["purchase_order"]["operations"][operation]["component"] =
            json!("receiving");
    }
    validate_operation_vocabulary(&parsed_manifest(&distinct))
        .expect("distinct requirement groups with explicit operation membership");

    let mut old_grouping = manifest();
    old_grouping["components"]["receiving"]["operations"] = json!(["purchase_order.get"]);
    assert!(PackageManifest::from_slice(&serde_json::to_vec(&old_grouping).unwrap()).is_err());
}

#[test]
fn shared_operation_vocabulary_refuses_permission_identity_drift() {
    let mut mismatch = manifest();
    mismatch["models"]["purchase_order"]["operations"]["get"]["permission"] =
        json!("purchase_order.query");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&mismatch))
            .expect_err("permission identity drift was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
    assert_eq!(
        run(&catalog(false), &mismatch, &QUERY_SOURCES)
            .expect_err("generation accepted permission identity drift")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn shared_operation_vocabulary_refuses_noncanonical_coordinates() {
    let mut package = manifest();
    package["package"]["id"] = json!("wamn-Receiving");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&package))
            .expect_err("noncanonical package id was accepted")
            .kind(),
        GenerateErrorKind::InvalidIdentity
    );

    for ambiguous in ["1.0.0:shadow", "1.0.0/shadow", "1.0.0@shadow"] {
        let mut version = manifest();
        version["package"]["version"] = json!(ambiguous);
        assert_eq!(
            validate_operation_vocabulary(&parsed_manifest(&version))
                .expect_err("ambiguous package version was accepted")
                .kind(),
            GenerateErrorKind::InvalidIdentity
        );
    }
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
    assert_eq!(
        input["expected_row_version"],
        json!({"field": "row_version", "type": "int64", "required": true})
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
    let package_manifest = parsed_manifest(&manifest());
    assert_eq!(
        canonical_operation_prefix(&package_manifest.package).unwrap(),
        "wamn-receiving:"
    );
    assert_eq!(
        canonical_operation_identity(&package_manifest.package, "receiving.record_receipt")
            .unwrap(),
        "wamn-receiving:receiving/record-receipt@1.0.0"
    );
    let package = run(&catalog(false), &manifest(), &QUERY_SOURCES).unwrap();
    let operation = artifact_json(
        &package,
        "generated/contracts/purchase_order/update.operation.json",
    );
    assert_eq!(operation["permission_token"], "purchase_order.update");
    assert_eq!(
        operation["grant"],
        "wamn-receiving:purchase-order/update@1.0.0"
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
            && case["detail"] == json!({})
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

/// A result CLASS says how many rows come back, not what is in them. Every
/// generated operation therefore ships a result CONTRACT beside its class, and
/// that contract carries the model's closed value domains — without it the
/// result survives only as statement columns, and a control rendering `status`
/// gets a free-text box where a choice belongs.
#[test]
fn generated_operations_ship_a_result_contract_with_closed_domains() {
    let package = run(&catalog(false), &manifest(), &QUERY_SOURCES).unwrap();
    for action in ["get", "query", "update"] {
        let operation = artifact_json(
            &package,
            &format!("generated/contracts/purchase_order/{action}.operation.json"),
        );
        let result = artifact_json(
            &package,
            &format!("generated/contracts/purchase_order/{action}.result.json"),
        );
        assert_eq!(
            result["class"], operation["result"],
            "{action} result contract disagrees with its declared class"
        );
        let status = result["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["path"] == "status")
            .unwrap_or_else(|| panic!("{action} projects status"));
        assert_eq!(
            status["values"],
            json!(["open", "complete", "cancelled"]),
            "{action} dropped the closed domain the model declares"
        );
    }

    assert_eq!(
        artifact_json(
            &package,
            "generated/contracts/purchase_order/get.result.json"
        ),
        json!({
            "class": "one",
            "fields": [
                {"path": "created_at", "type": "timestamptz", "nullable": false, "values": []},
                {"path": "id", "type": "uuid", "nullable": false, "values": []},
                {
                    "path": "purchase_order_number",
                    "type": "text",
                    "nullable": false,
                    "values": []
                },
                {"path": "row_version", "type": "int64", "nullable": false, "values": []},
                {
                    "path": "status",
                    "type": "text",
                    "nullable": false,
                    "values": ["open", "complete", "cancelled"]
                },
                {"path": "supplier_id", "type": "uuid", "nullable": false, "values": []}
            ]
        })
    );

    // A mutation's outcome discriminator is not a model field, so it carries
    // no domain rather than an invented one.
    let update = artifact_json(
        &package,
        "generated/contracts/purchase_order/update.result.json",
    );
    let outcome = &update["fields"][0];
    assert_eq!(outcome["path"], "outcome");
    assert_eq!(outcome["values"], json!([]));
}

#[test]
fn wamn_accessors_are_structurally_derived_from_operations_and_ir() {
    let package = run(&catalog(false), &manifest(), &QUERY_SOURCES).unwrap();
    let get_contract = artifact_json(
        &package,
        "generated/contracts/purchase_order/get.operation.json",
    );
    assert!(get_contract.get("sql_files").is_none());
    let get_statement = &get_contract["statements"][0];
    assert_eq!(get_statement["name"], "get");
    let get_path = get_statement["path"].as_str().unwrap();
    assert_eq!(
        get_statement["digest"],
        statement_digest(package.file(get_path).unwrap().bytes())
    );
    assert_eq!(
        get_statement["binds"],
        json!([{"name": "id", "type": "uuid", "nullable": false}])
    );
    assert_eq!(
        get_statement["columns"],
        json!([
            {"name": "created_at", "type": "timestamptz", "nullable": false},
            {"name": "id", "type": "uuid", "nullable": false},
            {"name": "purchase_order_number", "type": "text", "nullable": false},
            {"name": "row_version", "type": "int64", "nullable": false},
            {"name": "status", "type": "text", "nullable": false},
            {"name": "supplier_id", "type": "uuid", "nullable": false}
        ])
    );
    let source_map = artifact_json(&package, "generated/source-map/purchase_order.json");
    let api = &source_map["wamn_api"];
    assert_eq!(api["statement_digest_visibility"], "crate");
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
            "statement_digest_constant": "GET_DIGEST",
            "row": "PurchaseOrderRow",
            "fetch": "optional",
            "binds": [
                accessor_bind(
                    "id",
                    "uuid",
                    false,
                    "uuid::Uuid",
                    "wamn_postgres_statements::Uuid"
                )
            ]
        })
    );

    for (name, digest_constant, cursor_postgres, native_cursor, wamn_cursor) in [
        (
            "query_purchase_order_number_ascending",
            "QUERY_0_DIGEST",
            "text",
            "Option<String>",
            "Option<String>",
        ),
        (
            "query_purchase_order_number_descending",
            "QUERY_1_DIGEST",
            "text",
            "Option<String>",
            "Option<String>",
        ),
        (
            "query_status_ascending",
            "QUERY_2_DIGEST",
            "text",
            "Option<String>",
            "Option<String>",
        ),
        (
            "query_status_descending",
            "QUERY_3_DIGEST",
            "text",
            "Option<String>",
            "Option<String>",
        ),
        (
            "query_created_at_ascending",
            "QUERY_4_DIGEST",
            "timestamptz",
            "Option<chrono::DateTime<chrono::Utc>>",
            "Option<wamn_postgres_statements::TimestampTz>",
        ),
        (
            "query_created_at_descending",
            "QUERY_5_DIGEST",
            "timestamptz",
            "Option<chrono::DateTime<chrono::Utc>>",
            "Option<wamn_postgres_statements::TimestampTz>",
        ),
    ] {
        assert_eq!(
            wamn_accessor(&source_map, name),
            &json!({
                "name": name,
                "visibility": "crate",
                "operation": "query",
                "statement_digest_constant": digest_constant,
                "row": "PurchaseOrderRow",
                "fetch": "all",
                "binds": [
                    accessor_bind(
                        "supplier_id_filter",
                        "jsonb",
                        true,
                        "Option<serde_json::Value>",
                        "Option<wamn_postgres_statements::Json>"
                    ),
                    accessor_bind(
                        "status_filter",
                        "jsonb",
                        true,
                        "Option<serde_json::Value>",
                        "Option<wamn_postgres_statements::Json>"
                    ),
                    accessor_bind("cursor_key", cursor_postgres, true, native_cursor, wamn_cursor),
                    accessor_bind(
                        "cursor_id",
                        "uuid",
                        true,
                        "Option<uuid::Uuid>",
                        "Option<wamn_postgres_statements::Uuid>"
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
            "statement_digest_constant": "UPDATE_DIGEST",
            "row": "PurchaseOrderUpdateRow",
            "fetch": "one",
            "binds": [
                accessor_bind("id", "uuid", false, "uuid::Uuid", "wamn_postgres_statements::Uuid"),
                accessor_bind("expected_row_version", "int8", false, "i64", "i64"),
                accessor_bind("supplier_id_present", "boolean", false, "bool", "bool"),
                accessor_bind(
                    "supplier_id_value",
                    "uuid",
                    true,
                    "Option<uuid::Uuid>",
                    "Option<wamn_postgres_statements::Uuid>"
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
                {"name": "observed_row_version", "type": "Option<i64>"},
                {"name": "created_at", "type": "Option<wamn_postgres_statements::TimestampTz>"},
                {"name": "id", "type": "Option<wamn_postgres_statements::Uuid>"},
                {"name": "purchase_order_number", "type": "Option<String>"},
                {"name": "row_version", "type": "Option<i64>"},
                {"name": "status", "type": "Option<String>"},
                {"name": "supplier_id", "type": "Option<wamn_postgres_statements::Uuid>"}
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
                {"name": "observed_row_version", "type": "Option<i64>"},
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
        hex::encode(Sha256::digest(canonical_json_bytes(
            &serde_json::to_value(&base_ir).unwrap()
        )))
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
fn explicit_cdc_exclusion_is_a_required_relation_without_fabricated_fields() {
    let mut package_manifest = manifest();
    package_manifest["internal_relations"] = json!({
        "unused": {
            "schema": "receiving",
            "table": "unused",
            "cdc": "excluded"
        }
    });
    let package = run(&catalog(true), &package_manifest, &QUERY_SOURCES).unwrap();
    let weld = artifact_json(&package, "generated/package-weld.json");
    let required = weld["required_schema_contract"]["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|table| table["table"] == "unused")
        .expect("the explicit exclusion is part of the schema contract");
    assert_eq!(required["schema"], "receiving");
    assert_eq!(required["fields"], json!([]));
    assert_eq!(required["constraints"], json!([]));
}

#[test]
fn generated_weld_has_one_strict_canonical_reader() {
    let package = run(&catalog(false), &manifest(), &QUERY_SOURCES).unwrap();
    let bytes = package
        .file("generated/package-weld.json")
        .expect("the generated package carries its weld")
        .bytes();
    assert_eq!(&PackageWeld::from_slice(bytes).unwrap(), package.weld());

    let mut alternate = bytes.to_vec();
    alternate.push(b'\n');
    assert_eq!(
        PackageWeld::from_slice(&alternate).unwrap_err().kind(),
        GenerateErrorKind::InvalidManifest
    );

    let mut unknown: Value = serde_json::from_slice(bytes).unwrap();
    unknown["extra"] = json!(true);
    assert_eq!(
        PackageWeld::from_slice(&canonical_json_bytes(&unknown))
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidManifest
    );

    let mut contradictory: Value = serde_json::from_slice(bytes).unwrap();
    contradictory["promotion_state"] = json!("eligible");
    assert_eq!(
        PackageWeld::from_slice(&canonical_json_bytes(&contradictory))
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidManifest
    );
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
        GenerationProvenance::new("wamn-schema-generator/0.1.0", "rust-1.89"),
        &StatementTransactionality::default(),
    ))
    .unwrap();

    let purchase_query = artifact_json(
        &package,
        "generated/contracts/purchase_order/query.operation.json",
    );
    assert_eq!(purchase_query["statements"].as_array().unwrap().len(), 6);
    let receipt_query = artifact_json(&package, "generated/contracts/receipt/query.operation.json");
    assert_eq!(
        receipt_query["statements"][0]["path"],
        "generated/sql/receipt/query_created_at_ascending.sql"
    );
    package
        .file("generated/sql/receipt/query_created_at_ascending.sql")
        .unwrap();
    package
        .file("generated/native-verifier/receiving_record_receipt.rs")
        .unwrap();
    package
        .file("generated/wamn/receiving_record_receipt.rs")
        .unwrap();
    let command = artifact_json(
        &package,
        "generated/contracts/receiving/record_receipt.operation.json",
    );
    assert_eq!(command["transaction"], "explicit_per_input");
    assert_eq!(command["automatic_retry"], false);
    assert_eq!(command["statements"].as_array().unwrap().len(), 9);
    let errors = artifact_json(
        &package,
        "generated/contracts/receiving/record_receipt.errors.json",
    );
    assert!(errors["cases"].as_array().unwrap().iter().any(|case| {
        case["literal"] == "receipt_reference_conflict"
            && case["constraint"] == "receipt_purchase_order_id_receipt_reference_key"
    }));
    validate_parity_json(
        package
            .file("generated/parity/receiving_record_receipt.json")
            .unwrap()
            .bytes(),
    )
    .unwrap();

    let overlay_file = package.file(DATA_ACCESS_OVERLAY_PATH).unwrap();
    DataAccessOverlay::from_slice(overlay_file.bytes())
        .expect("the generator must emit exact canonical data-access evidence");
    let mut noncanonical_overlay = overlay_file.bytes().to_vec();
    noncanonical_overlay.push(b'\n');
    assert_eq!(
        DataAccessOverlay::from_slice(&noncanonical_overlay)
            .expect_err("a second byte spelling must refuse")
            .kind(),
        GenerateErrorKind::InvalidManifest
    );
    let overlay: Value = serde_json::from_slice(overlay_file.bytes()).unwrap();
    assert_eq!(overlay["role"], "wamn_app");
    assert_eq!(overlay["contract"], "receiving_data_access");
    let location = overlay["relations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|relation| relation["table"] == "location")
        .unwrap();
    // `location.list` reads the code, so the derived ACL grants SELECT on it.
    // The invariant this pins is that location is never WRITTEN: a read
    // operation may widen the select set and must not touch the rest.
    assert_eq!(location["select_fields"], json!(["id", "location_code"]));
    assert_eq!(location["insert_fields"], json!([]));
    assert_eq!(location["update_fields"], json!([]));
    assert_eq!(location["lock"], true);
    assert_eq!(location["lock_update_field"], "id");

    let receipt_source_map = artifact_json(&package, "generated/source-map/receipt.json");
    assert_eq!(
        receipt_source_map["wamn_api"],
        json!({
            "statement_digest_visibility": "crate",
            "mutation_constraints": [],
            "operation_rows": [],
            "accessors": [
                {
                    "name": "get",
                    "visibility": "crate",
                    "operation": "get",
                    "statement_digest_constant": "GET_DIGEST",
                    "row": "ReceiptRow",
                    "fetch": "optional",
                    "binds": [
                        accessor_bind(
                            "id",
                            "uuid",
                            false,
                            "uuid::Uuid",
                            "wamn_postgres_statements::Uuid"
                        )
                    ]
                },
                {
                    "name": "query_created_at_ascending",
                    "visibility": "crate",
                    "operation": "query",
                    "statement_digest_constant": "QUERY_DIGEST",
                    "row": "ReceiptRow",
                    "fetch": "all",
                    "binds": [
                        accessor_bind(
                            "cursor_key",
                            "timestamptz",
                            true,
                            "Option<chrono::DateTime<chrono::Utc>>",
                            "Option<wamn_postgres_statements::TimestampTz>"
                        ),
                        accessor_bind(
                            "cursor_id",
                            "uuid",
                            true,
                            "Option<uuid::Uuid>",
                            "Option<wamn_postgres_statements::Uuid>"
                        ),
                        accessor_bind("limit", "int8", false, "i64", "i64")
                    ]
                }
            ]
        })
    );
    assert_native_fixtures_match_parity(&package, "receipt");
}

#[test]
fn generic_custom_operation_path_preserves_shipped_receiving_bytes() {
    let package = generate(&GenerationInput::new(
        &receiving_catalog(),
        RECEIVING_MANIFEST,
        &RECEIVING_SOURCES,
        GenerationProvenance::new("wamn-schema-generator/0.1.0", "rust-1.98.0"),
        // THE SHIPPED BYTES CARRY POSTGRESQL'S VERDICTS, so the generation this
        // compares against must carry the same ones. 3c added the classification
        // and regenerated the packages but left this call unclassified, which
        // made every write statement differ and the test red on main. Frozen
        // here rather than read back out of the artifact: reading it from the
        // file under test would compare the file with itself.
        &StatementTransactionality::from_paths(BTreeMap::from([
            ("command/record_receipt/claim_command.sql".to_owned(), true),
            (
                "command/record_receipt/finalize_command.sql".to_owned(),
                true,
            ),
            ("command/record_receipt/find_replay.sql".to_owned(), false),
            (
                "command/record_receipt/finish_purchase_order.sql".to_owned(),
                true,
            ),
            ("command/record_receipt/insert_receipt.sql".to_owned(), true),
            (
                "command/record_receipt/insert_receipt_line.sql".to_owned(),
                true,
            ),
            (
                "command/record_receipt/lock_purchase_order.sql".to_owned(),
                true,
            ),
            (
                "command/record_receipt/update_purchase_order_line.sql".to_owned(),
                true,
            ),
            (
                "command/record_receipt/validate_receipt_line.sql".to_owned(),
                true,
            ),
        ])),
    ))
    .unwrap();
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../packages/receiving");
    let generated_root = package_root.join("generated");
    for relative in [
        "contracts/receiving/record_receipt.operation.json",
        "contracts/receiving/record_receipt.input.json",
        "contracts/receiving/record_receipt.result.json",
        "contracts/receiving/record_receipt.errors.json",
        "native-verifier/receiving_record_receipt.rs",
        "wamn/receiving_record_receipt.rs",
        "parity/receiving_record_receipt.json",
        "source-map/receiving_record_receipt.json",
    ] {
        let file = package.file(&format!("generated/{relative}")).unwrap();
        assert_eq!(
            file.bytes(),
            fs::read(generated_root.join(relative)).unwrap(),
            "generated byte drift in {relative}"
        );
    }
}

#[test]
fn generic_custom_operation_kinds_emit_typed_contracts_and_sql_siblings() {
    let manifest = generic_custom_operation_manifest();
    let package = run(
        &receiving_catalog(),
        &manifest,
        &generic_operation_sources(),
    )
    .unwrap();

    for (operation, module, kind) in [
        (
            "quality/load_purchase_order_detail",
            "quality_load_purchase_order_detail",
            "projection",
        ),
        (
            "quality/create_inspection",
            "quality_create_inspection",
            "event_handler",
        ),
    ] {
        let contract = artifact_json(
            &package,
            &format!("generated/contracts/{operation}.operation.json"),
        );
        assert_eq!(contract["kind"], kind);
        package
            .file(&format!("generated/contracts/{operation}.input.json"))
            .unwrap();
        package
            .file(&format!("generated/contracts/{operation}.errors.json"))
            .unwrap();
        package
            .file(&format!("generated/native-verifier/{module}.rs"))
            .unwrap();
        package
            .file(&format!("generated/wamn/{module}.rs"))
            .unwrap();
        let parity_path = format!("generated/parity/{module}.json");
        validate_parity_json(package.file(&parity_path).unwrap().bytes()).unwrap();
        let source_map = artifact_json(&package, &format!("generated/source-map/{module}.json"));
        assert_eq!(source_map["operation"], operation.replace('/', "."));
        assert_eq!(source_map["kind"], kind);
        assert_eq!(
            source_map["statements"].as_object().unwrap().len(),
            source_map["wamn_accessors"].as_array().unwrap().len()
        );
    }

    assert!(
        package
            .file("generated/contracts/quality/create_inspection.result.json")
            .is_none()
    );
    package
        .file("generated/contracts/quality/load_purchase_order_detail.result.json")
        .unwrap();
    let handler = artifact_json(
        &package,
        "generated/contracts/quality/create_inspection.operation.json",
    );
    assert_eq!(handler["registration"]["source_package"], "wamn_receiving");
    assert_eq!(handler["registration"]["ops"], json!(["insert"]));
    let data_access = artifact_json(&package, "generated/platform-policy/data-access.json");
    let location = data_access["relations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|relation| relation["table"] == "location")
        .unwrap();
    assert_eq!(location["select_fields"], json!(["id"]));
}

#[test]
fn ownership_only_model_generates_without_fabricated_crud() {
    let manifest = shipped_manifest();
    let package = shipped_generation(&receiving_catalog(), &manifest).unwrap();
    for model in ["item", "location", "purchase_order_line", "receipt_line"] {
        package
            .file(&format!("generated/models/{model}.json"))
            .unwrap();
        for operation in ["get", "query", "create", "update", "delete"] {
            assert!(
                package
                    .file(&format!(
                        "generated/contracts/{model}/{operation}.operation.json"
                    ))
                    .is_none()
            );
        }
    }
}

#[test]
fn internal_relation_cdc_exclusion_is_closed_and_not_a_model() {
    let manifest = shipped_manifest();
    let package = shipped_generation(&receiving_catalog(), &manifest).unwrap();
    assert!(
        package
            .file("generated/models/record_receipt_command.json")
            .is_none(),
        "the command ledger is mechanism state, not a fabricated model"
    );

    let mut overlap = manifest.clone();
    overlap["internal_relations"]["record_receipt_command"]["table"] = json!("receipt");
    let error = shipped_generation(&receiving_catalog(), &overlap)
        .expect_err("one physical relation received two CDC classifications");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidManifest);

    let mut duplicate_model = manifest.clone();
    let receipt_model = duplicate_model["models"]["receipt"].clone();
    duplicate_model["models"]["receipt_alias"] = receipt_model;
    let error = shipped_generation(&receiving_catalog(), &duplicate_model)
        .expect_err("one physical relation received two model identities");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidManifest);

    for (class, pointer, table) in [
        ("model", "/models/item/table", "wamn_entities"),
        (
            "internal relation",
            "/internal_relations/record_receipt_command/table",
            "wamn_cdc_exclusions",
        ),
    ] {
        let mut reserved = manifest.clone();
        *reserved.pointer_mut(pointer).unwrap() = json!(table);
        let error = shipped_generation(&receiving_catalog(), &reserved)
            .expect_err("a package claimed a control-owned relation");
        assert_eq!(error.kind(), GenerateErrorKind::InvalidManifest);
        assert!(error.to_string().contains(class));
        assert!(error.to_string().contains(table));
    }

    let mut reserved_operation = manifest.clone();
    reserved_operation["custom_operations"]["receiving.record_receipt"]["relations"][0]["table"] =
        json!("wamn_cdc_exclusions");
    let error = shipped_generation(&receiving_catalog(), &reserved_operation)
        .expect_err("a package operation referenced a control-owned relation");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    assert!(error.to_string().contains("wamn_cdc_exclusions"));

    let mut unknown = manifest.clone();
    unknown["internal_relations"]["record_receipt_command"]["table"] = json!("missing");
    let error = shipped_generation(&receiving_catalog(), &unknown)
        .expect_err("an exclusion for an absent relation was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::UnknownRelation);

    let mut open_vocabulary = manifest;
    open_vocabulary["internal_relations"]["record_receipt_command"]["cdc"] = json!("ignored");
    assert!(
        PackageManifest::from_slice(&serde_json::to_vec(&open_vocabulary).unwrap()).is_err(),
        "the CDC disposition vocabulary became open-ended"
    );
}

#[test]
fn command_privilege_declarations_match_sql_effects_and_row_locks() {
    let catalog = receiving_catalog();
    let mut wrong_verb = shipped_manifest();
    wrong_verb["custom_operations"]["receiving.record_receipt"]["relations"][0]["select_fields"] =
        json!([]);
    wrong_verb["custom_operations"]["receiving.record_receipt"]["relations"][0]["update_fields"] =
        json!(["id"]);
    let error = shipped_generation(&catalog, &wrong_verb).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    assert_eq!(error.object(), Some("receiving.location"));

    let mut undeclared_lock = shipped_manifest();
    undeclared_lock["custom_operations"]["receiving.record_receipt"]["relations"][0]["lock"] =
        json!(false);
    let error = shipped_generation(&catalog, &undeclared_lock).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    assert_eq!(error.object(), Some("receiving.location"));

    let mut unused_lock = shipped_manifest();
    let receipt = unused_lock["custom_operations"]["receiving.record_receipt"]["relations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|relation| relation["table"] == "receipt")
        .unwrap();
    receipt["lock"] = json!(true);
    let error = shipped_generation(&catalog, &unused_lock).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    assert_eq!(error.object(), Some("receiving.receipt"));
}

#[test]
fn shipped_command_source_map_parity_and_bind_fixtures_align_structurally() {
    let manifest = shipped_manifest();
    let package = shipped_generation(&receiving_catalog(), &manifest).unwrap();
    let operation = artifact_json(
        &package,
        "generated/contracts/receiving/record_receipt.operation.json",
    );
    let input = artifact_json(
        &package,
        "generated/contracts/receiving/record_receipt.input.json",
    );
    let source_map = artifact_json(
        &package,
        "generated/source-map/receiving_record_receipt.json",
    );
    let parity = artifact_json(&package, "generated/parity/receiving_record_receipt.json");
    validate_parity_json(
        package
            .file("generated/parity/receiving_record_receipt.json")
            .unwrap()
            .bytes(),
    )
    .unwrap();

    let request_id = input["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["path"] == "request_id")
        .unwrap();
    assert_eq!(request_id["type"], "text");
    assert_eq!(source_map["command"], "receiving.record_receipt");
    assert_eq!(
        source_map["manifest"],
        "wamn.json#/custom_operations/receiving.record_receipt"
    );
    assert_eq!(
        source_map["relations"],
        manifest["custom_operations"]["receiving.record_receipt"]["relations"]
    );

    let statements = source_map["statements"].as_object().unwrap();
    let accessors = source_map["wamn_accessors"].as_array().unwrap();
    let native_rows = source_map["native_rows"].as_array().unwrap();
    let wamn_rows = source_map["wamn_rows"].as_array().unwrap();
    let parity_fields = parity["fields"].as_array().unwrap();
    let parity_binds = parity["accessor_binds"].as_array().unwrap();
    assert_eq!(statements.len(), 9);
    assert_eq!(accessors.len(), statements.len());
    assert_eq!(native_rows.len(), statements.len());
    assert_eq!(wamn_rows.len(), statements.len());

    let sql_files = statements
        .values()
        .map(|statement| statement["path"].clone())
        .collect::<Vec<_>>();
    let operation_statements = operation["statements"].as_array().unwrap();
    assert_eq!(operation_statements.len(), sql_files.len());
    assert_eq!(
        operation_statements
            .iter()
            .map(|statement| statement["path"].clone())
            .collect::<Vec<_>>(),
        sql_files
    );
    assert_eq!(
        sql_files
            .iter()
            .map(Value::as_str)
            .collect::<Option<BTreeSet<_>>>()
            .unwrap()
            .len(),
        statements.len()
    );

    let mut observed_fields = 0;
    let mut observed_binds = 0;
    for (index, (statement_name, statement)) in statements.iter().enumerate() {
        let accessor = &accessors[index];
        let native_row = &native_rows[index];
        let wamn_row = &wamn_rows[index];
        assert_eq!(accessor["name"], statement_name.as_str());
        assert_eq!(accessor["fetch"], statement["fetch"]);
        assert_eq!(
            accessor["statement_digest_constant"],
            format!("{}_DIGEST", statement_name.to_ascii_uppercase())
        );
        let contract = operation_statements
            .iter()
            .find(|contract| contract["name"] == statement_name.as_str())
            .unwrap();
        assert_eq!(contract["path"], statement["path"]);
        let path = statement["path"].as_str().unwrap();
        let source = RECEIVING_SOURCES
            .iter()
            .find(|source| source.path() == path)
            .unwrap();
        assert_eq!(contract["digest"], statement_digest(source.bytes()));
        assert_eq!(contract["binds"], statement["parameters"]);
        assert_eq!(contract["columns"], statement["row"]);
        assert_eq!(native_row["name"], accessor["row"]);
        assert_eq!(wamn_row["name"], accessor["row"]);
        assert_eq!(native_row["visibility"], "crate");
        assert_eq!(wamn_row["visibility"], "crate");

        let declared_fields = statement["row"].as_array().unwrap();
        let native_fields = native_row["fields"].as_array().unwrap();
        let wamn_fields = wamn_row["fields"].as_array().unwrap();
        assert_eq!(native_fields.len(), declared_fields.len());
        assert_eq!(wamn_fields.len(), declared_fields.len());
        observed_fields += declared_fields.len();
        for declared in declared_fields {
            let name = declared["name"].as_str().unwrap();
            let identity = format!("{statement_name}.{name}");
            let parity_field = object_named(parity_fields, "field", &identity);
            let native_field = object_named(native_fields, "name", name);
            let wamn_field = object_named(wamn_fields, "name", name);
            assert_eq!(parity_field["nullable"], declared["nullable"]);
            assert_eq!(parity_field["wamn_sql_value"], declared["type"]);
            assert_eq!(native_field["type"], parity_field["native_rust"]);
            assert_eq!(wamn_field["type"], parity_field["wamn_rust"]);
        }

        let declared_parameters = statement["parameters"].as_array().unwrap();
        let accessor_binds = accessor["binds"].as_array().unwrap();
        assert_eq!(accessor_binds.len(), declared_parameters.len());
        observed_binds += declared_parameters.len();
        for declared in declared_parameters {
            let name = declared["name"].as_str().unwrap();
            let accessor_bind = object_named(accessor_binds, "parameter", name);
            let parity_bind = parity_binds
                .iter()
                .find(|bind| {
                    bind["accessor"] == statement_name.as_str() && bind["parameter"] == name
                })
                .unwrap();
            assert_eq!(accessor_bind["nullable"], declared["nullable"]);
            assert_eq!(accessor_bind["postgres"], parity_bind["postgres"]);
            assert_eq!(accessor_bind["native_rust"], parity_bind["native_rust"]);
            assert_eq!(accessor_bind["wamn_rust"], parity_bind["wamn_rust"]);
        }
    }
    assert_eq!(observed_fields, parity_fields.len());
    assert_eq!(observed_binds, parity_binds.len());
    assert_native_fixtures_match_parity(&package, "receiving_record_receipt");
}

#[test]
fn custom_statement_declarations_drive_both_siblings_without_domain_tables() {
    let catalog = receiving_catalog();
    let mut declaration = shipped_manifest();
    declaration["custom_operations"]["receiving.record_receipt"]["statements"]["claim_command"]["parameters"]
        [1] = json!({
        "name": "canonical_payload",
        "type": "text",
        "nullable": true
    });
    declaration["custom_operations"]["receiving.record_receipt"]["statements"]["claim_command"]["row"]
        [0] = json!({
        "name": "claimed_receipt_id",
        "type": "text",
        "nullable": true
    });
    let package = shipped_generation(&catalog, &declaration).unwrap();
    let source_map = artifact_json(
        &package,
        "generated/source-map/receiving_record_receipt.json",
    );
    let accessor = object_named(
        source_map["wamn_accessors"].as_array().unwrap(),
        "name",
        "claim_command",
    );
    let bind = object_named(
        accessor["binds"].as_array().unwrap(),
        "parameter",
        "canonical_payload",
    );
    assert_eq!(bind["postgres"], "text");
    assert_eq!(bind["nullable"], true);
    let row = object_named(
        source_map["wamn_rows"].as_array().unwrap(),
        "name",
        "ClaimCommandRow",
    );
    let field = object_named(
        row["fields"].as_array().unwrap(),
        "name",
        "claimed_receipt_id",
    );
    assert_eq!(field["type"], "Option<String>");

    let mut duplicate_path = shipped_manifest();
    duplicate_path["custom_operations"]["receiving.record_receipt"]["statements"]["claim_command"]
        ["path"] = json!("command/record_receipt/find_replay.sql");
    assert_eq!(
        shipped_generation(&catalog, &duplicate_path)
            .expect_err("two statements consumed one authored SQL path")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn custom_operation_ir_references_require_declared_fields_and_named_constraints() {
    let manifest = shipped_manifest();
    let catalog = receiving_catalog();
    let ledger = table(&catalog, "record_receipt_command");

    let missing_field = replacing_table(
        &catalog,
        rebuilt_table(
            ledger,
            ledger
                .columns()
                .iter()
                .filter(|column| column.name() != "canonical_command")
                .cloned()
                .collect(),
            ledger.constraints().to_vec(),
        ),
    );
    assert_eq!(
        shipped_generation(&missing_field, &manifest)
            .unwrap_err()
            .kind(),
        GenerateErrorKind::UnknownColumn
    );

    let missing_ledger_constraint = replacing_table(
        &catalog,
        rebuilt_table(
            ledger,
            ledger.columns().to_vec(),
            ledger
                .constraints()
                .iter()
                .filter(|constraint| {
                    constraint.name() != "record_receipt_command_idempotency_key_pkey"
                })
                .cloned()
                .collect(),
        ),
    );
    assert_eq!(
        shipped_generation(&missing_ledger_constraint, &manifest)
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let receipt = table(&catalog, "receipt");
    let missing_constraint = replacing_table(
        &catalog,
        rebuilt_table(
            receipt,
            receipt.columns().to_vec(),
            receipt
                .constraints()
                .iter()
                .filter(|constraint| {
                    constraint.name() != "receipt_purchase_order_id_receipt_reference_key"
                })
                .cloned()
                .collect(),
        ),
    );
    assert_eq!(
        shipped_generation(&missing_constraint, &manifest)
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mapped_check_constraint = replacing_table(
        &catalog,
        rebuilt_table(
            receipt,
            receipt.columns().to_vec(),
            receipt
                .constraints()
                .iter()
                .map(|constraint| {
                    if constraint.name() == "receipt_purchase_order_id_receipt_reference_key" {
                        Constraint::check(constraint.name(), "receipt_reference <> ''").unwrap()
                    } else {
                        constraint.clone()
                    }
                })
                .collect(),
        ),
    );
    let package = shipped_generation(&mapped_check_constraint, &manifest).unwrap();
    let errors = artifact_json(
        &package,
        "generated/contracts/receiving/record_receipt.errors.json",
    );
    let mapped = errors["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["literal"] == "receipt_reference_conflict")
        .unwrap();
    assert_eq!(mapped["from"], "check_violation");
}

#[test]
fn additive_unused_column_on_consumed_relation_preserves_required_contract() {
    let manifest = shipped_manifest();
    let catalog = receiving_catalog();
    let base = shipped_generation(&catalog, &manifest).unwrap();
    let command_ledger = table(&catalog, "record_receipt_command");
    let mut columns = command_ledger.columns().to_vec();
    columns.push(Column::new(
        "unused_receiving_note",
        ColumnType::Text,
        true,
        None,
        None,
    ));
    let additive_catalog = replacing_table(
        &catalog,
        rebuilt_table(
            command_ledger,
            columns,
            command_ledger.constraints().to_vec(),
        ),
    );
    let additive = shipped_generation(&additive_catalog, &manifest).unwrap();
    let base_weld = artifact_json(&base, "generated/package-weld.json");
    let additive_weld = artifact_json(&additive, "generated/package-weld.json");

    assert_ne!(
        base_weld["verified_schema_state_id"],
        additive_weld["verified_schema_state_id"]
    );
    assert_eq!(
        base_weld["required_schema_contract"],
        additive_weld["required_schema_contract"]
    );
}

/// A lineless command declares canonicalization without a line profile.
///
/// `line_order` names how a LINE SET is ordered before hashing, and its one
/// spelling is `purchase_order_line_id_ascending` — another package's table.
/// Requiring it of every command made the vocabulary un-nameable by the second
/// application to arrive, which is the shape a two-package test exists to
/// find. It is optional now, and absent means the command carries no lines.
#[test]
fn a_lineless_command_canonicalizes_without_a_line_profile() {
    let mut manifest = shipped_manifest();
    let command = manifest["custom_operations"]["receiving.record_receipt"]
        .as_object_mut()
        .expect("the shipped command is an object");

    // Strip the line set and everything that describes one, leaving a command
    // shaped like WMS's `inventory.move`.
    command["input"]
        .as_object_mut()
        .expect("input")
        .remove("line");
    let fields = command["input"]["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .filter(|field| {
            !field["path"]
                .as_str()
                .is_some_and(|path| path.contains("line[]"))
        })
        .cloned()
        .collect::<Vec<Value>>();
    command["input"]["fields"] = Value::Array(fields);
    let canonicalization = command["canonicalization"]
        .as_object_mut()
        .expect("canonicalization");
    canonicalization.remove("line_order");
    canonicalization.remove("duplicate_line");

    validate_operation_vocabulary(&parsed_manifest(&manifest))
        .expect("a lineless command canonicalizes its top-level fields alone");
}

/// The INPUT decides. A line profile without a line set, or a line set without
/// one, is a manifest that cannot mean what it says — so both refuse rather
/// than one silently governing nothing.
#[test]
fn a_line_profile_and_a_line_input_must_agree() {
    let strip_line_input = |manifest: &mut Value| {
        manifest["custom_operations"]["receiving.record_receipt"]["input"]
            .as_object_mut()
            .expect("input")
            .remove("line");
    };
    let strip_line_profile = |manifest: &mut Value| {
        let canonicalization =
            manifest["custom_operations"]["receiving.record_receipt"]["canonicalization"]
                .as_object_mut()
                .expect("canonicalization");
        canonicalization.remove("line_order");
        canonicalization.remove("duplicate_line");
    };

    for (label, mutate) in [
        (
            "a profile with no line input",
            Box::new(strip_line_input) as Box<dyn Fn(&mut Value)>,
        ),
        ("a line input with no profile", Box::new(strip_line_profile)),
    ] {
        let mut manifest = shipped_manifest();
        mutate(&mut manifest);
        let refusal = validate_operation_vocabulary(&parsed_manifest(&manifest)).expect_err(label);
        assert_eq!(
            refusal.kind(),
            GenerateErrorKind::InvalidOperation,
            "{label}"
        );
    }
}

/// `line_order` and `duplicate_line` are declared together: a command that
/// said how to order lines but not what a repeat costs would leave half a rule.
#[test]
fn the_two_line_members_are_declared_together() {
    for member in ["line_order", "duplicate_line"] {
        let mut manifest = shipped_manifest();
        manifest["custom_operations"]["receiving.record_receipt"]["canonicalization"]
            .as_object_mut()
            .expect("canonicalization")
            .remove(member);
        let refusal = validate_operation_vocabulary(&parsed_manifest(&manifest))
            .expect_err("half a line profile refuses");
        assert_eq!(
            refusal.kind(),
            GenerateErrorKind::InvalidOperation,
            "{member}"
        );
    }
}

// ---------------------------------------------------------------------------
// Generated create: command-identity-from-claim.
//
// The law, ratified 2026-09-03: any identity a command creates comes from the
// CLAIM, not from the work. A create that let PostgreSQL default its row id
// would mint a SECOND id on replay -- a duplicate IDENTITY, which is real stock
// on a row nothing points at, not merely a duplicate row.
//
// These tests are the generator's proof. No package declares a create today, so
// the emitted statements have no in-cluster consumer yet; the executing proof
// is due on the first one.
// ---------------------------------------------------------------------------

const CLAIM_TABLE: &str = "purchase_order_command";

/// One labelled way to break the create's closed error vocabulary.
type ErrorVocabularyMutant = (&'static str, Box<dyn Fn(&mut Value)>);

fn claim_columns() -> Vec<Column> {
    vec![
        Column::new("canonical_command", ColumnType::Bytes, false, None, None),
        Column::new("idempotency_key", ColumnType::Text, false, None, None),
        Column::new(
            "purchase_order_id",
            ColumnType::Uuid,
            false,
            Some(ColumnDefault::GenRandomUuid),
            None,
        ),
    ]
}

fn claim_constraints() -> Vec<Constraint> {
    vec![
        Constraint::primary_key(
            "purchase_order_command_idempotency_key_pkey",
            ["idempotency_key"],
        )
        .unwrap(),
        Constraint::unique(
            "purchase_order_command_purchase_order_id_key",
            ["purchase_order_id"],
        )
        .unwrap(),
    ]
}

fn claim_catalog_with(model: Table, claim: Table) -> CatalogIr {
    CatalogIr::new(vec![model, claim])
}

fn claim_catalog() -> CatalogIr {
    let model = table(&catalog(false), "purchase_order").clone();
    claim_catalog_with(
        model,
        Table::new(
            "receiving",
            CLAIM_TABLE,
            claim_columns(),
            claim_constraints(),
            Vec::new(),
        ),
    )
}

fn claim_manifest() -> Value {
    let mut manifest = manifest();
    manifest["models"]["purchase_order"]["operations"]["create"] = json!({
        "permission": "purchase_order.create",
        "error_details": {
            "invalid_input": {"required": ["field"]},
            "idempotency_conflict": {"required": ["field"]},
            "unique_violation": {"required": ["constraint"]},
            "check_violation": {"required": ["constraint"]},
            "retry": {},
            "timeout": {},
            "permission_denied": {"required": ["operation"]},
            "internal_error": {}
        },
        "writable_fields": ["supplier_id"],
        "claim": {
            "table": CLAIM_TABLE,
            "identities": {"id": "purchase_order_id"}
        },
        "result": "one"
    });
    manifest["internal_relations"] = json!({
        CLAIM_TABLE: {"schema": "receiving", "table": CLAIM_TABLE, "cdc": "excluded"}
    });
    manifest
}

fn generated_create_sql(package: &GeneratedPackage, statement: &str) -> String {
    String::from_utf8(
        package
            .file(&format!("generated/sql/purchase_order/{statement}.sql"))
            .unwrap_or_else(|| panic!("{statement} was emitted"))
            .bytes()
            .to_vec(),
    )
    .unwrap()
}

/// EXIT GATE: every identity the create hands out is written once, under the
/// claim's primary key, and bound into the insert rather than defaulted.
///
/// Read the three statements together. `create_claim` mints the ids under
/// `idempotency_key PRIMARY KEY`, so a second call with that key mints nothing.
/// `create` BINDS them (`$1::uuid`), so it cannot invent a different one.
/// `create_replay` reads the durable original back through the claim. That is
/// why a replay returns the same id BY CONSTRUCTION and not by an early return.
#[test]
fn generated_create_takes_every_identity_from_the_claim() {
    let package = run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES).unwrap();

    assert_eq!(
        generated_create_sql(&package, "create_claim"),
        "INSERT INTO purchase_order_command (idempotency_key, canonical_command)\n\
         VALUES ($1::text, $2::bytea)\n\
         ON CONFLICT ON CONSTRAINT purchase_order_command_idempotency_key_pkey DO NOTHING\n\
         RETURNING\n    purchase_order_id;\n",
    );
    assert_eq!(
        generated_create_sql(&package, "create"),
        "INSERT INTO purchase_order (id, supplier_id)\n\
         VALUES ($1::uuid, $2::uuid)\n\
         RETURNING\n    \
         created_at,\n    id,\n    purchase_order_number,\n    row_version,\n    status,\n    supplier_id;\n",
    );
    assert_eq!(
        generated_create_sql(&package, "create_replay"),
        "SELECT\n    claim.canonical_command,\n    \
         model.created_at,\n    model.id,\n    model.purchase_order_number,\n    \
         model.row_version,\n    model.status,\n    model.supplier_id\n\
         FROM purchase_order_command AS claim\n\
         JOIN purchase_order AS model\n    ON model.id = claim.purchase_order_id\n\
         WHERE claim.idempotency_key = $1::text;\n",
    );
}

/// EXIT GATE: the replay path performs ZERO writes.
///
/// A replay that re-ran the insert would be the duplicate the key exists to
/// prevent, so this reads the emitted text rather than trusting the caller.
#[test]
fn generated_create_replay_writes_nothing() {
    let package = run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES).unwrap();
    let replay = generated_create_sql(&package, "create_replay");

    assert!(replay.starts_with("SELECT\n"), "{replay}");
    for write in [
        "INSERT",
        "UPDATE",
        "DELETE",
        "MERGE",
        "FOR UPDATE",
        "nextval",
    ] {
        assert!(!replay.contains(write), "replay must not {write}: {replay}");
    }
    // The insert never defaults an identity: every id is a bind.
    let create = generated_create_sql(&package, "create");
    assert!(!create.contains("gen_random_uuid"), "{create}");
    assert!(!create.contains("DEFAULT"), "{create}");
}

/// EXIT GATE: the create's contracts carry the claim, the replay rule and the
/// typed refusal for a key rebound to a different request.
#[test]
fn generated_create_contracts_publish_the_claim_and_its_refusal() {
    let package = run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES).unwrap();

    let operation = artifact_json(
        &package,
        "generated/contracts/purchase_order/create.operation.json",
    );
    assert_eq!(
        operation["idempotency"],
        json!({
            "key": "idempotency_key",
            "canonical_command": "canonical_command",
            "claim": {
                "schema": "receiving",
                "table": CLAIM_TABLE,
                "constraint": "purchase_order_command_idempotency_key_pkey",
                "identities": {"id": "purchase_order_id"},
            },
            "statements": {
                "claim": "create_claim",
                "replay": "create_replay",
                "insert": "create",
            },
            "replay": {"writes": "none", "identity_source": "claim"},
            "conflict": {
                "on": "changed_canonical_command",
                "refusal": "idempotency_conflict",
            },
            "atomicity": "claim_and_insert_commit_together",
        })
    );
    let statements = operation["statements"].as_array().unwrap();
    assert_eq!(
        statements
            .iter()
            .map(|statement| statement["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["create_claim", "create_replay", "create"]
    );
    // The claim statement returns the minted identity, not the model row.
    assert_eq!(
        statements[0]["columns"],
        json!([{"name": "purchase_order_id", "type": "uuid", "nullable": false}])
    );
    assert_eq!(
        statements[0]["binds"],
        json!([
            {"name": "idempotency_key", "type": "text", "nullable": false},
            {"name": "canonical_command", "type": "bytes", "nullable": false},
        ])
    );
    // The insert binds the claim-minted id ahead of the writable fields.
    assert_eq!(
        statements[2]["binds"],
        json!([
            {"name": "id", "type": "uuid", "nullable": false},
            {"name": "supplier_id", "type": "uuid", "nullable": false},
        ])
    );

    let errors = artifact_json(
        &package,
        "generated/contracts/purchase_order/create.errors.json",
    );
    let conflict = errors["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["literal"] == "idempotency_conflict")
        .expect("a changed request for a live key is typed-refused");
    assert_eq!(conflict["from"], json!("changed_canonical_command"));
    assert_eq!(conflict["detail"]["required"], json!(["field"]));

    let input = artifact_json(
        &package,
        "generated/contracts/purchase_order/create.input.json",
    );
    assert_eq!(
        input["idempotency_key"],
        json!({"type": "text", "required": true})
    );
    assert_eq!(
        input["canonical_command"],
        json!({
            "over": "writable_fields",
            "payload": "canonical_compact_json",
            "changed": "idempotency_conflict",
        })
    );
}

/// EXIT GATE: the claim's shape reaches the required-schema contract and the
/// data-access overlay, so nothing the emitted SQL names is left unpinned.
#[test]
fn generated_create_pins_and_grants_its_claim_relation() {
    let package = run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES).unwrap();

    let weld = artifact_json(&package, "generated/package-weld.json");
    let claim = object_named(
        weld["required_schema_contract"]["tables"]
            .as_array()
            .unwrap(),
        "table",
        CLAIM_TABLE,
    );
    assert_eq!(
        claim["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|field| field["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["canonical_command", "idempotency_key", "purchase_order_id"]
    );
    assert_eq!(
        claim["constraints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|constraint| constraint["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "purchase_order_command_idempotency_key_pkey",
            "purchase_order_command_purchase_order_id_key",
        ]
    );

    let overlay = artifact_json(&package, DATA_ACCESS_OVERLAY_PATH);
    let relations = overlay["relations"].as_array().unwrap();
    let granted = object_named(relations, "table", CLAIM_TABLE);
    assert_eq!(
        granted["select_fields"],
        json!(["canonical_command", "idempotency_key", "purchase_order_id"])
    );
    assert_eq!(
        granted["insert_fields"],
        json!(["canonical_command", "idempotency_key"])
    );
    // A claim is written once: nothing may update it.
    assert_eq!(granted["update_fields"], json!([]));
    assert_eq!(granted["lock"], json!(false));
    // The model insert writes the claim-minted id, so the grant must allow it.
    let model = object_named(relations, "table", "purchase_order");
    assert_eq!(model["insert_fields"], json!(["id", "supplier_id"]));
}

/// EXIT GATE: every way the claim could stop being the identity source refuses.
///
/// The unmutated manifest and catalog run FIRST as the negative control: if
/// they did not generate, a refusal below would prove nothing.
#[test]
fn generated_create_refuses_a_claim_that_does_not_pre_generate_identity() {
    run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES)
        .expect("the unmutated claim generates");

    let model = || table(&catalog(false), "purchase_order").clone();
    let claim_table = |columns, constraints| {
        Table::new("receiving", CLAIM_TABLE, columns, constraints, Vec::new())
    };
    let without = |name: &str| {
        claim_columns()
            .into_iter()
            .filter(|column| column.name() != name)
            .collect::<Vec<_>>()
    };
    let replacing = |replacement: Column| {
        claim_columns()
            .into_iter()
            .map(|column| {
                if column.name() == replacement.name() {
                    replacement.clone()
                } else {
                    column
                }
            })
            .collect::<Vec<_>>()
    };

    let cases: Vec<(&str, CatalogIr, Value, GenerateErrorKind)> = vec![
        (
            "no claim declared at all",
            claim_catalog(),
            {
                let mut manifest = claim_manifest();
                manifest["models"]["purchase_order"]["operations"]["create"]
                    .as_object_mut()
                    .unwrap()
                    .remove("claim");
                manifest
            },
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "the claim relation does not exist",
            claim_catalog(),
            {
                let mut manifest = claim_manifest();
                manifest["models"]["purchase_order"]["operations"]["create"]["claim"]["table"] =
                    json!("absent_command");
                manifest["internal_relations"] = json!({
                    "absent_command": {
                        "schema": "receiving", "table": "absent_command", "cdc": "excluded"
                    }
                });
                manifest
            },
            GenerateErrorKind::UnknownRelation,
        ),
        (
            "the claim is not CDC-excluded, so mechanism state would ship as events",
            claim_catalog(),
            {
                let mut manifest = claim_manifest();
                manifest["internal_relations"] = json!({});
                manifest
            },
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "the identity column carries no gen_random_uuid default",
            claim_catalog_with(
                model(),
                claim_table(
                    replacing(Column::new(
                        "purchase_order_id",
                        ColumnType::Uuid,
                        false,
                        None,
                        None,
                    )),
                    claim_constraints(),
                ),
            ),
            claim_manifest(),
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "the identity column is nullable, so the claim may mint nothing",
            claim_catalog_with(
                model(),
                claim_table(
                    replacing(Column::new(
                        "purchase_order_id",
                        ColumnType::Uuid,
                        true,
                        Some(ColumnDefault::GenRandomUuid),
                        None,
                    )),
                    claim_constraints(),
                ),
            ),
            claim_manifest(),
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "the identity column is not UNIQUE, so two claims could mint one id",
            claim_catalog_with(
                model(),
                claim_table(
                    claim_columns(),
                    vec![
                        Constraint::primary_key(
                            "purchase_order_command_idempotency_key_pkey",
                            ["idempotency_key"],
                        )
                        .unwrap(),
                    ],
                ),
            ),
            claim_manifest(),
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "the key is not a primary key, so a second call could claim it again",
            claim_catalog_with(
                model(),
                claim_table(
                    claim_columns(),
                    vec![
                        Constraint::unique(
                            "purchase_order_command_purchase_order_id_key",
                            ["purchase_order_id"],
                        )
                        .unwrap(),
                    ],
                ),
            ),
            claim_manifest(),
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "the claim carries no canonical command to compare a replay against",
            claim_catalog_with(
                model(),
                claim_table(without("canonical_command"), claim_constraints()),
            ),
            claim_manifest(),
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "a claim column has no value the generated claim insert can supply",
            claim_catalog_with(
                model(),
                claim_table(
                    claim_columns()
                        .into_iter()
                        .chain([Column::new("actor_id", ColumnType::Uuid, false, None, None)])
                        .collect(),
                    claim_constraints(),
                ),
            ),
            claim_manifest(),
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "the declared identity is not a column of the claim",
            claim_catalog(),
            {
                let mut manifest = claim_manifest();
                manifest["models"]["purchase_order"]["operations"]["create"]["claim"]["identities"] =
                    json!({"id": "absent_id"});
                manifest
            },
            GenerateErrorKind::UnknownColumn,
        ),
        (
            "only a create may carry a claim",
            claim_catalog(),
            {
                let mut manifest = claim_manifest();
                manifest["models"]["purchase_order"]["operations"]["update"]["claim"] = json!({
                    "table": CLAIM_TABLE,
                    "identities": {"id": "purchase_order_id"}
                });
                manifest
            },
            GenerateErrorKind::InvalidOperation,
        ),
    ];

    for (label, catalog, manifest, kind) in cases {
        let refusal = run(&catalog, &manifest, &QUERY_SOURCES).expect_err(label);
        assert_eq!(refusal.kind(), kind, "{label}");
    }
}

/// EXIT GATE: a second identity added to the model breaks the build unless the
/// claim pre-generates it too.
///
/// This is the enumeration the law demands, made mechanical: the generator
/// derives the minted set from the catalog, so a new `gen_random_uuid()` column
/// cannot slip through and be re-minted on replay.
#[test]
fn a_model_identity_the_claim_does_not_mint_refuses() {
    let with_second_identity = rebuilt_table(
        table(&catalog(false), "purchase_order"),
        table(&catalog(false), "purchase_order")
            .columns()
            .to_vec()
            .into_iter()
            .chain([Column::new(
                "external_id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            )])
            .collect(),
        table(&catalog(false), "purchase_order")
            .constraints()
            .to_vec(),
    );
    let claim = Table::new(
        "receiving",
        CLAIM_TABLE,
        claim_columns(),
        claim_constraints(),
        Vec::new(),
    );
    let unmapped = claim_catalog_with(with_second_identity.clone(), claim);
    let mut manifest = claim_manifest();
    manifest["models"]["purchase_order"]["server_owned_fields"] = json!([
        "id",
        "external_id",
        "purchase_order_number",
        "status",
        "row_version",
        "created_at"
    ]);

    let refusal =
        run(&unmapped, &manifest, &QUERY_SOURCES).expect_err("an unminted identity refuses");
    assert_eq!(refusal.kind(), GenerateErrorKind::InvalidOperation);

    // Mapping it to its own pre-generated claim column restores generation.
    let mapped = claim_catalog_with(
        with_second_identity,
        Table::new(
            "receiving",
            CLAIM_TABLE,
            claim_columns()
                .into_iter()
                .chain([Column::new(
                    "external_id",
                    ColumnType::Uuid,
                    false,
                    Some(ColumnDefault::GenRandomUuid),
                    None,
                )])
                .collect(),
            claim_constraints()
                .into_iter()
                .chain([Constraint::unique(
                    "purchase_order_command_external_id_key",
                    ["external_id"],
                )
                .unwrap()])
                .collect(),
            Vec::new(),
        ),
    );
    manifest["models"]["purchase_order"]["operations"]["create"]["claim"]["identities"] =
        json!({"external_id": "external_id", "id": "purchase_order_id"});
    let package = run(&mapped, &manifest, &QUERY_SOURCES).expect("both identities are claimed");
    assert_eq!(
        generated_create_sql(&package, "create"),
        "INSERT INTO purchase_order (external_id, id, supplier_id)\n\
         VALUES ($1::uuid, $2::uuid, $3::uuid)\n\
         RETURNING\n    \
         created_at,\n    external_id,\n    id,\n    purchase_order_number,\n    \
         row_version,\n    status,\n    supplier_id;\n",
    );
}

/// EXIT GATE: `idempotency_conflict` is a required, exactly-shaped member of
/// the create's closed error vocabulary, and belongs to no other action.
#[test]
fn idempotency_conflict_is_closed_to_the_create() {
    let cases: [ErrorVocabularyMutant; 3] = [
        (
            "a create that does not declare the refusal",
            Box::new(|manifest: &mut Value| {
                manifest["models"]["purchase_order"]["operations"]["create"]["error_details"]
                    .as_object_mut()
                    .unwrap()
                    .remove("idempotency_conflict");
            }),
        ),
        (
            "a create whose refusal carries the wrong structured detail",
            Box::new(|manifest: &mut Value| {
                manifest["models"]["purchase_order"]["operations"]["create"]["error_details"]["idempotency_conflict"] =
                    json!({"required": ["constraint"]});
            }),
        ),
        (
            "an update that claims the refusal",
            Box::new(|manifest: &mut Value| {
                manifest["models"]["purchase_order"]["operations"]["update"]["error_details"]["idempotency_conflict"] =
                    json!({"required": ["field"]});
            }),
        ),
    ];
    for (label, mutate) in cases {
        let mut manifest = claim_manifest();
        mutate(&mut manifest);
        let refusal = run(&claim_catalog(), &manifest, &QUERY_SOURCES).expect_err(label);
        assert_eq!(
            refusal.kind(),
            GenerateErrorKind::InvalidOperation,
            "{label}"
        );
    }
}

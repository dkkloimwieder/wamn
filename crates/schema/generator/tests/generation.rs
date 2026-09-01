use std::collections::BTreeSet;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use wamn_execution_contract::canonical_json_bytes;
use wamn_schema_generator::{
    AuthoredSql, GenerateErrorKind, GeneratedPackage, GenerationInput, GenerationProvenance,
    OperationVisibility, PackageManifest, corpus_sha256, generate, validate_operation_vocabulary,
    validate_parity_json,
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
const RECEIVING_SOURCES: [AuthoredSql<'static>; 15] = [
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
            "purchase_order_get": {
                "operations": ["purchase_order.get"],
                "connections": ["postgres"]
            },
            "purchase_order_query": {
                "operations": ["purchase_order.query"],
                "connections": ["postgres"]
            },
            "purchase_order_update": {
                "operations": ["purchase_order.update"],
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
        "quality.load_purchase_order_detail": {
            "kind": "projection",
            "visibility": "public",
            "permission": "quality.load_purchase_order_detail"
        },
        "quality.create_inspection": {
            "kind": "event_handler",
            "visibility": "private",
            "registration": {
                "source_package": "wamn_receiving",
                "entity": "receipt",
                "ops": ["insert"]
            }
        }
    });
    overlay["components"]["quality_load_purchase_order_detail"] = json!({
        "operations": ["quality.load_purchase_order_detail"],
        "connections": ["postgres"]
    });
    overlay["components"]["quality_create_inspection"] = json!({
        "operations": ["quality.create_inspection"],
        "connections": ["postgres"]
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
    let generated = run(&catalog(false), &overlay, &QUERY_SOURCES)
        .expect("the exact overlay vocabulary must validate against its effective catalog");
    let projection = artifact_json(
        &generated,
        "generated/contracts/quality/load_purchase_order_detail.operation.json",
    );
    assert_eq!(projection["kind"], "projection");
    assert_eq!(projection["visibility"], "public");
    let handler = artifact_json(
        &generated,
        "generated/contracts/quality/create_inspection.operation.json",
    );
    assert_eq!(handler["kind"], "event_handler");
    assert_eq!(handler["visibility"], "private");
    assert_eq!(handler["permission_token"], Value::Null);
    assert_eq!(handler["registration"]["source_package"], "wamn_receiving");
    assert_eq!(handler["registration"]["ops"], json!(["insert"]));
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

    let mut missing_registration = overlay_manifest();
    missing_registration["custom_operations"]["quality.create_inspection"]
        .as_object_mut()
        .expect("event handler object")
        .remove("registration");
    assert!(
        PackageManifest::from_slice(
            &serde_json::to_vec(&missing_registration).expect("serialize manifest")
        )
        .is_err()
    );

    let mut projection_registration = overlay_manifest();
    projection_registration["custom_operations"]["quality.load_purchase_order_detail"]["registration"] =
        json!({"source_package": "wamn_receiving", "entity": "receipt", "ops": ["insert"]});
    assert!(
        PackageManifest::from_slice(
            &serde_json::to_vec(&projection_registration).expect("serialize manifest")
        )
        .is_err()
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
    command_detail["custom_operations"]["receiving.record_receipt"]["error_details"]["receipt_reference_conflict"]
        ["required"] = json!(["field"]);
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&command_detail))
            .expect_err("an incorrect command detail schema was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn shared_operation_vocabulary_refuses_non_singular_unknown_repeated_and_missing_components() {
    let mut non_singular = manifest();
    non_singular["components"]["purchase_order_get"]["operations"] =
        json!(["purchase_order.get", "purchase_order.query"]);
    let error = validate_operation_vocabulary(&parsed_manifest(&non_singular))
        .expect_err("a component owning multiple operations was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "purchase_order_get must declare exactly one operation"
    );
    assert_eq!(
        run(&catalog(false), &non_singular, &QUERY_SOURCES)
            .expect_err("generation accepted a component owning multiple operations")
            .kind(),
        GenerateErrorKind::InvalidComponent
    );

    let mut empty = manifest();
    empty["components"]["purchase_order_get"]["operations"] = json!([]);
    let error = validate_operation_vocabulary(&parsed_manifest(&empty))
        .expect_err("a component owning no operation was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "purchase_order_get must declare exactly one operation"
    );

    let mut unknown = manifest();
    unknown["components"]["purchase_order_get"]["operations"] = json!(["purchase_order.delete"]);
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&unknown))
            .expect_err("unknown grouped operation was accepted")
            .kind(),
        GenerateErrorKind::InvalidComponent
    );
    assert_eq!(
        run(&catalog(false), &unknown, &QUERY_SOURCES)
            .expect_err("generation accepted an unknown grouped operation")
            .kind(),
        GenerateErrorKind::InvalidComponent
    );

    let mut repeated = manifest();
    repeated["components"]["purchase_order_update"]["operations"] = json!(["purchase_order.get"]);
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&repeated))
            .expect_err("repeated grouped operation was accepted")
            .kind(),
        GenerateErrorKind::InvalidComponent
    );
    assert_eq!(
        run(&catalog(false), &repeated, &QUERY_SOURCES)
            .expect_err("generation accepted a repeated grouped operation")
            .kind(),
        GenerateErrorKind::InvalidComponent
    );

    let mut missing = manifest();
    missing["components"]
        .as_object_mut()
        .expect("components object")
        .remove("purchase_order_update");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&missing))
            .expect_err("ungrouped declared operation was accepted")
            .kind(),
        GenerateErrorKind::InvalidComponent
    );
    assert_eq!(
        run(&catalog(false), &missing, &QUERY_SOURCES)
            .expect_err("generation accepted an ungrouped declared operation")
            .kind(),
        GenerateErrorKind::InvalidComponent
    );
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

    let mut version = manifest();
    version["package"]["version"] = json!("1.0.0::shadow");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&version))
            .expect_err("ambiguous package version was accepted")
            .kind(),
        GenerateErrorKind::InvalidIdentity
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
                {"name": "observed_row_version", "type": "Option<i64>"},
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
    assert_eq!(command["sql_files"].as_array().unwrap().len(), 9);
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

    let overlay = artifact_json(&package, "generated/platform-policy/data-access.json");
    assert_eq!(overlay["role"], "wamn_app");
    assert_eq!(overlay["contract"], "receiving_data_access");
    let location = overlay["relations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|relation| relation["table"] == "location")
        .unwrap();
    assert_eq!(location["select_fields"], json!(["id"]));
    assert_eq!(location["update_fields"], json!([]));
    assert_eq!(location["lock"], true);
    assert_eq!(location["lock_update_field"], "id");

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
    assert_eq!(operation["sql_files"], Value::Array(sql_files.clone()));
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
            accessor["sql_constant"],
            format!("{}_SQL", statement_name.to_ascii_uppercase())
        );
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
fn command_statement_vocabulary_refuses_signature_and_path_mutants() {
    let catalog = receiving_catalog();
    let mutations = [
        (
            "path",
            "/custom_operations/receiving.record_receipt/statements/claim_command/path",
            json!("command/record_receipt/find_replay.sql"),
        ),
        (
            "fetch",
            "/custom_operations/receiving.record_receipt/statements/claim_command/fetch",
            json!("one"),
        ),
        (
            "parameter type",
            "/custom_operations/receiving.record_receipt/statements/claim_command/parameters/1/type",
            json!("text"),
        ),
        (
            "parameter name",
            "/custom_operations/receiving.record_receipt/statements/claim_command/parameters/1/name",
            json!("canonical_payload"),
        ),
        (
            "parameter nullability",
            "/custom_operations/receiving.record_receipt/statements/claim_command/parameters/1/nullable",
            json!(true),
        ),
        (
            "row type",
            "/custom_operations/receiving.record_receipt/statements/claim_command/row/0/type",
            json!("text"),
        ),
        (
            "row name",
            "/custom_operations/receiving.record_receipt/statements/claim_command/row/0/name",
            json!("claimed_receipt_id"),
        ),
        (
            "row nullability",
            "/custom_operations/receiving.record_receipt/statements/claim_command/row/0/nullable",
            json!(true),
        ),
    ];

    for (name, pointer, replacement) in mutations {
        let mut manifest = shipped_manifest();
        *manifest.pointer_mut(pointer).unwrap() = replacement;
        let error = shipped_generation(&catalog, &manifest).expect_err(name);
        assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation, "{name}");
    }
}

#[test]
fn command_ir_references_refuse_missing_or_changed_fields_and_named_constraints() {
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

    for (column_type, nullable) in [(ColumnType::Text, false), (ColumnType::Bytes, true)] {
        let changed_field = replacing_table(
            &catalog,
            rebuilt_table(
                ledger,
                ledger
                    .columns()
                    .iter()
                    .map(|column| {
                        if column.name() == "canonical_command" {
                            Column::new(
                                column.name(),
                                column_type,
                                nullable,
                                column.default(),
                                column.generation().cloned(),
                            )
                        } else {
                            column.clone()
                        }
                    })
                    .collect(),
                ledger.constraints().to_vec(),
            ),
        );
        assert_eq!(
            shipped_generation(&changed_field, &manifest)
                .unwrap_err()
                .kind(),
            GenerateErrorKind::InvalidOperation
        );
    }

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

    let changed_ledger_constraint = replacing_table(
        &catalog,
        rebuilt_table(
            ledger,
            ledger.columns().to_vec(),
            ledger
                .constraints()
                .iter()
                .map(|constraint| {
                    if constraint.name() == "record_receipt_command_idempotency_key_pkey" {
                        Constraint::primary_key(constraint.name(), ["receipt_id"]).unwrap()
                    } else {
                        constraint.clone()
                    }
                })
                .collect(),
        ),
    );
    assert_eq!(
        shipped_generation(&changed_ledger_constraint, &manifest)
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

    let changed_constraint = replacing_table(
        &catalog,
        rebuilt_table(
            receipt,
            receipt.columns().to_vec(),
            receipt
                .constraints()
                .iter()
                .map(|constraint| {
                    if constraint.name() == "receipt_purchase_order_id_receipt_reference_key" {
                        Constraint::unique(constraint.name(), ["receipt_reference"]).unwrap()
                    } else {
                        constraint.clone()
                    }
                })
                .collect(),
        ),
    );
    assert_eq!(
        shipped_generation(&changed_constraint, &manifest)
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn additive_unused_column_on_consumed_relation_preserves_required_contract() {
    let manifest = shipped_manifest();
    let catalog = receiving_catalog();
    let base = shipped_generation(&catalog, &manifest).unwrap();
    let location = table(&catalog, "location");
    let mut columns = location.columns().to_vec();
    columns.push(Column::new(
        "unused_receiving_note",
        ColumnType::Text,
        true,
        None,
        None,
    ));
    let additive_catalog = replacing_table(
        &catalog,
        rebuilt_table(location, columns, location.constraints().to_vec()),
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

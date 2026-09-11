use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use wamn_schema_generator::{
    AuthoredSql, GeneratedPackage, GenerationInput, GenerationProvenance, PackageManifest,
    StatementTransactionality, generate,
};
use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnDefault, ColumnType, Constraint, Table,
};

pub(super) const QUERY_SOURCES: [AuthoredSql<'static>; 6] = [
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

pub(super) fn catalog(add_unused_table: bool) -> CatalogIr {
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
                Some(ColumnDefault::text("open")),
                None,
            ),
            Column::new(
                "row_version",
                ColumnType::Int64,
                false,
                Some(ColumnDefault::int64(1)),
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

pub(super) fn projection_operation() -> Value {
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

pub(super) fn table<'a>(catalog: &'a CatalogIr, name: &str) -> &'a Table {
    catalog
        .tables()
        .iter()
        .find(|table| table.schema() == "receiving" && table.name() == name)
        .unwrap()
}

pub(super) fn rebuilt_table(table: &Table, columns: Vec<Column>, constraints: Vec<Constraint>) -> Table {
    Table::new(
        table.schema(),
        table.name(),
        columns,
        constraints,
        table.indexes().to_vec(),
    )
}

pub(super) fn replacing_table(catalog: &CatalogIr, replacement: Table) -> CatalogIr {
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

pub(super) fn manifest() -> Value {
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

pub(super) fn run(
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

pub(super) fn artifact_json(package: &GeneratedPackage, path: &str) -> Value {
    serde_json::from_slice(package.file(path).unwrap().bytes()).unwrap()
}

pub(super) fn statement_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

pub(super) fn accessor_bind(
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

pub(super) fn assert_native_fixtures_match_parity(package: &GeneratedPackage, model: &str) {
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

pub(super) fn object_named<'a>(values: &'a [Value], field: &str, name: &str) -> &'a Value {
    values.iter().find(|value| value[field] == name).unwrap()
}

pub(super) fn parsed_manifest(value: &Value) -> PackageManifest {
    PackageManifest::from_slice(&serde_json::to_vec(value).expect("serialize manifest"))
        .expect("parse strict manifest")
}

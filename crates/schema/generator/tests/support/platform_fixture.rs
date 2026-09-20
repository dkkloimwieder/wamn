use std::collections::BTreeMap;

use serde_json::{Value, json};
use wamn_schema_generator::{
    AuthoredSql, GeneratedPackage, GenerationInput, GenerationProvenance,
    StatementTransactionality, generate,
};
use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnDefault, ColumnType, Constraint, Table,
};

const QUERY_SQL: &[u8] =
    b"SELECT id, code, note, edit_version, created_at FROM widget ORDER BY created_at, id;\n";
const QUERY_DESCENDING_SQL: &[u8] =
    b"SELECT id, code, note, edit_version, created_at FROM widget ORDER BY created_at DESC, id DESC;\n";
const ARCHIVE_SQL: &[u8] = b"SELECT id, edit_version FROM widget WHERE id = $1 FOR UPDATE;\n";

pub(crate) fn generate_fixture() -> GeneratedPackage {
    let manifest = serde_json::to_vec(&manifest()).expect("serialize platform fixture manifest");
    generate(&GenerationInput::new(
        &catalog(),
        &manifest,
        &[
            AuthoredSql::new("query/widget.sql", QUERY_SQL),
            AuthoredSql::new(
                "query/widget_by_created_at_descending.sql",
                QUERY_DESCENDING_SQL,
            ),
            AuthoredSql::new("command/widget/archive.sql", ARCHIVE_SQL),
        ],
        GenerationProvenance::new("wamn-schema-generator/0.1.0", "platform-fixture"),
        &StatementTransactionality::default(),
    ))
    .expect("the platform-owned generator fixture generates")
}

pub(crate) fn contracts(package: &GeneratedPackage) -> BTreeMap<String, Vec<u8>> {
    package
        .files()
        .iter()
        .filter_map(|file| {
            file.path()
                .strip_prefix("generated/contracts/")
                .map(|path| (path.to_owned(), file.bytes().to_vec()))
        })
        .collect()
}

fn catalog() -> CatalogIr {
    let widget = Table::new(
        "inventory",
        "widget",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("code", ColumnType::Text, false, None, None),
            Column::new("note", ColumnType::Text, true, None, None),
            Column::new(
                "edit_version",
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
            Constraint::primary_key("widget_pkey", ["id"]).expect("valid primary key"),
            Constraint::unique("widget_code_key", ["code"]).expect("valid unique constraint"),
        ],
        Vec::new(),
    );
    let command = Table::new(
        "inventory",
        "widget_command",
        vec![
            Column::new("canonical_command", ColumnType::Bytes, false, None, None),
            Column::new("idempotency_key", ColumnType::Text, false, None, None),
            Column::new(
                "widget_id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
        ],
        vec![
            Constraint::primary_key("widget_command_pkey", ["idempotency_key"])
                .expect("valid primary key"),
            Constraint::unique("widget_command_widget_id_key", ["widget_id"])
                .expect("valid unique constraint"),
        ],
        Vec::new(),
    );
    CatalogIr::new(vec![widget, command])
}

fn manifest() -> Value {
    json!({
        "package": {"id": "platform_fixture", "version": "1.0.0"},
        "required_platform_policy_contract": {
            "id": "platform_fixture_data_access",
            "state": "unsatisfied"
        },
        "models": {
            "widget": {
                "schema": "inventory",
                "table": "widget",
                "owner": "platform_fixture",
                "server_owned_fields": ["id", "edit_version", "created_at"],
                "audit_log": {"columns": ["created_at"], "retention": "none"},
                "delete_mode": "hard",
                "operations": {
                    "create": {
                        "permission": "widget.create",
                        "writable_fields": ["code", "note"],
                        "claim": {
                            "table": "widget_command",
                            "identities": {"id": "widget_id"}
                        },
                        "result": "one"
                    },
                    "get": {"permission": "widget.get", "result": "one"},
                    "query": {
                        "permission": "widget.query",
                        "authored_sql": {
                            "default": "query/widget.sql",
                            "variants": [
                                {
                                    "field": "created_at",
                                    "direction": "ascending",
                                    "path": "query/widget.sql"
                                },
                                {
                                    "field": "created_at",
                                    "direction": "descending",
                                    "path": "query/widget_by_created_at_descending.sql"
                                }
                            ]
                        },
                        "filters": [{"field": "code"}],
                        "sort": {
                            "fields": ["created_at"],
                            "directions": ["ascending", "descending"]
                        },
                        "pagination": {
                            "default_sort": {"field": "created_at", "direction": "ascending"},
                            "tie_breaker": {"field": "id"}
                        },
                        "limit": {"default": 100, "minimum": 1, "maximum": 100},
                        "result": "page"
                    },
                    "update": {
                        "permission": "widget.update",
                        "writable_fields": ["code", "note"],
                        "revision_field": "edit_version",
                        "result": "one"
                    },
                    "delete": {
                        "permission": "widget.delete",
                        "revision_field": "edit_version",
                        "result": "one"
                    }
                }
            }
        },
        "internal_relations": {
            "widget_command": {
                "schema": "inventory",
                "table": "widget_command",
                "cdc": "excluded"
            }
        },
        "custom_operations": {
            "widget.archive": {
                "kind": "command",
                "visibility": "public",
                "permission": "widget.archive",
                "connection": "postgres",
                "transaction": "explicit_per_input",
                "automatic_retry": false,
                "idempotent_by": {"state": {"guards": {"widget": "expected_edit_version"}}},
                "input": {"fields": [
                    {"path": "id", "type": "uuid", "nullable": false},
                    {
                        "path": "expected_edit_version",
                        "type": "int64",
                        "nullable": false,
                        "revision": true
                    }
                ]},
                "result": {"class": "one", "fields": [
                    {"path": "id", "type": "uuid", "nullable": false},
                    {
                        "path": "edit_version",
                        "type": "int64",
                        "nullable": false,
                        "revision": true
                    },
                    {"path": "note", "type": "text", "nullable": true}
                ]},
                "errors": [
                    "invalid_input", "already_archived", "concurrency_conflict", "retry",
                    "timeout", "permission_denied", "internal_error"
                ],
                "error_details": {"already_archived": {"required": ["field"]}},
                "constraint_errors": {},
                "relations": [{
                    "schema": "inventory",
                    "table": "widget",
                    "select_fields": ["id", "edit_version"],
                    "insert_fields": [],
                    "update_fields": [],
                    "lock": true,
                    "constraints": []
                }],
                "statements": {"archive": {
                    "path": "command/widget/archive.sql",
                    "fetch": "one",
                    "parameters": [{"name": "id", "type": "uuid", "nullable": false}],
                    "row": [
                        {"name": "id", "type": "uuid", "nullable": false},
                        {"name": "edit_version", "type": "int64", "nullable": false}
                    ]
                }}
            }
        },
        "connections": ["postgres"],
        "components": {"fixture": {"connections": ["postgres"]}}
    })
}

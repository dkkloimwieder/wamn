use std::collections::BTreeMap;

use serde_json::{Value, json};
use wamn_schema_generator::client_ir::{ClientContractIr, ReplayIr, ResponseIr, RouteIr};
use wamn_schema_generator::{
    AuthoredSql, GeneratedPackage, GenerationInput, GenerationProvenance,
    StatementTransactionality, generate,
};
use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnDefault, ColumnType, Constraint, Table,
};

pub(crate) const QUERY_SQL: &[u8] =
    b"SELECT id, code, note, edit_version, created_at FROM widget ORDER BY created_at, id;\n";
pub(crate) const QUERY_DESCENDING_SQL: &[u8] =
    b"SELECT id, code, note, edit_version, created_at FROM widget ORDER BY created_at DESC, id DESC;\n";
/// The bounded-list read. It selects exactly what `widget.archive` reads, so
/// the fixture's data-access grant does not move.
pub(crate) const LIST_SQL: &[u8] = b"SELECT id, edit_version FROM widget ORDER BY id;\n";
pub(crate) const ARCHIVE_SQL: &[u8] =
    b"SELECT id, edit_version FROM widget WHERE id = $1 FOR UPDATE;\n";
pub(crate) const CLAIM_SQL: &[u8] = b"INSERT INTO widget_command (canonical_command, idempotency_key) VALUES ($1, $2) ON CONFLICT DO NOTHING RETURNING widget_id;\n";
pub(crate) const REPLAY_SQL: &[u8] =
    b"SELECT canonical_command, widget_id FROM widget_command WHERE idempotency_key = $1;\n";
pub(crate) const FINALIZE_SQL: &[u8] =
    b"SELECT widget_id FROM widget_command WHERE idempotency_key = $1;\n";

pub(crate) fn generate_fixture() -> GeneratedPackage {
    generate_with(&catalog(), &manifest())
}

pub(crate) fn generate_with(catalog: &CatalogIr, value: &Value) -> GeneratedPackage {
    try_generate_with(catalog, value).expect("the platform-owned generator fixture generates")
}

pub(crate) fn try_generate_with(
    catalog: &CatalogIr,
    value: &Value,
) -> Result<GeneratedPackage, wamn_schema_generator::GenerateError> {
    let manifest = serde_json::to_vec(value).expect("serialize platform fixture manifest");
    let sources = [
        AuthoredSql::new("query/widget.sql", QUERY_SQL),
        AuthoredSql::new(
            "query/widget_by_created_at_descending.sql",
            QUERY_DESCENDING_SQL,
        ),
        AuthoredSql::new("query/widget_list.sql", LIST_SQL),
        AuthoredSql::new("command/widget/archive.sql", ARCHIVE_SQL),
        AuthoredSql::new("command/widget/claim.sql", CLAIM_SQL),
        AuthoredSql::new("command/widget/replay.sql", REPLAY_SQL),
        AuthoredSql::new("command/widget/finalize.sql", FINALIZE_SQL),
    ]
    .into_iter()
    .filter(|source| {
        value["models"]["widget"]["operations"]["query"]["authored_sql"]
            .to_string()
            .contains(source.path())
            || value["custom_operations"]
                .to_string()
                .contains(source.path())
    })
    .collect::<Vec<_>>();
    generate(&GenerationInput::new(
        catalog,
        &manifest,
        &sources,
        GenerationProvenance::new("wamn-schema-generator/0.1.0", "platform-fixture"),
        &StatementTransactionality::default(),
    ))
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

pub(crate) fn client_release() -> ClientContractIr {
    let package = generate_fixture();
    let contracts = contracts(&package);
    let routes = [
        "archive", "create", "delete", "get", "list", "query", "update",
    ]
    .into_iter()
    .map(|name| {
        let identity = format!("platform-fixture:widget/{name}@1.0.0");
        (
            identity.clone(),
            RouteIr {
                method: "POST".to_owned(),
                template: format!("/widget/{name}"),
                input_schema: match name {
                    "update" => Some(json!({
                        "type": "array",
                        "items": {"type": "object", "properties": {"change": {
                        "type": "object", "properties": {"note": {
                            "type": ["string", "null"],
                            "x-wamn-explicit-null": "accepted"
                        }, "code": {
                            "type": ["string", "null"],
                            "x-wamn-explicit-null": "invalid_input"
                        }}
                        }}}
                    })),
                    // The page controls a served query publishes. The release
                    // states no domain for the sort, which the paging contract
                    // holds instead.
                    "query" => Some(json!({
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["request_id"],
                            "properties": {
                                "request_id": {"type": "string"},
                                "cursor": {"type": "string"},
                                "limit": {"type": "integer"},
                                "filter": {"type": "object", "properties": {
                                    "code": {"type": "array", "items": {"type": "string"}}
                                }},
                                "sort": {
                                    "type": "object",
                                    "required": ["field", "direction"],
                                    "properties": {
                                        "field": {"type": "string"},
                                        "direction": {"type": "string"}
                                    }
                                }
                            }
                        }
                    })),
                    _ => None,
                },
                terminal_operation: Some(identity),
                direct: true,
                response: ResponseIr::default(),
                replay: (name == "archive").then_some(ReplayIr::State),
            },
        )
    })
    .collect();
    ClientContractIr::from_release_contracts("platform_fixture", &contracts, &routes)
        .expect("platform fixture projects as a release")
}

pub(crate) fn catalog() -> CatalogIr {
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
            Constraint::check(
                "widget_code_check",
                "code = ANY (ARRAY['priority'::text, 'standard'::text])",
            )
            .expect("valid check constraint"),
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

pub(crate) fn manifest() -> Value {
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
                "enum_fields": {"code": ["priority", "standard"]},
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
            // The one bounded-list read. `widget.query` serves a page, and the
            // two result envelopes differ, so the fixture declares both. It
            // also carries the fixture's only `json` members, on both sides,
            // because a json value keeps its own keys.
            "widget.list": {
                "kind": "projection",
                "visibility": "public",
                "permission": "widget.list",
                "connection": "postgres",
                "input": {"fields": [
                    {"path": "request_id", "type": "text", "nullable": false},
                    {"path": "selector", "type": "json", "nullable": false}
                ]},
                "result": {"class": "bounded_list", "fields": [
                    {"path": "id", "type": "uuid", "nullable": false},
                    {"path": "edit_version", "type": "int64", "nullable": false},
                    {"path": "attributes", "type": "json", "nullable": false}
                ]},
                "errors": [
                    "invalid_input", "retry", "timeout", "permission_denied", "internal_error"
                ],
                "constraint_errors": {},
                "relations": [{
                    "schema": "inventory",
                    "table": "widget",
                    "select_fields": ["id", "edit_version"],
                    "insert_fields": [],
                    "update_fields": [],
                    "lock": false,
                    "constraints": []
                }],
                "statements": {"list": {
                    "path": "query/widget_list.sql",
                    "fetch": "bounded_list",
                    "parameters": [],
                    "row": [
                        {"name": "id", "type": "uuid", "nullable": false},
                        {"name": "edit_version", "type": "int64", "nullable": false}
                    ]
                }}
            },
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

use std::collections::BTreeMap;

use serde_json::{Value, json};
use wamn_schema_generator::client_ir::{ClientContractIr, ReplayIr, ResponseIr, RouteIr};
use wamn_schema_generator::{
    AuthoredSql, GeneratedPackage, GenerationInput, GenerationProvenance,
    StatementTransactionality, generate,
};
use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnDefault, ColumnType, Constraint, ForeignKeyAction, ForeignKeyColumn,
    Table,
};

pub(crate) const QUERY_SQL: &[u8] =
    b"SELECT id, code, note, edit_version, created_at FROM widget ORDER BY created_at, id;\n";
pub(crate) const QUERY_DESCENDING_SQL: &[u8] =
    b"SELECT id, code, note, edit_version, created_at FROM widget ORDER BY created_at DESC, id DESC;\n";
/// The bounded-list read. A selector reads it, so it selects the key and the
/// text a person reads beside the revision that `widget.archive` needs.
pub(crate) const LIST_SQL: &[u8] = b"SELECT id, code, edit_version FROM widget ORDER BY id;\n";
/// The second model's list. A selector reads it, so it selects the key and
/// the text a person reads.
pub(crate) const WIDGET_MAKER_LIST_SQL: &[u8] =
    b"SELECT id, name FROM widget_maker ORDER BY name;\n";
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
        AuthoredSql::new("query/widget_maker_list.sql", WIDGET_MAKER_LIST_SQL),
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
        ("widget", "archive"),
        ("widget", "create"),
        ("widget", "delete"),
        ("widget", "get"),
        ("widget", "list"),
        ("widget", "query"),
        ("widget", "record_batch"),
        ("widget", "update"),
        // A selector reads a served list, so the second model's list is
        // published like every other operation.
        ("widget-maker", "list"),
    ]
    .into_iter()
    .map(|(model, name)| {
        // A contract identity spells its operation with hyphens, and a route
        // template keeps the operation path.
        let identity = format!("platform-fixture:{model}/{}@1.0.0", name.replace('_', "-"));
        (
            identity.clone(),
            RouteIr {
                method: "POST".to_owned(),
                template: format!("/{model}/{name}"),
                input_schema: match name {
                    // The shape a real release publishes: the record key, the
                    // revision it expects, the request identity, and the change.
                    "update" => Some(json!({
                        "type": "array",
                        "items": {"type": "object", "required": ["id", "expected_edit_version", "request_id"], "properties": {
                            "id": {"type": "string", "format": "uuid"},
                            "expected_edit_version": {"type": "string"},
                            "request_id": {"type": "string"},
                            "change": {
                            "type": "object", "properties": {"maker_id": {
                                "type": ["string", "null"],
                                "format": "uuid",
                                "x-wamn-explicit-null": "accepted"
                            }, "note": {
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
                    // A command with lines, published the way a release
                    // publishes one. The schema states the group's bounds and
                    // no text, so the group's declared label has to reach the
                    // IR through the declared contract beside it.
                    "record_batch" => Some(json!({
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 100,
                        "items": {
                            "type": "object",
                            "required": ["request_id", "value"],
                            "additionalProperties": false,
                            "properties": {
                                "request_id": {"type": "string", "minLength": 1},
                                "value": {
                                    "type": "object",
                                    "required": ["idempotency_key", "line"],
                                    "additionalProperties": false,
                                    "properties": {
                                        "idempotency_key": {"type": "string", "minLength": 1},
                                        "note": {"type": ["string", "null"]},
                                        "maker_id": {"type": ["string", "null"], "format": "uuid"},
                                        "line": {
                                            "type": "array",
                                            "minItems": 1,
                                            "maxItems": 10,
                                            "items": {
                                                "type": "object",
                                                "required": [
                                                    "purchase_order_line_id",
                                                    "quantity"
                                                ],
                                                "additionalProperties": false,
                                                "properties": {
                                                    "purchase_order_line_id": {
                                                        "type": "string",
                                                        "format": "uuid"
                                                    },
                                                    "quantity": {"type": "string"}
                                                }
                                            }
                                        }
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

/// The index of the widget model, which every emitter test reads.
///
/// The fixture holds two models now, and their order is the contract order,
/// so a test states the model it means instead of the first one.
#[allow(dead_code, reason = "not every test module reads the widget model")]
pub(crate) fn widget_index(ir: &ClientContractIr) -> usize {
    ir.models
        .iter()
        .position(|model| model.name == "widget")
        .expect("the fixture declares the widget model")
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
            // The one column that names another model's record. A selector
            // for it is derived from this foreign key alone.
            Column::new("maker_id", ColumnType::Uuid, true, None, None),
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
            Constraint::foreign_key(
                "widget_maker_id_fkey",
                vec![ForeignKeyColumn::new("maker_id", "id")],
                "inventory",
                "widget_maker",
                ForeignKeyAction::NoAction,
                ForeignKeyAction::NoAction,
            )
            .expect("valid foreign key"),
        ],
        Vec::new(),
    );
    // The second model. It is as small as the first: an identity, one text
    // column, and one list operation that a selector reads. Its name keeps it
    // after `widget` in contract order, so every model index a test states
    // stays where it was.
    let widget_maker = Table::new(
        "inventory",
        "widget_maker",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("name", ColumnType::Text, false, None, None),
        ],
        vec![Constraint::primary_key("widget_maker_pkey", ["id"]).expect("valid primary key")],
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
    CatalogIr::new(vec![widget, widget_maker, command])
}

pub(crate) fn manifest() -> Value {
    json!({
        "package": {"id": "platform_fixture", "version": "1.0.0"},
        "required_platform_policy_contract": {
            "id": "platform_fixture_data_access",
            "state": "unsatisfied"
        },
        "models": {
            "widget_maker": {
                "schema": "inventory",
                "table": "widget_maker",
                "owner": "platform_fixture",
                "server_owned_fields": ["id"],
                "enum_fields": {},
                "audit_log": {"columns": [], "retention": "none"},
                "operations": {}
            },
            "widget": {
                "schema": "inventory",
                "table": "widget",
                "owner": "platform_fixture",
                "server_owned_fields": ["id", "edit_version", "created_at"],
                "enum_fields": {"code": ["priority", "standard"]},
                // A model has no per-field object, so its text is a map keyed
                // by column name. `note` states a label alone, which proves
                // that the two members are independent.
                "field_text": {
                    "code": {
                        "label": "Widget code",
                        "description": "The code an operator types to find one widget."
                    },
                    "note": {"label": "Operator note"}
                },
                "audit_log": {"columns": ["created_at"], "retention": "none"},
                "delete_mode": "hard",
                "operations": {
                    "create": {
                        "permission": "widget.create",
                        "writable_fields": ["code", "maker_id", "note"],
                        "claim": {
                            "table": "widget_command",
                            "identities": {"id": "widget_id"}
                        },
                        "result": "one"
                    },
                    "get": {"permission": "widget.get", "result": "one"},
                    "query": {
                        "permission": "widget.query",
                        "label": "Find widgets",
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
                        "writable_fields": ["code", "maker_id", "note"],
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
                // A selector over widgets reads the code, not the identity.
                "lists": {"model": "widget", "key_field": "id", "display_field": "code"},
                "visibility": "public",
                "permission": "widget.list",
                "connection": "postgres",
                "input": {"fields": [
                    {"path": "request_id", "type": "text", "nullable": false},
                    {"path": "selector", "type": "json", "nullable": false},
                    // The input a narrowed selector fills: list the widgets of
                    // one maker. The plan matches it by the model it names.
                    {
                        "path": "maker_id",
                        "type": "uuid",
                        "nullable": true,
                        "references": {"model": "widget_maker"}
                    }
                ]},
                "result": {"class": "bounded_list", "fields": [
                    {"path": "id", "type": "uuid", "nullable": false},
                    {"path": "code", "type": "text", "nullable": false},
                    {"path": "edit_version", "type": "int64", "nullable": false},
                    {
                        "path": "attributes",
                        "type": "json",
                        "nullable": false,
                        "label": "Attributes",
                        "description": "Every key the widget carries, as the release stored it."
                    }
                ]},
                "errors": [
                    "invalid_input", "retry", "timeout", "permission_denied", "internal_error"
                ],
                "constraint_errors": {},
                "relations": [{
                    "schema": "inventory",
                    "table": "widget",
                    "select_fields": ["code", "id", "edit_version"],
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
                        {"name": "code", "type": "text", "nullable": false},
                        {"name": "edit_version", "type": "int64", "nullable": false}
                    ]
                }}
            },
            // The second model's list, which a selector reads. It declares
            // what it lists and states no display field, so the plan applies
            // the default: the model's first text field.
            "widget_maker.list": {
                "kind": "projection",
                "visibility": "public",
                "permission": "widget_maker.list",
                "connection": "postgres",
                "lists": {"model": "widget_maker", "key_field": "id"},
                "input": {"fields": [
                    {"path": "request_id", "type": "text", "nullable": false}
                ]},
                "result": {"class": "bounded_list", "fields": [
                    {"path": "id", "type": "uuid", "nullable": false},
                    {"path": "name", "type": "text", "nullable": false}
                ]},
                "errors": [
                    "invalid_input", "retry", "timeout", "permission_denied", "internal_error"
                ],
                "constraint_errors": {},
                "relations": [{
                    "schema": "inventory",
                    "table": "widget_maker",
                    "select_fields": ["id", "name"],
                    "insert_fields": [],
                    "update_fields": [],
                    "lock": false,
                    "constraints": []
                }],
                "statements": {"list": {
                    "path": "query/widget_maker_list.sql",
                    "fetch": "bounded_list",
                    "parameters": [],
                    "row": [
                        {"name": "id", "type": "uuid", "nullable": false},
                        {"name": "name", "type": "text", "nullable": false}
                    ]
                }}
            },
            // The one command whose input nests and repeats. A form of this
            // shape is what a refusal path with `[]` names, and the bounds of
            // its group are declared.
            "widget.record_batch": {
                "kind": "command",
                "label": "Record a batch",
                "description": "One submission that records several lines at once.",
                "visibility": "public",
                "permission": "widget.record_batch",
                "connection": "postgres",
                "transaction": "explicit_per_input",
                "automatic_retry": false,
                "idempotent_by": "claim",
                "claim": {
                    "table": "widget_command",
                    "identities": {"widget_id": "widget_id"},
                    "claim": "claim_batch",
                    "replay": "find_batch",
                    "finalize": "finalize_batch"
                },
                "input": {
                    "raw_body_maximum": 1_048_576,
                    "envelope": {"minimum": 1, "maximum": 100},
                    "line": {
                        "minimum": 1,
                        "maximum": 10,
                        "label": "Batch lines",
                        "description": "One line for each widget this batch records."
                    },
                    "fields": [
                        {"path": "request_id", "type": "text", "nullable": false},
                        {"path": "value.idempotency_key", "type": "text", "nullable": false},
                        {
                            "path": "value.note",
                            "type": "text",
                            "nullable": true,
                            "label": "Batch note",
                            "description": "What the operator recorded about this batch."
                        },
                        // The authored reference: this command has no column,
                        // so it declares the record each input names.
                        {
                            "path": "value.maker_id",
                            "type": "uuid",
                            "nullable": true,
                            "label": "Maker",
                            "references": {"model": "widget_maker"}
                        },
                        {
                            "path": "value.line[].purchase_order_line_id",
                            "type": "uuid",
                            "nullable": false,
                            "label": "Line",
                            "references": {
                                "model": "widget",
                                "narrowed_by": "value.maker_id"
                            }
                        },
                        {
                            "path": "value.line[].quantity",
                            "type": "numeric",
                            "nullable": false,
                            "label": "Quantity received"
                        }
                    ]
                },
                // A line input must declare a canonical line profile. The
                // closed vocabulary admits one literal, and that profile
                // requires these two member names, so the fixture carries a
                // Receiving column name. wamn-cguw owns widening it.
                "canonicalization": {
                    "excluded_fields": ["request_id", "value.idempotency_key"],
                    "line_order": "purchase_order_line_id_ascending"
                },
                "result": {"class": "one", "fields": [
                    {"path": "widget_id", "type": "uuid", "nullable": false}
                ]},
                "errors": [
                    "invalid_input", "idempotency_conflict", "retry", "timeout",
                    "permission_denied", "internal_error"
                ],
                "error_details": {"idempotency_conflict": {"required": ["field"]}},
                "constraint_errors": {},
                "relations": [{
                    "schema": "inventory",
                    "table": "widget_command",
                    "select_fields": ["canonical_command", "idempotency_key", "widget_id"],
                    "insert_fields": ["canonical_command", "idempotency_key"],
                    "update_fields": [],
                    "lock": false,
                    "constraints": ["widget_command_pkey"]
                }],
                "statements": {
                    "claim_batch": {
                        "path": "command/widget/claim.sql",
                        "fetch": "optional_one",
                        "parameters": [
                            {"name": "canonical_command", "type": "bytes", "nullable": false},
                            {"name": "idempotency_key", "type": "text", "nullable": false}
                        ],
                        "row": [{"name": "widget_id", "type": "uuid", "nullable": false}]
                    },
                    "find_batch": {
                        "path": "command/widget/replay.sql",
                        "fetch": "optional_one",
                        "parameters": [
                            {"name": "idempotency_key", "type": "text", "nullable": false}
                        ],
                        "row": [
                            {"name": "canonical_command", "type": "bytes", "nullable": false},
                            {"name": "widget_id", "type": "uuid", "nullable": false}
                        ]
                    },
                    "finalize_batch": {
                        "path": "command/widget/finalize.sql",
                        "fetch": "one",
                        "parameters": [
                            {"name": "idempotency_key", "type": "text", "nullable": false}
                        ],
                        "row": [{"name": "widget_id", "type": "uuid", "nullable": false}]
                    }
                }
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

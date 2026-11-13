use serde_json::{Value, json};
use wamn_compiler::manifest::PackageManifest;
use wamn_compiler::{
    AuthoredSql, GeneratedPackage, GenerationInput, StatementTransactionality, generate,
};
use wamn_scalar::SqlValueType;
use wamn_schema_introspection::ir::{CatalogIr, Column, ColumnDefault, Constraint, Table};

pub(super) const QUERY_SOURCES: [AuthoredSql<'static>; 6] = [
    AuthoredSql::new(
        "query/open_gadget_by_part_code_ascending.sql",
        b"SELECT $3, $4, $5\nWHERE ($1::jsonb IS NULL OR stock_id::text IN (SELECT jsonb_array_elements_text($1::jsonb)))\n    AND ($2::jsonb IS NULL OR status IN (SELECT jsonb_array_elements_text($2::jsonb)))\n/* part_code ascending */;\n",
    ),
    AuthoredSql::new(
        "query/open_gadget_by_part_code_descending.sql",
        b"SELECT $3, $4, $5\nWHERE ($1::jsonb IS NULL OR stock_id::text IN (SELECT jsonb_array_elements_text($1::jsonb)))\n    AND ($2::jsonb IS NULL OR status IN (SELECT jsonb_array_elements_text($2::jsonb)))\n/* part_code descending */;\n",
    ),
    AuthoredSql::new(
        "query/open_gadget_by_status_ascending.sql",
        b"SELECT $3, $4, $5\nWHERE ($1::jsonb IS NULL OR stock_id::text IN (SELECT jsonb_array_elements_text($1::jsonb)))\n    AND ($2::jsonb IS NULL OR status IN (SELECT jsonb_array_elements_text($2::jsonb)))\n/* status ascending */;\n",
    ),
    AuthoredSql::new(
        "query/open_gadget_by_status_descending.sql",
        b"SELECT $3, $4, $5\nWHERE ($1::jsonb IS NULL OR stock_id::text IN (SELECT jsonb_array_elements_text($1::jsonb)))\n    AND ($2::jsonb IS NULL OR status IN (SELECT jsonb_array_elements_text($2::jsonb)))\n/* status descending */;\n",
    ),
    AuthoredSql::new(
        "query/open_gadget.sql",
        b"SELECT $3, $4, $5\nWHERE ($1::jsonb IS NULL OR stock_id::text IN (SELECT jsonb_array_elements_text($1::jsonb)))\n    AND ($2::jsonb IS NULL OR status IN (SELECT jsonb_array_elements_text($2::jsonb)))\n/* created_at ascending */;\n",
    ),
    AuthoredSql::new(
        "query/open_gadget_by_created_at_descending.sql",
        b"SELECT $3, $4, $5\nWHERE ($1::jsonb IS NULL OR stock_id::text IN (SELECT jsonb_array_elements_text($1::jsonb)))\n    AND ($2::jsonb IS NULL OR status IN (SELECT jsonb_array_elements_text($2::jsonb)))\n/* created_at descending */;\n",
    ),
];

pub(super) fn catalog(add_unused_table: bool) -> CatalogIr {
    let gadget = Table::new(
        "inventory",
        "gadget",
        vec![
            Column::new(
                "id",
                SqlValueType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("part_code", SqlValueType::Text, false, None, None),
            Column::new("stock_id", SqlValueType::Uuid, false, None, None),
            Column::new(
                "status",
                SqlValueType::Text,
                false,
                Some(ColumnDefault::text("open")),
                None,
            ),
            Column::new(
                "row_version",
                SqlValueType::Int64,
                false,
                Some(ColumnDefault::int64(1)),
                None,
            ),
            Column::new(
                "created_at",
                SqlValueType::Timestamptz,
                false,
                Some(ColumnDefault::CurrentTimestamp),
                None,
            ),
        ],
        vec![
            Constraint::primary_key("gadget_id_pkey", ["id"]).unwrap(),
            Constraint::unique("gadget_part_code_key", ["part_code"]).unwrap(),
            Constraint::check(
                "gadget_status_check",
                "status = ANY (ARRAY['open'::text, 'complete'::text, 'cancelled'::text])",
            )
            .unwrap(),
        ],
        Vec::new(),
    );
    let mut tables = vec![gadget];
    if add_unused_table {
        tables.push(Table::new(
            "inventory",
            "unused",
            vec![Column::new("id", SqlValueType::Uuid, false, None, None)],
            vec![Constraint::primary_key("unused_id_pkey", ["id"]).unwrap()],
            Vec::new(),
        ));
    }
    CatalogIr::new(tables)
}

pub(super) fn projection_operation() -> Value {
    json!({
        "type": "projection",
        "visibility": "public",
        "permission": "quality.load_gadget_part",
        "connection": "postgres",
        "input": {
            "fields": [
                {"path": "gadget_id", "type": "uuid", "nullable": false}
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

        "statements": {
            "load_gadget_part": {
                "path": "query/quality_gadget_part.sql",
                "fetch": "optional_one",
                "parameters": [
                    {"name": "gadget_id", "type": "uuid", "nullable": false}
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
        .find(|table| table.schema() == "inventory" && table.name() == name)
        .unwrap()
}

pub(super) fn rebuilt_table(
    table: &Table,
    columns: Vec<Column>,
    constraints: Vec<Constraint>,
) -> Table {
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
        "package": {"id": "platform_gadget", "version": "1.0.0"},
        "required_platform_policy_contract": {
            "id": "gadget_data_access",
            "state": "unsatisfied"
        },
        "models": {
            "gadget": {
                "schema": "inventory",
                "table": "gadget",
                "owner": "platform_gadget",
                "server_owned_fields": [
                    "part_code", "status", "row_version"
                ],
                "enum_fields": {"status": ["open", "complete", "cancelled"]},
                "audit_log": {"columns": ["created_at"], "retention": "none"},
                "operations": {
                    "get": {
                        "permission": "gadget.get",

                        "result": "one"
                    },
                    "query": {
                        "feed": false,
                        "permission": "gadget.query",

                        "authored_sql": {
                            "default": "query/open_gadget.sql",
                            "variants": [
                                {"field": "part_code", "direction": "ascending", "path": "query/open_gadget_by_part_code_ascending.sql"},
                                {"field": "part_code", "direction": "descending", "path": "query/open_gadget_by_part_code_descending.sql"},
                                {"field": "status", "direction": "ascending", "path": "query/open_gadget_by_status_ascending.sql"},
                                {"field": "status", "direction": "descending", "path": "query/open_gadget_by_status_descending.sql"},
                                {"field": "created_at", "direction": "ascending", "path": "query/open_gadget.sql"},
                                {"field": "created_at", "direction": "descending", "path": "query/open_gadget_by_created_at_descending.sql"}
                            ]
                        },
                        "filters": [
                            {"field": "stock_id"},
                            {"field": "status"}
                        ],
                        "sort": {
                            "fields": ["part_code", "status", "created_at"],
                            "directions": ["ascending", "descending"]
                        },
                        "pagination": {
                            "default_sort": {"field": "created_at", "direction": "ascending"},
                            "tie_breaker": {"field": "id"}
                        },
                        "limit": {
                            "default": 100,
                            "minimum": 1,
                            "maximum": 100
                        },
                        "result": "page"
                    },
                    "update": {
                        "permission": "gadget.update",

                        "writable_fields": ["stock_id"],
                        "revision_field": "row_version",
                        "result": "one"
                    }
                }
            }
        },
        "connections": {},
        "components": {
            "inventory": {
                "connections": ["postgres"]
            }
        }
    })
}

pub(super) fn run(
    catalog: &CatalogIr,
    value: &Value,
    sources: &[AuthoredSql<'_>],
) -> Result<GeneratedPackage, wamn_compiler::ManifestError> {
    run_with_bases(catalog, value, sources, &[manifest()])
}

pub(super) fn run_with_bases(
    catalog: &CatalogIr,
    manifest: &Value,
    sources: &[AuthoredSql<'_>],
    bases: &[Value],
) -> Result<GeneratedPackage, wamn_compiler::ManifestError> {
    let bases = bases
        .iter()
        .map(|base| PackageManifest::from_slice(&serde_json::to_vec(base).unwrap()).unwrap())
        .collect::<Vec<_>>();
    let bytes = wamn_compiler::derive_manifest(
        catalog,
        &serde_json::to_vec(manifest).unwrap(),
        sources,
        &bases,
    )?;
    generate(&GenerationInput::new(
        catalog,
        &bytes,
        sources,
        &StatementTransactionality::from_paths(std::collections::BTreeMap::new()),
    ))
}

pub(super) fn description_json(package: &GeneratedPackage, local: &str, part: &str) -> Value {
    serde_json::to_value(&package.operations()[local]).unwrap()[part].clone()
}

pub(super) fn published_input(package: &GeneratedPackage, local: &str) -> Value {
    let identity = &package.operations()[local].operation.operation;
    let attachments: Value = serde_json::from_slice(
        package
            .file(wamn_compiler::publication::ATTACHMENTS)
            .unwrap()
            .bytes(),
    )
    .unwrap();
    attachments
        .as_object()
        .unwrap()
        .values()
        .find(|attachment| attachment["registered-operation"] == *identity)
        .unwrap()["definition"]["input-schema"]
        .clone()
}

pub(super) fn parsed_manifest(value: &Value) -> PackageManifest {
    PackageManifest::from_slice(&serde_json::to_vec(value).expect("serialize manifest"))
        .expect("parse strict manifest")
}

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

/// The fixture's authored SQL, as the fixture application holds it.
///
/// The generator crate states no SQL of its own: `apps/platform_fixture` is
/// the one place the fixture's query and command files live, and
/// `wamn-fixture-package` reads them.
pub(crate) fn authored_sql() -> Vec<(String, Vec<u8>)> {
    wamn_fixture_package::authored_sql()
}

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
    try_generate_with_sql(catalog, value, &authored_sql())
}

/// Generate the fixture from authored SQL that a test supplies.
#[allow(dead_code, reason = "not every test module replaces authored SQL")]
pub(crate) fn try_generate_with_sql(
    catalog: &CatalogIr,
    value: &Value,
    authored: &[(String, Vec<u8>)],
) -> Result<GeneratedPackage, wamn_schema_generator::GenerateError> {
    let manifest = serde_json::to_vec(value).expect("serialize platform fixture manifest");
    let sources = authored
        .iter()
        .map(|(path, bytes)| AuthoredSql::new(path, bytes))
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
        // A selector reads a served list, so the second model's reads are
        // published like every other operation.
        ("widget-maker", "list"),
        ("widget-maker", "query"),
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
                    // holds instead. Each model publishes the filter it
                    // declares, so the two queries differ by one property.
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
                                    declared_filter(model): {
                                        "type": "array",
                                        "items": {"type": "string"}
                                    }
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
                                    "required": ["idempotency_key", "expected_edit_version", "line"],
                                    "additionalProperties": false,
                                    "properties": {
                                        "idempotency_key": {"type": "string", "minLength": 1},
                                        "note": {"type": ["string", "null"]},
                                        "maker_id": {"type": ["string", "null"], "format": "uuid"},
                                        "expected_edit_version": {"type": "string"},
                                        "line": {
                                            "type": "array",
                                            "minItems": 1,
                                            "maxItems": 10,
                                            "items": {
                                                "type": "object",
                                                "required": ["widget_id", "amount"],
                                                "additionalProperties": false,
                                                "properties": {
                                                    "widget_id": {
                                                        "type": "string",
                                                        "format": "uuid"
                                                    },
                                                    "amount": {"type": "string"}
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

/// The filter each model's query declares, as its route publishes it.
///
/// A selector searches by the filter on its display field, so the two models
/// differ here: the widget filters by its code, and the maker by its name.
fn declared_filter(model: &str) -> &'static str {
    if model == "widget" { "code" } else { "name" }
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
            Constraint::primary_key("widget_id_pkey", ["id"]).expect("valid primary key"),
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
    // column, and the timestamp its query sorts by. Its name keeps it after
    // `widget` in contract order, so every model index a test states stays
    // where it was.
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
            Column::new(
                "created_at",
                ColumnType::Timestamptz,
                false,
                Some(ColumnDefault::CurrentTimestamp),
                None,
            ),
        ],
        vec![Constraint::primary_key("widget_maker_id_pkey", ["id"]).expect("valid primary key")],
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
            Constraint::primary_key("widget_command_idempotency_key_pkey", ["idempotency_key"])
                .expect("valid primary key"),
            Constraint::unique("widget_command_widget_id_key", ["widget_id"])
                .expect("valid unique constraint"),
        ],
        Vec::new(),
    );
    // The third model logs its changes and carries no stamp column. Its one
    // operation, an update, gives the App role a write that history records.
    let widget_tag = Table::new(
        "inventory",
        "widget_tag",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("label", ColumnType::Text, false, None, None),
            Column::new(
                "edit_version",
                ColumnType::Int64,
                false,
                Some(ColumnDefault::int64(1)),
                None,
            ),
        ],
        vec![Constraint::primary_key("widget_tag_id_pkey", ["id"]).expect("valid primary key")],
        Vec::new(),
    );
    CatalogIr::new(vec![widget, widget_maker, widget_tag, command])
}

/// The fixture manifest, as the fixture application authors it.
///
/// `apps/platform_fixture/wamn.json` is the one authority. A test that needs
/// a different shape changes this value on its own copy.
///
/// # Panics
/// Panics when the fixture application's manifest is not valid JSON.
pub(crate) fn manifest() -> Value {
    serde_json::from_slice(&wamn_fixture_package::manifest_bytes())
        .expect("the fixture application manifest is JSON")
}

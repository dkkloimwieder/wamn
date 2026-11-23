use wamn_package::schema::ForeignKeyColumn;
use wamn_scalar::SqlValueType;

use serde_json::Value;
use wamn_codegen::{
    AuthoredSql, GeneratedPackage, GenerationInput, StatementTransactionality, generate,
};
use wamn_schema::ir::{
    CatalogIr, Column, ColumnDefault, Constraint, ForeignKeyAction, Index, IndexColumn,
    IndexDirection, Table, View, ViewBaseTable, ViewColumn, ViewColumnOrigin,
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
) -> Result<GeneratedPackage, wamn_package::manifest::ManifestError> {
    try_generate_with_sql(catalog, value, &authored_sql())
}

/// Generate the fixture from authored SQL that a test supplies.
#[allow(dead_code, reason = "not every test module replaces authored SQL")]
pub(crate) fn try_generate_with_sql(
    catalog: &CatalogIr,
    value: &Value,
    authored: &[(String, Vec<u8>)],
) -> Result<GeneratedPackage, wamn_package::manifest::ManifestError> {
    let manifest = serde_json::to_vec(value).expect("serialize platform fixture manifest");
    let parsed = wamn_component_policy::PackageManifest::from_slice(&manifest)?;
    let paths: std::collections::BTreeSet<_> = parsed
        .models
        .values()
        .flat_map(|model| model.operations.values())
        .filter_map(|operation| operation.authored_sql.as_ref())
        .flat_map(|query| {
            std::iter::once(query.default.as_str())
                .chain(query.variants.iter().map(|variant| variant.path.as_str()))
        })
        .chain(
            parsed
                .custom_operations
                .values()
                .flat_map(|operation| operation.statements.values())
                .map(|statement| statement.path.as_str()),
        )
        .collect();
    let sources = authored
        .iter()
        .map(|(path, bytes)| AuthoredSql::new(path, bytes))
        .filter(|source| paths.contains(source.path()))
        .collect::<Vec<_>>();
    generate(&GenerationInput::new(
        catalog,
        &manifest,
        &sources,
        &StatementTransactionality::from_paths(std::collections::BTreeMap::new()),
    ))
}

pub(crate) fn catalog() -> CatalogIr {
    catalog_of(true, true)
}

/// The fixture catalog with `widget.note` NOT NULL: a schema change that
/// moves the generated contracts and edits no manifest.
#[allow(dead_code, reason = "not every test module changes the widget table")]
pub(crate) fn catalog_with_required_note() -> CatalogIr {
    catalog_of(false, true)
}

/// The fixture catalog with no foreign key from `widget.maker_id`, so no
/// column names another model's record.
#[allow(dead_code, reason = "not every test module drops the maker key")]
pub(crate) fn catalog_without_maker_key() -> CatalogIr {
    catalog_of(true, false)
}

fn catalog_of(note_nullable: bool, maker_key: bool) -> CatalogIr {
    let widget = Table::new(
        "inventory",
        "widget",
        vec![
            Column::new(
                "id",
                SqlValueType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("code", SqlValueType::Text, false, None, None),
            Column::new("note", SqlValueType::Text, note_nullable, None, None),
            // The one column that names another model's record. A selector
            // for it is derived from this foreign key alone.
            Column::new("maker_id", SqlValueType::Uuid, true, None, None),
            Column::new(
                "edit_version",
                SqlValueType::Int32,
                false,
                Some(ColumnDefault::int32(1)),
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
            Constraint::primary_key("widget_id_pkey", ["id"]).expect("valid primary key"),
            Constraint::unique("widget_code_key", ["code"]).expect("valid unique constraint"),
            Constraint::check(
                "widget_code_check",
                "code = ANY (ARRAY['priority'::text, 'standard'::text])",
            )
            .expect("valid check constraint"),
        ]
        .into_iter()
        .chain(maker_key.then(|| {
            Constraint::foreign_key(
                "widget_maker_id_fkey",
                vec![ForeignKeyColumn::new("maker_id", "id")],
                "inventory",
                "widget_maker",
                ForeignKeyAction::NoAction,
                ForeignKeyAction::NoAction,
            )
            .expect("valid foreign key")
        }))
        .collect(),
        Vec::new(),
    );
    // The second model. It is as small as the first: an identity, one text
    // column, a revision, and the timestamp its query sorts by. The batch
    // names it twice, so its revision names the inspector input (wamn-nv87,
    // wamn-kyak). Its name keeps it after
    // `widget` in contract order, so every model index a test states stays
    // where it was.
    let widget_maker = Table::new(
        "inventory",
        "widget_maker",
        vec![
            Column::new(
                "id",
                SqlValueType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("name", SqlValueType::Text, false, None, None),
            Column::new(
                "edit_version",
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
        vec![Constraint::primary_key("widget_maker_id_pkey", ["id"]).expect("valid primary key")],
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
                SqlValueType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("label", SqlValueType::Text, false, None, None),
            Column::new(
                "edit_version",
                SqlValueType::Int64,
                false,
                Some(ColumnDefault::int64(1)),
                None,
            ),
        ],
        vec![Constraint::primary_key("widget_tag_id_pkey", ["id"]).expect("valid primary key")],
        Vec::new(),
    );
    let numbered_label = Table::new(
        "inventory",
        "numbered_label",
        vec![
            Column::new(
                "id",
                SqlValueType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("number", SqlValueType::Text, false, None, None),
            Column::new("stock_id", SqlValueType::Text, false, None, None),
            Column::new(
                "edit_version",
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
            Column::new("created_by", SqlValueType::Uuid, false, None, None),
            Column::new(
                "updated_at",
                SqlValueType::Timestamptz,
                false,
                Some(ColumnDefault::CurrentTimestamp),
                None,
            ),
            Column::new("updated_by", SqlValueType::Uuid, false, None, None),
        ],
        vec![
            Constraint::primary_key("numbered_label_id_pkey", ["id"]).unwrap(),
            Constraint::unique("numbered_label_number_key", ["number"]).unwrap(),
        ],
        Vec::new(),
    );
    let counter = Table::new(
        "inventory",
        "counter",
        vec![
            Column::new(
                "id",
                SqlValueType::Int64,
                false,
                None,
                Some(wamn_schema::ir::ColumnGeneration::Identity {
                    mode: wamn_schema::ir::IdentityMode::Always,
                }),
            ),
            Column::new("label", SqlValueType::Text, false, None, None),
            Column::new(
                "edit_version",
                SqlValueType::Int32,
                false,
                Some(ColumnDefault::int32(1)),
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
        vec![Constraint::primary_key("counter_id_pkey", ["id"]).expect("valid primary key")],
        Vec::new(),
    );
    let mut tables = vec![widget, widget_maker, widget_tag, numbered_label, counter];
    for name in ["feed_order", "feed_item"] {
        tables.push(Table::new(
            "inventory",
            name,
            vec![
                Column::new("id", SqlValueType::Uuid, false, None, None),
                Column::new("label", SqlValueType::Text, false, None, None),
            ],
            vec![Constraint::primary_key(format!("{name}_id_pkey"), ["id"]).unwrap()],
            Vec::new(),
        ));
    }
    tables.push(Table::new(
        "inventory",
        "feed_line",
        vec![
            Column::new("id", SqlValueType::Uuid, false, None, None),
            Column::new("order_id", SqlValueType::Uuid, true, None, None),
            Column::new("item_id", SqlValueType::Uuid, true, None, None),
            Column::new("label", SqlValueType::Text, false, None, None),
            Column::new(
                "enabled",
                SqlValueType::Boolean,
                false,
                Some(ColumnDefault::boolean(true)),
                None,
            ),
        ],
        vec![
            Constraint::primary_key("feed_line_id_pkey", ["id"]).unwrap(),
            Constraint::foreign_key(
                "feed_line_order_id_fkey",
                vec![ForeignKeyColumn::new("order_id", "id")],
                "inventory",
                "feed_order",
                ForeignKeyAction::NoAction,
                ForeignKeyAction::SetNull,
            )
            .unwrap(),
            Constraint::foreign_key(
                "feed_line_item_id_fkey",
                vec![ForeignKeyColumn::new("item_id", "id")],
                "inventory",
                "feed_item",
                ForeignKeyAction::NoAction,
                ForeignKeyAction::SetNull,
            )
            .unwrap(),
        ],
        ["order_id", "item_id"]
            .into_iter()
            .map(|field| {
                Index::new(
                    format!("feed_line_{field}_idx"),
                    vec![IndexColumn::new(field, IndexDirection::Asc)],
                )
                .unwrap()
            })
            .collect(),
    ));
    let columns = [
        ("id", "feed_line", "id", SqlValueType::Uuid),
        ("order_id", "feed_line", "order_id", SqlValueType::Uuid),
        ("item_id", "feed_line", "item_id", SqlValueType::Uuid),
        ("label", "feed_line", "label", SqlValueType::Text),
        ("enabled", "feed_line", "enabled", SqlValueType::Boolean),
        ("order_label", "feed_order", "label", SqlValueType::Text),
        ("item_label", "feed_item", "label", SqlValueType::Text),
    ]
    .into_iter()
    .map(|(name, table, field, ty)| {
        ViewColumn::new(name, ty).with_origin(ViewColumnOrigin {
            schema: "inventory".into(),
            table: table.into(),
            column: field.into(),
        })
    })
    .collect();
    CatalogIr::new(tables).with_views(vec![View::new("inventory", "feed_screen", columns, ["feed_line", "feed_order", "feed_item"].into_iter().map(|name| ViewBaseTable::new("inventory", name)).collect()).with_definition("SELECT line.id, line.order_id, line.item_id, line.label, line.enabled, orders.label AS order_label, item.label AS item_label FROM inventory.feed_line AS line JOIN inventory.feed_order AS orders ON orders.id = line.order_id JOIN inventory.feed_item AS item ON item.id = line.item_id".into(), false)])
}

/// The name of [`unread_table`].
#[allow(
    dead_code,
    reason = "not every test module needs a relation no operation reads"
)]
pub(crate) const UNREAD_TABLE: &str = "widget_ledger";

/// A relation in the fixture schema that no fixture operation reads. A test
/// declares it as an internal relation or an ownership-only model.
#[allow(
    dead_code,
    reason = "not every test module needs a relation no operation reads"
)]
pub(crate) fn unread_table() -> Table {
    Table::new(
        "inventory",
        UNREAD_TABLE,
        vec![
            Column::new(
                "entry_id",
                SqlValueType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("note", SqlValueType::Text, false, None, None),
        ],
        vec![
            Constraint::primary_key("widget_ledger_entry_id_pkey", ["entry_id"])
                .expect("valid primary key"),
        ],
        Vec::new(),
    )
}

/// The fixture catalog with [`unread_table`].
#[allow(
    dead_code,
    reason = "not every test module needs a relation no operation reads"
)]
pub(crate) fn catalog_with_unread_table() -> CatalogIr {
    let mut tables = catalog().tables().to_vec();
    tables.push(unread_table());
    CatalogIr::new(tables).with_views(catalog().views().to_vec())
}

/// The fixture manifest, as the fixture application authors it.
///
/// `apps/platform_fixture/wamn.k` is the one authority, compiled to
/// `apps/target/wamn/platform_fixture/wamn.json`. A test that needs
/// a different shape changes this value on its own copy.
///
/// # Panics
/// Panics when the compiled manifest is missing, and names the command that
/// builds it, or when the manifest is not valid JSON.
pub(crate) fn manifest() -> Value {
    let path = wamn_fixture_package::manifest_path();
    let bytes = std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "the test input {} is missing ({error}); build it with `cargo xtask prepare-tests`",
            path.display()
        )
    });
    serde_json::from_slice(&bytes).expect("the fixture application manifest is JSON")
}

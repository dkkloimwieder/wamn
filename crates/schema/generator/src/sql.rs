use wamn_schema_introspection::ir::{Column, ColumnType, Table};

use wamn_record_history::history_table_name;

use crate::manifest::{DeleteMode, FilterMatch, ModelDeclaration};
use crate::{CursorDirection, OperationDeclaration};

// SQL outcomes are shared with the generated result contracts.
pub(crate) const OUTCOME_NOT_FOUND: &str = "not_found";
pub(crate) const OUTCOME_CONCURRENCY_CONFLICT: &str = "concurrency_conflict";
pub(crate) const OUTCOME_UPDATED: &str = "updated";
pub(crate) const OUTCOME_DELETED: &str = "deleted";

/// The tombstone marker column.
///
/// It is set once by the tombstone delete and never cleared, so one predicate
/// hides the row from every read, every update, and a second delete.
const TOMBSTONE_MARKER: &str = "deleted_at";

/// The same predicate where the statement carries the `model` alias.
const LIVE_ROW: &str = "model.deleted_at IS NULL";

/// The tombstone marker, as the delete statement writes it.
///
/// The actor reads the same setting that `wamn_history.stamp_row` reads. That
/// trigger fires on this UPDATE and refuses an unbound actor first, so an
/// actorless tombstone never reaches the row.
const TOMBSTONE_ASSIGNMENT: &str = "        deleted_at = transaction_timestamp(),\n        deleted_by = NULLIF(current_setting('app.user_id', true), '')::uuid";

pub(crate) fn get(table: &Table, tombstoned: bool) -> String {
    let key_cast = key_cast(table);
    let live = if tombstoned {
        format!("\n  AND {LIVE_ROW}")
    } else {
        String::new()
    };
    format!(
        "SELECT\n    {}\nFROM {} AS model\nWHERE model.id = $1::{key_cast}{live};\n",
        select_columns(table),
        table.name()
    )
}

/// Insert the row and return it, including any database-generated key.
///
/// A numbered field binds last, as the text `next-number` returned, and the
/// SQL casts it to the column. An actor field reads the `app.user_id` claim,
/// never a parameter.
pub(crate) fn create(
    table: &Table,
    model: &ModelDeclaration,
    operation: &OperationDeclaration,
) -> String {
    let mut columns = std::iter::once("id")
        .filter(|_| !crate::key::database_generated(table))
        .chain(operation.writable_fields.iter().map(String::as_str))
        .collect::<Vec<_>>();
    let mut values = columns
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let ty = column_type(table, field);
            format!("${}::{}", index + 1, postgres_type(ty))
        })
        .collect::<Vec<_>>();
    if let Some((field, _)) = model.number_field() {
        let bind = format!("${}::text", values.len() + 1);
        values.push(match column_type(table, field) {
            ColumnType::Text => bind,
            ty => format!("{bind}::{}", postgres_type(ty)),
        });
        columns.push(field);
    }
    for field in model.actor_fields() {
        values.push(format!(
            "current_setting('app.user_id')::{}",
            postgres_type(column_type(table, field))
        ));
        columns.push(field);
    }
    if columns.is_empty() {
        return format!(
            "INSERT INTO {} DEFAULT VALUES\nRETURNING\n    {};\n",
            table.name(),
            returning_columns(table)
        );
    }
    format!(
        "INSERT INTO {} ({})\nVALUES ({})\nRETURNING\n    {};\n",
        table.name(),
        columns.join(", "),
        values.join(", "),
        returning_columns(table)
    )
}

/// Update the present fields when the revision matches.
///
/// A change also fills the model's `updated_by` actor field from the
/// `app.user_id` claim; a true no-op leaves it, as it leaves the revision.
pub(crate) fn update(
    table: &Table,
    model: &ModelDeclaration,
    operation: &OperationDeclaration,
    tombstoned: bool,
) -> String {
    let revision = operation
        .revision_field
        .as_deref()
        .expect("update validation requires revision");
    let key_cast = key_cast(table);
    let revision_cast = postgres_type(column_type(table, revision));
    // A tombstoned row has no target, so an update of one reads as not_found.
    let live = if tombstoned {
        format!("\n      AND {TOMBSTONE_MARKER} IS NULL")
    } else {
        String::new()
    };
    // Each writable field binds a presence flag, then its value.
    let binds = operation
        .writable_fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let value_bind = 4 + index * 2;
            (
                field,
                value_bind - 1,
                format!(
                    "${value_bind}::{}",
                    postgres_type(column_type(table, field))
                ),
            )
        })
        .collect::<Vec<_>>();
    // A true no-op keeps its revision, so the stamp trigger sees no change.
    // The comparison uses text, so a numeric scale-only change is a change.
    let changed = binds
        .iter()
        .map(|(field, present, value)| {
            format!("(${present}::boolean AND {value}::text IS DISTINCT FROM model.{field}::text)")
        })
        .collect::<Vec<_>>()
        .join("\n            OR ");
    let assignments = binds
        .iter()
        .map(|(field, present, value)| {
            format!("        {field} = CASE WHEN ${present}::boolean THEN {value} ELSE model.{field} END")
        })
        .chain(model.update_actor_field().map(|field| {
            format!(
                "        {field} = CASE\n            WHEN {changed}\n            THEN current_setting('app.user_id')::{}\n            ELSE model.{field}\n        END",
                postgres_type(column_type(table, field))
            )
        }))
        .chain(std::iter::once(format!(
            "        {revision} = CASE\n            WHEN {changed}\n            THEN model.{revision} + 1\n            ELSE model.{revision}\n        END"
        )))
        .collect::<Vec<_>>()
        .join(",\n");
    let returning = select_columns(table);
    let returned = table
        .columns()
        .iter()
        .map(|column| format!("updated.{}", column.name()))
        .collect::<Vec<_>>()
        .join(",\n    ");
    format!(
        "WITH target AS MATERIALIZED (\n    SELECT id, {revision}\n    FROM {}\n    WHERE id = $1::{key_cast}{live}\n    FOR UPDATE\n),\nupdated AS (\n    UPDATE {} AS model\n    SET\n{assignments}\n    FROM target\n    WHERE model.id = target.id\n      AND target.{revision} = $2::{revision_cast}\n    RETURNING\n    {returning}\n)\nSELECT\n    CASE\n        WHEN NOT EXISTS (SELECT 1 FROM target) THEN '{OUTCOME_NOT_FOUND}'\n        WHEN NOT EXISTS (SELECT 1 FROM updated) THEN '{OUTCOME_CONCURRENCY_CONFLICT}'\n        ELSE '{OUTCOME_UPDATED}'\n    END AS outcome,\n    (SELECT target.{revision} FROM target) AS observed_{revision},\n    {returned}\nFROM (SELECT 1) AS singleton\nLEFT JOIN updated ON TRUE;\n",
        table.name(),
        table.name()
    )
}

/// The delete statement of the declared mode.
///
/// A hard delete removes the row, and an inbound foreign key can refuse it. A
/// tombstone sets the marker instead, so it removes nothing and violates
/// nothing. Both report the same three outcomes, and the revision the row
/// carried, as update does, so a conflict reports it without a second read.
pub(crate) fn delete(table: &Table, operation: &OperationDeclaration, mode: DeleteMode) -> String {
    let revision = operation
        .revision_field
        .as_deref()
        .expect("delete validation requires revision");
    let key_cast = key_cast(table);
    let revision_cast = postgres_type(column_type(table, revision));
    let (live, removal) = match mode {
        DeleteMode::Hard => (
            String::new(),
            format!(
                "    DELETE FROM {} AS model\n    USING target",
                table.name()
            ),
        ),
        DeleteMode::Tombstone => (
            format!("\n      AND {TOMBSTONE_MARKER} IS NULL"),
            format!(
                "    UPDATE {} AS model\n    SET\n{TOMBSTONE_ASSIGNMENT}\n    FROM target",
                table.name()
            ),
        ),
    };
    format!(
        "WITH target AS MATERIALIZED (\n    SELECT id, {revision}\n    FROM {}\n    WHERE id = $1::{key_cast}{live}\n    FOR UPDATE\n),\ndeleted AS (\n{removal}\n    WHERE model.id = target.id\n      AND target.{revision} = $2::{revision_cast}\n    RETURNING model.id\n)\nSELECT CASE\n    WHEN NOT EXISTS (SELECT 1 FROM target) THEN '{OUTCOME_NOT_FOUND}'\n    WHEN NOT EXISTS (SELECT 1 FROM deleted) THEN '{OUTCOME_CONCURRENCY_CONFLICT}'\n    ELSE '{OUTCOME_DELETED}'\nEND AS outcome,\n    (SELECT target.{revision} FROM target) AS observed_{revision};\n",
        table.name()
    )
}

pub(crate) fn query(
    table: &Table,
    operation: &OperationDeclaration,
    sort_field: &str,
    direction: CursorDirection,
    tombstoned: bool,
    feed: bool,
) -> String {
    let key_cast = key_cast(table);
    let (mut predicates, next_bind) = scope_predicates(table, operation, tombstoned);
    let cursor_value_bind = next_bind;
    let cursor_id_bind = cursor_value_bind + 1;
    let limit_bind = cursor_id_bind + 1;
    let sort_type = postgres_type(column_type(table, sort_field));
    let comparison = match direction {
        CursorDirection::Ascending => ">",
        CursorDirection::Descending => "<",
    };
    predicates.push(format!(
        "    (${cursor_value_bind}::{sort_type} IS NULL OR (model.{sort_field}, model.id) {comparison} (${cursor_value_bind}::{sort_type}, ${cursor_id_bind}::{key_cast}))"
    ));
    let page = format!(
        "SELECT\n    {}\nFROM {} AS model\nWHERE\n{}\nORDER BY model.{sort_field} {direction}, model.id {direction}\nLIMIT ${limit_bind}::int8",
        select_columns(table),
        table.name(),
        predicates.join("\n    AND\n"),
        direction = sql_direction(direction),
    );
    if !feed {
        return format!("{page};\n");
    }
    // The page of a list read with a change feed also returns the cursor the
    // feed starts from: the snapshot xmin of this one statement. The outer
    // row carries it when the page has no rows (`docs/plan/wamn-sync.md`).
    format!(
        "SELECT\n    bound.feed_cursor,\n    {}\nFROM (SELECT {SNAPSHOT_XMIN} AS feed_cursor) AS bound\nLEFT JOIN LATERAL (\n{}\n) AS page ON true\nORDER BY page.{sort_field} {direction}, page.id {direction};\n",
        aliased_columns(table, "page"),
        indent(&page),
        direction = sql_direction(direction),
    )
}

/// The snapshot xmin of the statement: every transaction below it has ended.
const SNAPSHOT_XMIN: &str = "pg_snapshot_xmin(pg_current_snapshot())::text::int8";

/// The change feed of a list read (`docs/plan/wamn-sync.md`, "Change feed").
///
/// The statement reads the history entries of the transactions from the
/// cursor transaction, including visible transactions at or above xmin and
/// below xmax, from one snapshot. It collapses entries to one row per id in
/// transaction and id order. Each row carries the live row when it is still
/// in the scope of the list's filters. Otherwise it is a deletion, and it
/// carries the revision the delete entry left. The outer row carries the
/// snapshot xmin, and whether pruning removed a transaction at or above the
/// cursor xmin, even when no entry remains.
///
/// The binds are the list's filters and search, then the cursor transaction,
/// the cursor id and the limit.
pub(crate) fn changes(
    table: &Table,
    operation: &OperationDeclaration,
    revision: &str,
    tombstoned: bool,
) -> String {
    let key_cast = key_cast(table);
    let (predicates, next_bind) = scope_predicates(table, operation, tombstoned);
    let transaction = next_bind;
    let after = transaction + 1;
    let limit = after + 1;
    let history = history_table_name(table.name());
    let present = std::iter::once("model.id IS NOT NULL".to_owned())
        .chain(
            predicates
                .iter()
                .map(|predicate| predicate.trim().to_owned()),
        )
        .collect::<Vec<_>>()
        .join("\n            AND ");
    let expired = format!(
        "EXISTS (SELECT FROM wamn_history.pruning_watermarks AS watermark WHERE watermark.history_table = '{history}'::regclass AND ${transaction}::int8 <= watermark.transaction_id)"
    );
    format!(
        "SELECT
    bound.feed_end,
    bound.expired,
    page.change_id,
    page.change_transaction,
    page.present,
    page.deleted_version,
    {columns}
FROM (
    SELECT
        pg_snapshot_xmin(snapshot)::text::int8 AS feed_end,
        pg_snapshot_xmax(snapshot)::text::int8 AS feed_max,
        snapshot,
        {expired} AS expired
    FROM (SELECT pg_current_snapshot() AS snapshot OFFSET 0) AS current
) AS bound
LEFT JOIN LATERAL (
    SELECT
        changed.change_id,
        changed.change_transaction,
        ({present}) AS present,
        changed.deleted_version,
        {model_columns}
    FROM (
        SELECT
            (entry.row_key->>'id')::{key_cast} AS change_id,
            max(entry.transaction_id) AS change_transaction,
            max((entry.before->>'{revision}')::int4) FILTER (WHERE entry.type = 'delete') AS deleted_version
        FROM {history} AS entry
        WHERE entry.transaction_id >= ${transaction}::int8
          AND (entry.transaction_id < bound.feed_end
               OR (entry.transaction_id < bound.feed_max
                   AND pg_visible_in_snapshot(entry.transaction_id::text::xid8, bound.snapshot)))
        GROUP BY (entry.row_key->>'id')::{key_cast}
    ) AS changed
    LEFT JOIN {table} AS model ON model.id = changed.change_id
    WHERE ${after}::{key_cast} IS NULL
       OR (changed.change_transaction, changed.change_id) > (${transaction}::int8, ${after}::{key_cast})
    ORDER BY changed.change_transaction, changed.change_id
    LIMIT ${limit}::int8
) AS page ON true
ORDER BY page.change_transaction, page.change_id;
",
        columns = aliased_columns(table, "page"),
        model_columns = select_columns(table).replace("\n    ", "\n        "),
        table = table.name(),
    )
}

/// The predicates of a list read's filters and search, in bind order from
/// `$1`, and the next free bind.
fn scope_predicates(
    table: &Table,
    operation: &OperationDeclaration,
    tombstoned: bool,
) -> (Vec<String>, usize) {
    let mut predicates = Vec::new();
    if tombstoned {
        predicates.push(format!("    {LIVE_ROW}"));
    }
    for (index, filter) in operation.filters.iter().enumerate() {
        let field = &filter.field;
        let bind = index + 1;
        predicates.push(match filter.match_mode {
            FilterMatch::Exact => format!(
                "    (${bind}::jsonb IS NULL OR model.{field} IN (\n        SELECT filter.value::{}\n        FROM jsonb_array_elements_text(${bind}::jsonb) AS filter(value)\n    ))",
                postgres_type(column_type(table, field)),
            ),
            // strpos reads the value as text, so a % or _ in it matches itself.
            FilterMatch::Contains => format!(
                "    (${bind}::jsonb IS NULL OR EXISTS (\n        SELECT 1\n        FROM jsonb_array_elements_text(${bind}::jsonb) AS filter(value)\n        WHERE strpos(model.{field}, filter.value) > 0\n    ))"
            ),
            // starts_with reads the value as text too.
            FilterMatch::Prefix => format!(
                "    (${bind}::jsonb IS NULL OR EXISTS (\n        SELECT 1\n        FROM jsonb_array_elements_text(${bind}::jsonb) AS filter(value)\n        WHERE starts_with(model.{field}, filter.value)\n    ))"
            ),
            FilterMatch::Range => {
                let bounds = range_bounds(bind, field, postgres_type(column_type(table, field)));
                match filter.default {
                    // A read without the band reads the default one, as of
                    // the statement.
                    Some(default) => format!(
                        "    (CASE WHEN ${bind}::jsonb IS NULL\n        THEN model.{field} >= now() - make_interval(days => {})\n        ELSE {bounds}\n    END)",
                        default.last_days
                    ),
                    None => format!("    (${bind}::jsonb IS NULL OR {bounds})"),
                }
            }
            FilterMatch::IsNull => format!(
                "    (${bind}::jsonb IS NULL OR (model.{field} IS NULL) = (${bind}::jsonb)::boolean)"
            ),
        });
    }
    // The search, when declared, binds after the filters and before the cursor.
    let mut next_bind = operation.filters.len() + 1;
    if let Some(search) = &operation.search {
        let bind = next_bind;
        next_bind += 1;
        predicates.push(format!(
            "    (${bind}::text IS NULL OR {})",
            search
                .fields
                .iter()
                .map(|field| format!("strpos(lower(model.{field}), lower(${bind}::text)) > 0"))
                .collect::<Vec<_>>()
                .join("\n        OR ")
        ));
    }
    (predicates, next_bind)
}

/// Each statement line indented by four more spaces.
fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The bounds of one range filter: each bound the value states, inclusive.
fn range_bounds(bind: usize, field: &str, ty: &str) -> String {
    format!(
        "(\n        (${bind}::jsonb->>'min' IS NULL OR model.{field} >= (${bind}::jsonb->>'min')::{ty})\n        AND (${bind}::jsonb->>'max' IS NULL OR model.{field} <= (${bind}::jsonb->>'max')::{ty})\n    )"
    )
}

pub(crate) const fn direction_name(direction: CursorDirection) -> &'static str {
    match direction {
        CursorDirection::Ascending => "ascending",
        CursorDirection::Descending => "descending",
    }
}

pub(crate) const fn postgres_type(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::Boolean => "boolean",
        ColumnType::Int32 => "int4",
        ColumnType::Int64 => "int8",
        ColumnType::Float64 => "float8",
        ColumnType::Text => "text",
        ColumnType::Bytes => "bytea",
        ColumnType::Numeric => "numeric",
        ColumnType::Timestamptz => "timestamptz",
        ColumnType::Json => "jsonb",
        ColumnType::Uuid => "uuid",
    }
}

fn key_cast(table: &Table) -> &'static str {
    postgres_type(
        crate::key::key_type(table)
            .expect("validation resolved the model key")
            .column_type(),
    )
}

fn column_type(table: &Table, field: &str) -> ColumnType {
    table
        .columns()
        .iter()
        .find(|column| column.name() == field)
        .expect("validation resolved SQL field")
        .column_type()
}

/// The model columns as an outer statement reads them from `alias`.
fn aliased_columns(table: &Table, alias: &str) -> String {
    table
        .columns()
        .iter()
        .map(|column| format!("{alias}.{}", column.name()))
        .collect::<Vec<_>>()
        .join(",\n    ")
}

fn select_columns(table: &Table) -> String {
    table
        .columns()
        .iter()
        .map(|column| format!("model.{}", column.name()))
        .collect::<Vec<_>>()
        .join(",\n    ")
}

fn returning_columns(table: &Table) -> String {
    table
        .columns()
        .iter()
        .map(Column::name)
        .collect::<Vec<_>>()
        .join(",\n    ")
}

const fn sql_direction(direction: CursorDirection) -> &'static str {
    match direction {
        CursorDirection::Ascending => "ASC",
        CursorDirection::Descending => "DESC",
    }
}

#[cfg(test)]
mod tests {
    use super::{changes, delete, get, query, update};
    use crate::manifest::DeleteMode;
    use crate::{CursorDirection, ModelDeclaration, OperationDeclaration};
    use tokio_postgres::{NoTls, SimpleQueryMessage};
    use wamn_schema_introspection::ir::{Column, ColumnType, Constraint, Table};

    #[test]
    fn key_casts_follow_the_primary_key_type() {
        let operation: OperationDeclaration = serde_json::from_value(serde_json::json!({
            "permission": "write", "result": "one",
            "revision_field": "version", "writable_fields": ["label"]
        }))
        .unwrap();
        let model: ModelDeclaration = serde_json::from_value(serde_json::json!({
            "schema": "public", "table": "key_row", "owner": "fixture", "operations": {}
        }))
        .unwrap();
        for (ty, cast) in [
            (ColumnType::Uuid, "uuid"),
            (ColumnType::Int32, "int4"),
            (ColumnType::Int64, "int8"),
        ] {
            let table = Table::new(
                "public",
                "key_row",
                vec![
                    Column::new(
                        "id",
                        ty,
                        false,
                        None,
                        (ty != ColumnType::Uuid).then_some(
                            wamn_schema_introspection::ir::ColumnGeneration::Identity {
                                mode: wamn_schema_introspection::ir::IdentityMode::Always,
                            },
                        ),
                    ),
                    Column::new("label", ColumnType::Text, false, None, None),
                    Column::new("version", ColumnType::Int64, false, None, None),
                ],
                vec![Constraint::primary_key("key_row_pkey", ["id"]).unwrap()],
                Vec::new(),
            );
            for tombstoned in [false, true] {
                let live = if tombstoned {
                    "\n  AND model.deleted_at IS NULL"
                } else {
                    ""
                };
                assert_eq!(
                    get(&table, tombstoned),
                    format!(
                        "SELECT\n    model.id,\n    model.label,\n    model.version\nFROM key_row AS model\nWHERE model.id = $1::{cast}{live};\n"
                    )
                );
                let live = if tombstoned {
                    "\n      AND deleted_at IS NULL"
                } else {
                    ""
                };
                let target = format!(
                    "WITH target AS MATERIALIZED (\n    SELECT id, version\n    FROM key_row\n    WHERE id = $1::{cast}{live}\n    FOR UPDATE\n),\n"
                );
                let updated = update(&table, &model, &operation, tombstoned);
                assert!(updated.starts_with(&target), "{updated}");
                let mode = if tombstoned {
                    DeleteMode::Tombstone
                } else {
                    DeleteMode::Hard
                };
                let deleted = delete(&table, &operation, mode);
                assert!(deleted.starts_with(&target), "{deleted}");
                if tombstoned {
                    assert!(
                        deleted.contains("NULLIF(current_setting('app.user_id', true), '')::uuid")
                    );
                }
                for (direction, comparison) in [
                    (CursorDirection::Ascending, ">"),
                    (CursorDirection::Descending, "<"),
                ] {
                    for feed in [false, true] {
                        let page = query(&table, &operation, "label", direction, tombstoned, feed);
                        assert!(page.contains(&format!("($1::text IS NULL OR (model.label, model.id) {comparison} ($1::text, $2::{cast}))")), "{page}");
                        // Sorting by the key casts both cursor values to its type.
                        let page = query(&table, &operation, "id", direction, tombstoned, feed);
                        assert!(page.contains(&format!("($1::{cast} IS NULL OR (model.id, model.id) {comparison} ($1::{cast}, $2::{cast}))")), "{page}");
                    }
                }
                let feed = changes(&table, &operation, "version", tombstoned);
                for expected in [
                    format!("(entry.row_key->>'id')::{cast} AS change_id"),
                    format!("GROUP BY (entry.row_key->>'id')::{cast}\n"),
                    format!("WHERE $2::{cast} IS NULL\n"),
                    format!(
                        "(changed.change_transaction, changed.change_id) > ($1::int8, $2::{cast})"
                    ),
                ] {
                    assert!(feed.contains(&expected), "{feed} lacks {expected}");
                }
            }
        }
    }

    #[tokio::test]
    async fn create_returns_database_generated_integer_ids() {
        use wamn_schema_introspection::ir::{ColumnDefault, ColumnGeneration, IdentityMode};

        let database = wamn_test_postgres::database();
        let (client, connection) = tokio_postgres::connect(database.url(), NoTls)
            .await
            .unwrap();
        let connection = tokio::spawn(connection);
        let model: ModelDeclaration = serde_json::from_value(serde_json::json!({
            "schema": "public", "table": "key_row", "owner": "fixture", "operations": {}
        }))
        .unwrap();
        for (ty, sql_type, expected) in [
            (ColumnType::Int32, "int4", "2147483647"),
            (ColumnType::Int64, "int8", "9007199254740993"),
        ] {
            for (generation, mode) in [
                (format!("DEFAULT {expected}"), None),
                (
                    format!("GENERATED ALWAYS AS IDENTITY (START WITH {expected})"),
                    Some(IdentityMode::Always),
                ),
                (
                    format!("GENERATED BY DEFAULT AS IDENTITY (START WITH {expected})"),
                    Some(IdentityMode::ByDefault),
                ),
            ] {
                for writable in [false, true] {
                    client.batch_execute(&format!(
                        "CREATE TABLE key_row (id {sql_type} {generation} PRIMARY KEY, label text DEFAULT 'default');"
                    )).await.unwrap();
                    let table = Table::new(
                        "public",
                        "key_row",
                        vec![
                            Column::new(
                                "id",
                                ty,
                                false,
                                mode.is_none().then(|| match ty {
                                    ColumnType::Int32 => ColumnDefault::int32(2_147_483_647),
                                    _ => ColumnDefault::int64(9_007_199_254_740_993),
                                }),
                                mode.map(|mode| ColumnGeneration::Identity { mode }),
                            ),
                            Column::new("label", ColumnType::Text, true, None, None),
                        ],
                        vec![Constraint::primary_key("key_row_pkey", ["id"]).unwrap()],
                        Vec::new(),
                    );
                    let operation: OperationDeclaration = serde_json::from_value(serde_json::json!({
                        "permission": "create", "result": "one", "writable_fields": if writable { vec!["label"] } else { vec![] }
                    })).unwrap();
                    let sql = super::create(&table, &model, &operation);
                    client
                        .batch_execute(&format!("PREPARE create_row AS {sql}"))
                        .await
                        .unwrap();
                    let rows = client
                        .simple_query(if writable {
                            "EXECUTE create_row('supplied')"
                        } else {
                            "EXECUTE create_row"
                        })
                        .await
                        .unwrap();
                    let row = rows
                        .iter()
                        .find_map(|row| match row {
                            SimpleQueryMessage::Row(row) => Some(row),
                            _ => None,
                        })
                        .unwrap();
                    assert_eq!(row.get("id"), Some(expected));
                    assert_eq!(
                        row.get("label"),
                        Some(if writable { "supplied" } else { "default" })
                    );
                    client
                        .batch_execute("DEALLOCATE create_row; DROP TABLE key_row;")
                        .await
                        .unwrap();
                }
            }
        }
        drop(client);
        connection.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn query_seeks_and_pages_tied_values_in_both_directions() {
        let database = wamn_test_postgres::database();
        let (client, connection) = tokio_postgres::connect(database.url(), NoTls)
            .await
            .expect("connect to the query test database");
        let connection = tokio::spawn(connection);
        client
            .batch_execute(
                "CREATE TABLE page_row (id uuid PRIMARY KEY, rank bigint NOT NULL);
                 CREATE INDEX page_row_rank_id ON page_row (rank, id);
                 INSERT INTO page_row SELECT lpad(n::text, 32, '0')::uuid, n / 100
                 FROM generate_series(1, 4000) AS n;
                 ANALYZE page_row;",
            )
            .await
            .expect("seed ordered rows with tied sort values");
        let table = Table::new(
            "public",
            "page_row",
            vec![
                Column::new("id", ColumnType::Uuid, false, None, None),
                Column::new("rank", ColumnType::Int64, false, None, None),
            ],
            vec![Constraint::primary_key("page_row_pkey", ["id"]).unwrap()],
            Vec::new(),
        );
        let operation: OperationDeclaration =
            serde_json::from_value(serde_json::json!({"permission": "read", "result": "page"}))
                .expect("declare a paged query");
        for direction in [CursorDirection::Ascending, CursorDirection::Descending] {
            let ascending = matches!(direction, CursorDirection::Ascending);
            let sql = query(&table, &operation, "rank", direction, false, false);
            client
                .batch_execute(&format!("PREPARE page AS {sql}"))
                .await
                .expect("prepare the generated query");
            let mut cursor = "NULL, NULL".to_owned();
            for offset in (0_u32..126).step_by(21) {
                let messages = client
                    .simple_query(&format!("EXECUTE page({cursor}, 21)"))
                    .await
                    .expect("read a generated page");
                let rows = messages
                    .iter()
                    .filter_map(|message| match message {
                        SimpleQueryMessage::Row(row) => Some(row),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let ids = rows
                    .iter()
                    .map(|row| row.get(0).unwrap().replace('-', "").parse::<u32>().unwrap())
                    .collect::<Vec<_>>();
                let expected = if ascending {
                    (offset + 1..=offset + 21).collect::<Vec<_>>()
                } else {
                    (4000 - offset - 20..=4000 - offset).rev().collect()
                };
                assert_eq!(
                    ids, expected,
                    "pages must neither skip nor repeat tied rows"
                );
                let last = rows.last().expect("a full page");
                cursor = format!("{}, '{}'", last.get(1).unwrap(), last.get(0).unwrap());
            }
            let plan: serde_json::Value = client
                .query_one(
                    "EXPLAIN (ANALYZE, FORMAT JSON) EXECUTE page(20, '00000000-0000-0000-0000-000000002000', 21)",
                    &[],
                )
                .await
                .expect("measure a page that starts inside the index")
                .get(0);
            assert!(scanned_rows(&plan[0]["Plan"]) <= 21.0, "{plan}");
            client.batch_execute("DEALLOCATE page").await.unwrap();
        }
        drop(client);
        connection.await.unwrap().unwrap();
    }

    fn scanned_rows(plan: &serde_json::Value) -> f64 {
        if plan["Relation Name"] == "page_row" {
            return plan["Actual Rows"].as_f64().unwrap()
                + plan["Rows Removed by Filter"].as_f64().unwrap_or(0.0);
        }
        plan["Plans"]
            .as_array()
            .map_or(0.0, |plans| plans.iter().map(scanned_rows).sum())
    }
}

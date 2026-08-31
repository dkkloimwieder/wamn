use wamn_schema_introspection::ir::{Column, ColumnType, Table};

use crate::{CursorDirection, OperationDeclaration};

pub(crate) fn get(table: &Table) -> String {
    format!(
        "SELECT\n    {}\nFROM {} AS model\nWHERE model.id = $1::uuid;\n",
        select_columns(table),
        table.name()
    )
}

pub(crate) fn create(table: &Table, operation: &OperationDeclaration) -> String {
    let columns = operation.writable_fields.join(", ");
    let binds = operation
        .writable_fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let ty = column_type(table, field);
            format!("${}::{}", index + 1, postgres_type(ty))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO {} ({columns})\nVALUES ({binds})\nRETURNING\n    {};\n",
        table.name(),
        returning_columns(table)
    )
}

pub(crate) fn update(table: &Table, operation: &OperationDeclaration) -> String {
    let revision = operation
        .revision_field
        .as_deref()
        .expect("update validation requires revision");
    let assignments = operation
        .writable_fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let value_bind = 4 + index * 2;
            format!(
                "        {field} = CASE WHEN ${}::boolean THEN ${value_bind}::{} ELSE model.{field} END",
                value_bind - 1,
                postgres_type(column_type(table, field))
            )
        })
        .chain(std::iter::once(format!(
            "        {revision} = model.{revision} + 1"
        )))
        .collect::<Vec<_>>()
        .join(",\n");
    let returned = table
        .columns()
        .iter()
        .map(|column| format!("updated.{}", column.name()))
        .collect::<Vec<_>>()
        .join(",\n    ");
    format!(
        "WITH target AS MATERIALIZED (\n    SELECT id, {revision}\n    FROM {}\n    WHERE id = $1::uuid\n    FOR UPDATE\n),\nupdated AS (\n    UPDATE {} AS model\n    SET\n{assignments}\n    FROM target\n    WHERE model.id = target.id\n      AND target.{revision} = $2::int8\n    RETURNING model.*\n)\nSELECT\n    CASE\n        WHEN NOT EXISTS (SELECT 1 FROM target) THEN 'not_found'\n        WHEN NOT EXISTS (SELECT 1 FROM updated) THEN 'concurrency_conflict'\n        ELSE 'updated'\n    END AS outcome,\n    (SELECT target.{revision} FROM target) AS observed_{revision},\n    {returned}\nFROM (SELECT 1) AS singleton\nLEFT JOIN updated ON TRUE;\n",
        table.name(),
        table.name()
    )
}

pub(crate) fn delete(table: &Table, operation: &OperationDeclaration) -> String {
    let revision = operation
        .revision_field
        .as_deref()
        .expect("delete validation requires revision");
    format!(
        "WITH target AS MATERIALIZED (\n    SELECT id, {revision}\n    FROM {}\n    WHERE id = $1::uuid\n    FOR UPDATE\n),\ndeleted AS (\n    DELETE FROM {} AS model\n    USING target\n    WHERE model.id = target.id\n      AND target.{revision} = $2::int8\n    RETURNING model.id\n)\nSELECT CASE\n    WHEN NOT EXISTS (SELECT 1 FROM target) THEN 'not_found'\n    WHEN NOT EXISTS (SELECT 1 FROM deleted) THEN 'concurrency_conflict'\n    ELSE 'deleted'\nEND AS outcome;\n",
        table.name(),
        table.name()
    )
}

pub(crate) fn query(
    table: &Table,
    operation: &OperationDeclaration,
    sort_field: &str,
    direction: CursorDirection,
) -> String {
    let mut predicates = Vec::new();
    for (index, filter) in operation.filters.iter().enumerate() {
        let field = &filter.field;
        predicates.push(format!(
            "    (${bind}::jsonb IS NULL OR model.{field} IN (\n        SELECT filter.value::{}\n        FROM jsonb_array_elements_text(${bind}::jsonb) AS filter(value)\n    ))",
            postgres_type(column_type(table, field)),
            bind = index + 1,
        ));
    }
    let cursor_value_bind = operation.filters.len() + 1;
    let cursor_id_bind = cursor_value_bind + 1;
    let limit_bind = cursor_id_bind + 1;
    let sort_type = postgres_type(column_type(table, sort_field));
    let comparison = match direction {
        CursorDirection::Ascending => ">",
        CursorDirection::Descending => "<",
    };
    predicates.push(format!(
        "    (${cursor_value_bind}::{sort_type} IS NULL OR model.{sort_field} {comparison} ${cursor_value_bind}::{sort_type}\n        OR (model.{sort_field} = ${cursor_value_bind}::{sort_type} AND model.id {comparison} ${cursor_id_bind}::uuid))"
    ));
    format!(
        "SELECT\n    {}\nFROM {} AS model\nWHERE\n{}\nORDER BY model.{sort_field} {direction}, model.id {direction}\nLIMIT ${limit_bind}::int8;\n",
        select_columns(table),
        table.name(),
        predicates.join("\n    AND\n"),
        direction = sql_direction(direction),
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

fn column_type(table: &Table, field: &str) -> ColumnType {
    table
        .columns()
        .iter()
        .find(|column| column.name() == field)
        .expect("validation resolved SQL field")
        .column_type()
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

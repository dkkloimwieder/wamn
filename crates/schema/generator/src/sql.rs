use wamn_schema_introspection::ir::{Column, ColumnType, Table};

use crate::generate::{CLAIM_COMMAND_COLUMN, CLAIM_KEY_COLUMN};
use crate::manifest::DeleteMode;
use crate::{CursorDirection, OperationDeclaration};

/// One create's resolved claim, as the emitters need it.
pub(crate) struct Claim<'a> {
    pub(crate) table: &'a Table,
    /// Constraint name of the claim's `idempotency_key` primary key.
    pub(crate) primary_key: &'a str,
    /// Model field paired with the claim column that pre-generated it, in
    /// model-field order.
    pub(crate) identities: Vec<(&'a str, &'a str)>,
}

impl Claim<'_> {
    /// The claim column that mints the created row's `id`.
    fn id_column(&self) -> &str {
        self.identities
            .iter()
            .find(|(field, _)| *field == "id")
            .map(|(_, claim_column)| *claim_column)
            .expect("claim validation requires an id identity")
    }
}

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
    let live = if tombstoned {
        format!("\n  AND {LIVE_ROW}")
    } else {
        String::new()
    };
    format!(
        "SELECT\n    {}\nFROM {} AS model\nWHERE model.id = $1::uuid{live};\n",
        select_columns(table),
        table.name()
    )
}

/// Mint the claim, or yield nothing because this key already has one.
///
/// The claim row pre-generates every identity the create hands out, so the
/// replay path reads back the ids the FIRST call minted instead of minting a
/// second set. Yielding nothing is not a failure: it is the signal that the
/// caller must read the created row through [`create_replay`].
pub(crate) fn create_claim(claim: &Claim<'_>) -> String {
    format!(
        "INSERT INTO {} ({CLAIM_KEY_COLUMN}, {CLAIM_COMMAND_COLUMN})\nVALUES ($1::text, $2::bytea)\nON CONFLICT ON CONSTRAINT {} DO NOTHING\nRETURNING\n    {};\n",
        claim.table.name(),
        claim.primary_key,
        claim
            .identities
            .iter()
            .map(|(_, claim_column)| *claim_column)
            .collect::<Vec<_>>()
            .join(",\n    "),
    )
}

/// Read the current state of the row the claim for one key created, writing
/// nothing.
///
/// The row is read live, so a replay after a later update returns that update.
/// The id is still the one the claim minted. The canonical command comes back
/// beside the row so the caller can refuse a key rebound to a different request.
/// The join is inner because the claim and its row are inserted in one
/// transaction: a visible claim always has its row.
pub(crate) fn create_replay(table: &Table, claim: &Claim<'_>) -> String {
    format!(
        "SELECT\n    claim.{CLAIM_COMMAND_COLUMN},\n    {}\nFROM {} AS claim\nJOIN {} AS model\n    ON model.id = claim.{}\nWHERE claim.{CLAIM_KEY_COLUMN} = $1::text;\n",
        select_columns(table),
        claim.table.name(),
        table.name(),
        claim.id_column(),
    )
}

/// Insert the row under the identities the claim already minted.
///
/// Every identity is bound, never defaulted: a `DEFAULT gen_random_uuid()` here
/// would mint a fresh id on every attempt, which is exactly the duplicate
/// identity the claim exists to prevent.
pub(crate) fn create(table: &Table, claim: &Claim<'_>, operation: &OperationDeclaration) -> String {
    let fields = claim
        .identities
        .iter()
        .map(|(field, _)| *field)
        .chain(operation.writable_fields.iter().map(String::as_str))
        .collect::<Vec<_>>();
    let binds = fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let ty = column_type(table, field);
            format!("${}::{}", index + 1, postgres_type(ty))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO {} ({})\nVALUES ({binds})\nRETURNING\n    {};\n",
        table.name(),
        fields.join(", "),
        returning_columns(table)
    )
}

pub(crate) fn update(table: &Table, operation: &OperationDeclaration, tombstoned: bool) -> String {
    let revision = operation
        .revision_field
        .as_deref()
        .expect("update validation requires revision");
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
        "WITH target AS MATERIALIZED (\n    SELECT id, {revision}\n    FROM {}\n    WHERE id = $1::uuid{live}\n    FOR UPDATE\n),\nupdated AS (\n    UPDATE {} AS model\n    SET\n{assignments}\n    FROM target\n    WHERE model.id = target.id\n      AND target.{revision} = $2::int8\n    RETURNING\n    {returning}\n)\nSELECT\n    CASE\n        WHEN NOT EXISTS (SELECT 1 FROM target) THEN 'not_found'\n        WHEN NOT EXISTS (SELECT 1 FROM updated) THEN 'concurrency_conflict'\n        ELSE 'updated'\n    END AS outcome,\n    (SELECT target.{revision} FROM target) AS observed_{revision},\n    {returned}\nFROM (SELECT 1) AS singleton\nLEFT JOIN updated ON TRUE;\n",
        table.name(),
        table.name()
    )
}

/// The delete statement of the declared mode.
///
/// A hard delete removes the row, and an inbound foreign key can refuse it. A
/// tombstone sets the marker instead, so it removes nothing and violates
/// nothing. Both report the same three outcomes.
pub(crate) fn delete(table: &Table, operation: &OperationDeclaration, mode: DeleteMode) -> String {
    let revision = operation
        .revision_field
        .as_deref()
        .expect("delete validation requires revision");
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
        "WITH target AS MATERIALIZED (\n    SELECT id, {revision}\n    FROM {}\n    WHERE id = $1::uuid{live}\n    FOR UPDATE\n),\ndeleted AS (\n{removal}\n    WHERE model.id = target.id\n      AND target.{revision} = $2::int8\n    RETURNING model.id\n)\nSELECT CASE\n    WHEN NOT EXISTS (SELECT 1 FROM target) THEN 'not_found'\n    WHEN NOT EXISTS (SELECT 1 FROM deleted) THEN 'concurrency_conflict'\n    ELSE 'deleted'\nEND AS outcome;\n",
        table.name()
    )
}

pub(crate) fn query(
    table: &Table,
    operation: &OperationDeclaration,
    sort_field: &str,
    direction: CursorDirection,
    tombstoned: bool,
) -> String {
    let mut predicates = Vec::new();
    if tombstoned {
        predicates.push(format!("    {LIVE_ROW}"));
    }
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

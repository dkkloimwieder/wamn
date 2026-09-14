//! The record-history shapes that stay out of the schema description.
//!
//! `wamn_history.create_history_table` in `deploy/sql/record-history.sql` is
//! the one definition of the history table. This module describes the shape
//! that the function creates for a package relation, which has no tenant
//! column. Introspection admits a history table only when it has this shape,
//! and the generator adds this description for each logged relation.

use crate::ir::{Column, ColumnGeneration, ColumnType, Constraint, IdentityMode, Table};

/// The suffix of every history table name. Naming reserves it.
pub const HISTORY_TABLE_SUFFIX: &str = "_history";

/// The log trigger that apply-package installs on a logged relation.
pub const RECORD_HISTORY_LOG_TRIGGER: &str = "wamn_record_history_log";

/// The retention of a relation that keeps no log.
pub const NO_LOG_RETENTION: &str = "none";

/// PostgreSQL truncates an object name of 64 bytes or more.
const NAME_BYTE_LIMIT: usize = 64;

/// The identity column that orders the entries of one row.
pub(crate) const IDENTITY_COLUMN: &str = "position";

/// The primary key columns, in key order.
pub(crate) const PRIMARY_KEY_COLUMNS: [&str; 2] = ["row_key", "position"];

/// The columns that carry a CHECK constraint named `<history>_<column>_check`.
pub(crate) const CHECKED_COLUMNS: [&str; 2] = ["kind", "operation"];

/// The history table columns in creation order, without `tenant_id`.
const HISTORY_COLUMNS: [(&str, ColumnType); 9] = [
    (IDENTITY_COLUMN, ColumnType::Int64),
    ("row_key", ColumnType::Json),
    ("kind", ColumnType::Text),
    ("operation", ColumnType::Text),
    ("changed_by", ColumnType::Uuid),
    ("changed_at", ColumnType::Timestamptz),
    ("transaction_id", ColumnType::Int64),
    ("before", ColumnType::Json),
    ("after", ColumnType::Json),
];

/// The history table name of a relation.
pub fn history_table_name(relation: &str) -> String {
    format!("{relation}{HISTORY_TABLE_SUFFIX}")
}

/// Whether a relation name ends with the reserved history suffix.
pub fn is_history_table_name(name: &str) -> bool {
    name.ends_with(HISTORY_TABLE_SUFFIX)
}

/// The fixed description of the history table of `schema.relation`.
///
/// The description carries the columns and the primary key. Every column is
/// `NOT NULL`, and `position` is an identity column that always generates.
pub fn history_table(schema: &str, relation: &str) -> Table {
    let history = history_table_name(relation);
    let columns = HISTORY_COLUMNS
        .iter()
        .map(|(name, column_type)| {
            let generation = (*name == IDENTITY_COLUMN).then_some(ColumnGeneration::Identity {
                mode: IdentityMode::Always,
            });
            Column::new(*name, *column_type, false, None, generation)
        })
        .collect();
    let primary_key = Constraint::primary_key(format!("{history}_pkey"), PRIMARY_KEY_COLUMNS)
        .expect("the history primary key name is not empty");
    Table::new(schema, history, columns, vec![primary_key], Vec::new())
}

/// The columns that the log trigger function writes, in creation order.
///
/// The identity column is not among them, because PostgreSQL generates it.
pub fn history_entry_columns() -> impl Iterator<Item = &'static str> {
    HISTORY_COLUMNS
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| *name != IDENTITY_COLUMN)
}

/// The history table column names and types in creation order.
pub(crate) fn history_columns() -> impl Iterator<Item = (&'static str, ColumnType)> {
    HISTORY_COLUMNS.iter().copied()
}

/// Every object name that the history table of `relation` derives.
///
/// The names are the NOT NULL constraint of each column, the primary key, the
/// two CHECK constraints, and the identity sequence.
pub fn history_object_names(relation: &str) -> Vec<String> {
    let history = history_table_name(relation);
    HISTORY_COLUMNS
        .iter()
        .map(|(column, _)| format!("{history}_{column}_not_null"))
        .chain([format!("{history}_pkey")])
        .chain(
            CHECKED_COLUMNS
                .iter()
                .map(|column| format!("{history}_{column}_check")),
        )
        .chain([format!("{history}_{IDENTITY_COLUMN}_seq")])
        .collect()
}

/// Whether every derived history object name of `relation` fits in a PostgreSQL name.
pub fn history_object_names_fit(relation: &str) -> bool {
    history_object_names(relation)
        .iter()
        .all(|name| name.len() < NAME_BYTE_LIMIT)
}

/// Whether `value` is the retention of a relation that keeps a log.
///
/// The value is `unlimited` or `P<n>D`, where n is a positive whole number of
/// days with no leading zero.
pub fn is_log_retention(value: &str) -> bool {
    if value == "unlimited" {
        return true;
    }
    value
        .strip_prefix('P')
        .and_then(|rest| rest.strip_suffix('D'))
        .is_some_and(|days| {
            days.bytes().next().is_some_and(|first| first != b'0')
                && days.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::{history_object_names_fit, is_log_retention};

    #[test]
    fn log_retention_accepts_unlimited_and_whole_days_only() {
        for accepted in ["unlimited", "P1D", "P30D", "P90D", "P365D", "P1000D"] {
            assert!(is_log_retention(accepted), "{accepted}");
        }
        for refused in [
            "none",
            "",
            "P",
            "PD",
            "P0D",
            "P01D",
            "P-1D",
            "P+1D",
            "P1.5D",
            "P1W",
            "P1M",
            "P1Y",
            "PT1H",
            "P1DT1H",
            "p30d",
            "P30d",
            " P30D",
            "P30D ",
            "UNLIMITED",
        ] {
            assert!(!is_log_retention(refused), "{refused}");
        }
    }

    #[test]
    fn a_relation_name_of_32_bytes_overflows_its_history_names() {
        assert!(history_object_names_fit(&"r".repeat(31)));
        assert!(!history_object_names_fit(&"r".repeat(32)));
    }
}

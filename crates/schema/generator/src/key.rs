//! Primary-key facts shared by generated SQL and contracts.

use wamn_schema_introspection::ir::{ColumnType, ConstraintType, Table};

use crate::{GenerateError, GenerateErrorType};

/// The supported scalar types of a generated model's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyType {
    Uuid,
    Int32,
    Int64,
}

impl KeyType {
    pub(crate) const fn column_type(self) -> ColumnType {
        match self {
            Self::Uuid => ColumnType::Uuid,
            Self::Int32 => ColumnType::Int32,
            Self::Int64 => ColumnType::Int64,
        }
    }
}

/// Resolve the key from the constraint and its introspected column type.
pub(crate) fn key_type(table: &Table) -> Result<KeyType, GenerateError> {
    let relation = format!("{}.{}", table.schema(), table.name());
    // Generated operations address the one key column as `id`.
    if primary_key(table) != Some("id") {
        return Err(GenerateError::for_object(
            GenerateErrorType::InvalidModel,
            format!("{relation} generated operations require a single-column primary key named id"),
            relation,
        ));
    }
    let column = table
        .columns()
        .iter()
        .find(|column| column.name() == "id")
        .expect("the primary key references an introspected column");
    match column.column_type() {
        ColumnType::Uuid => Ok(KeyType::Uuid),
        ColumnType::Int32 => Ok(KeyType::Int32),
        ColumnType::Int64 => Ok(KeyType::Int64),
        ty => Err(GenerateError::for_object(
            GenerateErrorType::InvalidModel,
            format!(
                "{relation}.id has unsupported primary key type {}; generated operations support uuid, int4, or int8",
                ty.as_str()
            ),
            format!("{relation}.id"),
        )),
    }
}

/// The model's one-column primary key.
pub(crate) fn primary_key(table: &Table) -> Option<&str> {
    table
        .constraints()
        .iter()
        .find_map(|constraint| match constraint.constraint_type() {
            ConstraintType::PrimaryKey { columns } => match &**columns {
                [column] => Some(&**column),
                _ => None,
            },
            _ => None,
        })
}

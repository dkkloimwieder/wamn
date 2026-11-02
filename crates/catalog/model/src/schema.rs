//! Shared column and foreign-key declarations.

use serde::{Deserialize, Serialize};

/// The non-null cases of frozen `wamn:postgres/types.sql-value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnType {
    Boolean,
    Int32,
    Int64,
    Float64,
    Text,
    Bytes,
    Numeric,
    Timestamptz,
    Json,
    Uuid,
}

impl ColumnType {
    /// Frozen wire literal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Int32 => "int32",
            Self::Int64 => "int64",
            Self::Float64 => "float64",
            Self::Text => "text",
            Self::Bytes => "bytes",
            Self::Numeric => "numeric",
            Self::Timestamptz => "timestamptz",
            Self::Json => "json",
            Self::Uuid => "uuid",
        }
    }
}

/// One local-to-referenced column pair in a foreign key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ForeignKeyColumn {
    column: Box<str>,
    referenced_column: Box<str>,
}

impl ForeignKeyColumn {
    /// Construct one foreign-key column pair.
    pub fn new(column: impl Into<Box<str>>, referenced_column: impl Into<Box<str>>) -> Self {
        Self {
            column: column.into(),
            referenced_column: referenced_column.into(),
        }
    }

    pub fn column(&self) -> &str {
        &self.column
    }

    pub fn referenced_column(&self) -> &str {
        &self.referenced_column
    }
}

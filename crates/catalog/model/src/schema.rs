//! Shared column and foreign-key declarations.

use serde::{Deserialize, Serialize};

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

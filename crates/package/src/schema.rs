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

/// How one filter value matches its column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterMatch {
    /// The column equals one of the values.
    #[default]
    Exact,
    /// The column contains one of the values. Only a text column that
    /// declares no value domain takes it.
    Contains,
    /// The column starts with one of the values. Only a text column that
    /// declares no value domain takes it.
    Prefix,
    /// The column lies between an optional minimum and an optional maximum,
    /// both inclusive. Only a number or a time column takes it.
    Range,
    /// The column is empty, or it is not. Only a nullable column takes it.
    IsNull,
}

impl FilterMatch {
    /// Whether the mode is the default, which a declaration leaves unstated.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        matches!(self, Self::Exact)
    }

    /// Whether the value is a list, which a row matches by any one member.
    #[must_use]
    pub const fn takes_list(self) -> bool {
        matches!(self, Self::Exact | Self::Contains | Self::Prefix)
    }

    /// Frozen wire literal of the value's binding in the published contract.
    #[must_use]
    pub const fn binding(self) -> &'static str {
        match self {
            Self::Exact | Self::Contains | Self::Prefix => "json_array",
            Self::Range => "json_range",
            Self::IsNull => "json_boolean",
        }
    }

    /// Frozen wire literal.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Contains => "contains",
            Self::Prefix => "prefix",
            Self::Range => "range",
            Self::IsNull => "is_null",
        }
    }
}

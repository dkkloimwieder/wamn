//! Validated catalog facts for authored query feeds.

use serde::{Deserialize, Serialize};

use crate::schema::ForeignKeyColumn;

/// A schema-qualified table in a feed's read set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedTable {
    pub schema: String,
    pub table: String,
}

/// How a non-root table maps its changes to root keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedDirection {
    ReferencesRoot,
    ReferencedByLink,
}

/// One link, including the base column used for indexed lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedLink {
    pub table: FeedTable,
    pub linking_column: String,
    pub direction: FeedDirection,
    pub foreign_key_table: FeedTable,
    pub foreign_key_columns: Vec<ForeignKeyColumn>,
}

/// The build's compact input to authored feed statement generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedSource {
    pub root: FeedTable,
    pub read_set: Vec<FeedTable>,
    pub join_path: Vec<FeedLink>,
}

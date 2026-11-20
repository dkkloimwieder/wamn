//! Operation contract vocabulary.

use serde::{Deserialize, Serialize};

/// The contract type of one application operation.
///
/// The values are the `type` literals of the generated contract
/// `operation.json`, which is the only source of this fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationType {
    Get,
    Query,
    Create,
    Update,
    Delete,
    Command,
    Projection,
    EventHandler,
}

impl OperationType {
    /// The types that read without changing a record.
    pub const READ_TYPES: [Self; 3] = [Self::Get, Self::Query, Self::Projection];

    /// Whether this type reads without changing a record.
    #[must_use]
    pub fn is_read(self) -> bool {
        Self::READ_TYPES.contains(&self)
    }

    /// The HTTP method of a route that calls an operation of this type.
    ///
    /// A read is a GET and every other type is a POST. The method follows from
    /// the type alone, so no author writes it (`docs/architecture/execution.md`).
    #[must_use]
    pub fn http_method(self) -> &'static str {
        if self.is_read() { "GET" } else { "POST" }
    }
}

impl OperationType {
    /// Frozen operation type literal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Query => "query",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Command => "command",
            Self::Projection => "projection",
            Self::EventHandler => "event_handler",
        }
    }
}

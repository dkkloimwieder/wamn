//! Row operations that trigger workflows.

use serde::{Deserialize, Serialize};

/// The row operation that can trigger a data-change workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WiringEventOperation {
    Insert,
    Update,
    Delete,
}

impl WiringEventOperation {
    /// The stored row operation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

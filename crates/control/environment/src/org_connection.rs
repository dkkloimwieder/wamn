//! Resolved connection inputs retained by an environment apply.

use serde_json::Value;
use wamn_catalog::RequirementType;

/// The resolved inputs retained for one apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConnection {
    pub requirement_type: RequirementType,
    pub definition: Value,
    pub credential_handle: Option<String>,
    pub org_connection_name: Option<String>,
}

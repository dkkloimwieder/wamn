//! Compiled relation facts read by schema installation. Authorship belongs to the compiler.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const CONTROL_OWNED_RELATION_TABLES: [&str; 2] = ["wamn_entities", "wamn_cdc_exclusions"];

/// Installation facts projected from a compiled package document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageManifest {
    pub package: PackageIdentity,
    pub models: BTreeMap<String, ModelDeclaration>,
    #[serde(default)]
    pub internal_relations: BTreeMap<String, InternalRelation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageIdentity {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDeclaration {
    pub schema: String,
    pub table: String,
    pub owner: String,
    #[serde(default)]
    pub audit_log: Option<AuditLogDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InternalRelation {
    pub schema: String,
    pub table: String,
}

impl PackageManifest {
    /// Decode compiled facts without repeating authored document checks.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    pub fn owns_no_table(&self) -> bool {
        self.internal_relations.is_empty()
            && self
                .models
                .values()
                .all(|model| model.owner != self.package.id)
    }
}

impl ModelDeclaration {
    pub fn log_retention(&self) -> Option<&str> {
        self.audit_log
            .as_ref()
            .map(|log| log.retention.as_str())
            .filter(|retention| *retention != wamn_record_history::NO_LOG_RETENTION)
    }
}

/// Stamp columns and log retention of one relation-owning model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditLogDeclaration {
    pub columns: Vec<RecordHistoryColumn>,
    /// `none`, `unlimited`, or `P<n>D` with a positive whole number of days.
    pub retention: String,
}

/// The four reserved record-history column names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordHistoryColumn {
    CreatedAt,
    CreatedBy,
    UpdatedAt,
    UpdatedBy,
}

impl RecordHistoryColumn {
    /// Every reserved name, in trigger argument order.
    pub const ALL: [Self; 4] = [
        Self::CreatedAt,
        Self::CreatedBy,
        Self::UpdatedAt,
        Self::UpdatedBy,
    ];

    /// Reserved column spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CreatedAt => "created_at",
            Self::CreatedBy => "created_by",
            Self::UpdatedAt => "updated_at",
            Self::UpdatedBy => "updated_by",
        }
    }
}

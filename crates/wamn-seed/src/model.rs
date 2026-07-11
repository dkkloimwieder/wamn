//! The seed-dataset model (3.6).
//!
//! A [`Dataset`] is reference/fixture data for a catalog (3.1): rows grouped by
//! entity, each row identified by a **symbolic key** (unique within its entity).
//! Reference fields carry the *key* of the target row, not a uuid — the compiler
//! resolves keys to deterministic ids. This is **data, not DDL**: the compiler
//! (`compile`) turns it into tenant-scoped `INSERT`s against the generated
//! tables. Datasets are stored as jsonb in `catalog.seed_datasets`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use wamn_catalog::EntityId;

/// The seed-model **format** version. `0.1.x` is additive/clarifying only.
pub const SCHEMA_VERSION: &str = "0.1";

/// A named collection of seed rows for a catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Dataset {
    /// The seed-model format version (e.g. `"0.1"`). See [`SCHEMA_VERSION`].
    pub schema_version: String,
    /// The catalog these rows populate (`Catalog::catalog_id`).
    pub catalog_id: String,
    /// Rows grouped by entity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<EntitySeed>,
}

/// The seed rows for one entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct EntitySeed {
    /// The entity id these rows belong to.
    pub entity: EntityId,
    /// The rows, in author order (the emission order within the entity).
    pub rows: Vec<SeedRow>,
}

/// One seed row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SeedRow {
    /// A stable symbolic key, unique within the entity. Used to derive the row's
    /// deterministic id and to reference it from other rows.
    pub key: String,
    /// Field values by field **name**. A `reference` field's value is the target
    /// row's `key`; the managed `id` / `tenant_id` columns are never set here.
    /// A `BTreeMap` keeps the emitted column order stable.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, Value>,
}

impl Dataset {
    /// Parse from canonical JSON (import; also the jsonb stored per dataset).
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Serialize to canonical pretty JSON (export). Default-valued fields are
    /// omitted, so an exported dataset re-imports to an identical value.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("Dataset serializes")
    }
}

//! Reconcile captured models to PostgreSQL REPLICA IDENTITY FULL.

use std::collections::{BTreeMap, BTreeSet};
use wamn_catalog::package_schema::ManagedModel;

/// A table's modeled REPLICA IDENTITY target.
///
/// Only `FULL` versus not-`FULL` is load-bearing. PostgreSQL's index and nothing
/// modes are treated as `Default` so they remain untouched unless a live `FULL`
/// requirement requires a flip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaIdentity {
    Full,
    Default,
}

impl ReplicaIdentity {
    /// Fold PostgreSQL's `relreplident` character into the controlled target.
    pub const fn from_relreplident(value: char) -> Self {
        if value == 'f' {
            Self::Full
        } else {
            Self::Default
        }
    }

    const fn keyword(self) -> &'static str {
        match self {
            Self::Full => "FULL",
            Self::Default => "DEFAULT",
        }
    }
}

/// One idempotent relation flip derived from a package model key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaIdentityFlip {
    pub model_id: String,
    pub schema: String,
    pub table: String,
    pub from: ReplicaIdentity,
    pub to: ReplicaIdentity,
    pub sql: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReplicaIdentityPlan {
    pub flips: Vec<ReplicaIdentityFlip>,
    pub unchanged: Vec<(String, ReplicaIdentity)>,
    pub skipped_absent: Vec<String>,
}

impl ReplicaIdentityPlan {
    pub fn is_noop(&self) -> bool {
        self.flips.is_empty()
    }

    /// Model keys whose required `FULL` setting has not yet been applied.
    ///
    /// Resets to `DEFAULT` are flips but not old-image gaps. A flip to `FULL`
    /// closes only future events; the gap is permanently non-retroactive.
    pub fn pending_old_image_gap(&self) -> Vec<&str> {
        self.flips
            .iter()
            .filter(|flip| flip.to == ReplicaIdentity::Full)
            .map(|flip| flip.model_id.as_str())
            .collect()
    }
}

/// Reconcile every manifest model against current `(schema, table)` state.
///
/// Missing relations are reported and never created here; package migrations
/// own physical schema. Only a differing observed identity emits a flip.
pub fn reconcile_replica_identity(
    models: &[ManagedModel],
    current: &BTreeMap<(String, String), ReplicaIdentity>,
    captured_schemas: &BTreeSet<String>,
) -> ReplicaIdentityPlan {
    let mut plan = ReplicaIdentityPlan::default();
    for model in models {
        let desired = if captured_schemas.contains(&model.schema) {
            ReplicaIdentity::Full
        } else {
            ReplicaIdentity::Default
        };
        let key = (model.schema.clone(), model.table.clone());
        match current.get(&key) {
            None => plan
                .skipped_absent
                .push(format!("{}.{}", model.schema, model.table)),
            Some(&actual) if actual == desired => {
                plan.unchanged.push((model.model_id.clone(), actual));
            }
            Some(&actual) => plan.flips.push(ReplicaIdentityFlip {
                model_id: model.model_id.clone(),
                schema: model.schema.clone(),
                table: model.table.clone(),
                from: actual,
                to: desired,
                sql: alter_replica_identity_sql(&model.schema, &model.table, desired),
            }),
        }
    }
    plan
}

pub fn alter_replica_identity_sql(schema: &str, table: &str, to: ReplicaIdentity) -> String {
    format!(
        "ALTER TABLE {}.{} REPLICA IDENTITY {}",
        postgres_protocol::escape::escape_identifier(schema),
        postgres_protocol::escape::escape_identifier(table),
        to.keyword(),
    )
}

/// Read ordinary tables from the exact set of schemas named by the manifest.
pub fn select_replica_identity_sql() -> &'static str {
    "SELECT n.nspname, c.relname, c.relreplident::text FROM pg_class c \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = ANY($1::text[]) AND c.relkind = 'r' \
     ORDER BY n.nspname, c.relname"
}

//! Reconcile captured models to PostgreSQL REPLICA IDENTITY FULL.

use crate::ManagedModel;
use std::collections::{BTreeMap, BTreeSet};

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
        wamn_pg_core::quote_ident(schema),
        wamn_pg_core::quote_ident(table),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn models() -> Vec<ManagedModel> {
        vec![
            ManagedModel {
                model_id: "orders".into(),
                schema: "store".into(),
                table: "sales_orders".into(),
            },
            ManagedModel {
                model_id: "lines".into(),
                schema: "store".into(),
                table: "line_items".into(),
            },
            ManagedModel {
                model_id: "notes".into(),
                schema: "audit".into(),
                table: "notes".into(),
            },
        ]
    }

    #[test]
    fn captured_models_keep_full_without_registrations() {
        let current = BTreeMap::from([
            (
                ("store".into(), "sales_orders".into()),
                ReplicaIdentity::Default,
            ),
            (("store".into(), "line_items".into()), ReplicaIdentity::Full),
            (("audit".into(), "notes".into()), ReplicaIdentity::Default),
        ]);
        let plan = reconcile_replica_identity(
            &models(),
            &current,
            &BTreeSet::from(["store".to_owned(), "audit".to_owned()]),
        );
        assert_eq!(plan.flips.len(), 2);
        assert!(
            plan.flips
                .iter()
                .all(|flip| flip.to == ReplicaIdentity::Full)
        );
        assert_eq!(
            plan.unchanged,
            vec![("lines".into(), ReplicaIdentity::Full)]
        );
    }

    #[test]
    fn reconciliation_reports_absent_models_and_converges() {
        let captured = BTreeSet::from(["store".to_owned()]);
        let mut current = BTreeMap::from([
            (
                ("store".into(), "sales_orders".into()),
                ReplicaIdentity::Default,
            ),
            (("audit".into(), "notes".into()), ReplicaIdentity::Full),
        ]);
        let plan = reconcile_replica_identity(&models(), &current, &captured);
        assert_eq!(plan.skipped_absent, ["store.line_items"]);
        assert_eq!(plan.pending_old_image_gap(), ["orders"]);
        assert_eq!(plan.flips.len(), 2);
        for flip in plan.flips {
            current.insert((flip.schema, flip.table), flip.to);
        }
        let again = reconcile_replica_identity(&models(), &current, &captured);
        assert!(again.is_noop());
        assert_eq!(
            again.unchanged,
            [
                ("orders".into(), ReplicaIdentity::Full),
                ("notes".into(), ReplicaIdentity::Default),
            ]
        );
    }

    #[test]
    fn relation_identifiers_are_quoted_and_reads_cover_exact_manifest_schemas() {
        assert_eq!(
            alter_replica_identity_sql("app", "orders", ReplicaIdentity::Default),
            "ALTER TABLE \"app\".\"orders\" REPLICA IDENTITY DEFAULT"
        );
        assert_eq!(
            alter_replica_identity_sql("a\"b", "t", ReplicaIdentity::Full),
            "ALTER TABLE \"a\"\"b\".\"t\" REPLICA IDENTITY FULL"
        );
        assert!(select_replica_identity_sql().contains("ANY($1::text[])"));
        assert!(select_replica_identity_sql().ends_with("ORDER BY n.nspname, c.relname"));
    }

    #[test]
    fn only_postgresql_full_maps_to_full() {
        assert_eq!(
            ReplicaIdentity::from_relreplident('f'),
            ReplicaIdentity::Full
        );
        for value in ['d', 'i', 'n'] {
            assert_eq!(
                ReplicaIdentity::from_relreplident(value),
                ReplicaIdentity::Default
            );
        }
    }
}

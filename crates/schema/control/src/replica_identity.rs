//! Pure per-model REPLICA IDENTITY reconciliation.
//!
//! Model identity and physical relation mapping come only from the strict
//! package manifest. Given that mapping, the complete cross-tenant registration
//! set for the package, and current `pg_class.relreplident` observations, this
//! module derives idempotent `FULL`/`DEFAULT` flips without database I/O.
//!
//! `FULL` is required when any registration on a model reads the root `old`
//! image or subscribes to `delete`. The registration read must span every
//! tenant because PostgreSQL replica identity belongs to the shared table, not
//! to an RLS row. A newly applied `FULL` setting is not retroactive: events
//! written before the flip remain without old values and must continue to fail
//! closed as `old-image-absent` in the materializer.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use wamn_event_reg::EventRegistration;

use crate::ManagedModel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadableRegistrationsKind {
    /// The registration relation is absent, not an observed empty set.
    Absent,
    /// The cross-tenant read failed or was blocked by FORCE RLS.
    Unreadable,
}

impl UnreadableRegistrationsKind {
    /// Frozen operator-facing refusal literal.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Absent => "replica-identity-registrations-absent",
            Self::Unreadable => "replica-identity-registrations-unreadable",
        }
    }
}

/// A cross-tenant registration read refused before it could be mistaken for an
/// empty set and silently reset package tables to `DEFAULT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableRegistrations {
    pub kind: UnreadableRegistrationsKind,
    pub package_id: String,
}

impl fmt::Display for UnreadableRegistrations {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: refusing to reconcile REPLICA IDENTITY for package {:?} — ",
            self.kind.name(),
            self.package_id
        )?;
        match self.kind {
            UnreadableRegistrationsKind::Absent => formatter.write_str(
                "catalog.event_registrations does not exist, so this project environment is not registration-provisioned. An unprovisioned set is NOT an empty one: reconciling it would reset every model to DEFAULT, and that flip is NOT retroactive.",
            ),
            UnreadableRegistrationsKind::Unreadable => formatter.write_str(
                "the cross-tenant read failed. It must run with BYPASSRLS under row_security = off: FORCE ROW LEVEL SECURITY can otherwise produce ZERO ROWS WITH NO ERROR, which would be misread as authority to perform a non-retroactive DEFAULT reset.",
            ),
        }
    }
}

impl Error for UnreadableRegistrations {}

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

/// Package model keys whose cross-tenant union of registrations requires FULL.
pub fn entities_requiring_full<'a>(
    models: &'a [ManagedModel],
    registrations: &[EventRegistration],
) -> BTreeSet<&'a str> {
    let known = models
        .iter()
        .map(|model| model.model_id.as_str())
        .collect::<BTreeSet<_>>();
    registrations
        .iter()
        .filter(|registration| registration.requires_replica_identity_full())
        .filter_map(|registration| known.get(registration.entity.as_str()).copied())
        .collect()
}

/// Reconcile every manifest model against current `(schema, table)` state.
///
/// Missing relations are reported and never created here; package migrations
/// own physical schema. Only a differing observed identity emits a flip.
pub fn reconcile_replica_identity(
    models: &[ManagedModel],
    registrations: &[EventRegistration],
    current: &BTreeMap<(String, String), ReplicaIdentity>,
) -> ReplicaIdentityPlan {
    let full = entities_requiring_full(models, registrations);
    let mut plan = ReplicaIdentityPlan::default();
    for model in models {
        let desired = if full.contains(model.model_id.as_str()) {
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
    use wamn_event_reg::{Op, RegistrationInput, SCHEMA_VERSION};

    fn models() -> Vec<ManagedModel> {
        vec![
            ManagedModel {
                model_id: "orders".into(),
                schema: "receiving".into(),
                table: "sales_orders".into(),
            },
            ManagedModel {
                model_id: "lines".into(),
                schema: "receiving".into(),
                table: "line_items".into(),
            },
            ManagedModel {
                model_id: "notes".into(),
                schema: "audit".into(),
                table: "notes".into(),
            },
        ]
    }

    fn registration(
        id: &str,
        entity: &str,
        ops: Vec<Op>,
        condition: Option<&str>,
    ) -> EventRegistration {
        EventRegistration {
            schema_version: SCHEMA_VERSION.into(),
            registration_id: id.into(),
            package_id: "shop".into(),
            flow_id: "notify".into(),
            entity: entity.into(),
            ops,
            input: RegistrationInput::Event,
            condition: condition.map(str::to_owned),
        }
    }

    #[test]
    fn old_condition_delete_and_cross_tenant_union_require_full_by_model_key() {
        let registrations = vec![
            registration(
                "tenant-a-new-only",
                "orders",
                vec![Op::Insert],
                Some("new.status == 'open'"),
            ),
            // A second tenant's changed-to subscription controls the same
            // shared relation, so the complete union requires FULL.
            registration(
                "tenant-b-old",
                "orders",
                vec![Op::Update],
                Some("new.status != old.status"),
            ),
            registration("delete-lines", "lines", vec![Op::Delete], None),
            registration("unknown", "ghost", vec![Op::Delete], None),
        ];
        let models = models();
        let full = entities_requiring_full(&models, &registrations);
        assert_eq!(full, BTreeSet::from(["lines", "orders"]));
    }

    #[test]
    fn new_only_and_insert_only_registrations_leave_the_full_set_empty() {
        let registrations = vec![
            registration(
                "new-only",
                "orders",
                vec![Op::Insert, Op::Update],
                Some("new.status == 'open'"),
            ),
            registration("insert-note", "notes", vec![Op::Insert], None),
        ];
        assert!(entities_requiring_full(&models(), &registrations).is_empty());
    }

    #[test]
    fn delete_without_a_condition_still_requires_full() {
        let registrations = vec![registration(
            "delete-orders",
            "orders",
            vec![Op::Delete],
            None,
        )];
        assert!(entities_requiring_full(&models(), &registrations).contains("orders"));
    }

    #[test]
    fn a_registration_on_an_unknown_model_has_no_relation_to_flip() {
        let registrations = vec![registration(
            "delete-ghost",
            "ghost",
            vec![Op::Delete],
            None,
        )];
        assert!(entities_requiring_full(&models(), &registrations).is_empty());
    }

    #[test]
    fn reconcile_flips_both_directions_and_reports_absent_relations() {
        let registrations = vec![registration(
            "delete-orders",
            "orders",
            vec![Op::Delete],
            None,
        )];
        let current = BTreeMap::from([
            (
                ("receiving".into(), "sales_orders".into()),
                ReplicaIdentity::Default,
            ),
            (
                ("receiving".into(), "line_items".into()),
                ReplicaIdentity::Full,
            ),
        ]);
        let plan = reconcile_replica_identity(&models(), &registrations, &current);
        assert_eq!(plan.flips.len(), 2);
        assert_eq!(plan.pending_old_image_gap(), vec!["orders"]);
        assert_eq!(plan.skipped_absent, vec!["audit.notes"]);
        let up = plan
            .flips
            .iter()
            .find(|flip| flip.model_id == "orders")
            .expect("orders flips up");
        assert_eq!(up.from, ReplicaIdentity::Default);
        assert_eq!(up.to, ReplicaIdentity::Full);
        assert_eq!(
            up.sql,
            "ALTER TABLE \"receiving\".\"sales_orders\" REPLICA IDENTITY FULL"
        );
        let down = plan
            .flips
            .iter()
            .find(|flip| flip.model_id == "lines")
            .expect("lines flips down");
        assert_eq!(down.from, ReplicaIdentity::Full);
        assert_eq!(down.to, ReplicaIdentity::Default);
    }

    #[test]
    fn pending_gap_excludes_resets_and_is_empty_for_a_noop() {
        let registrations = vec![registration(
            "insert-orders",
            "orders",
            vec![Op::Insert],
            None,
        )];
        let current = BTreeMap::from([(
            ("receiving".into(), "line_items".into()),
            ReplicaIdentity::Full,
        )]);
        let plan = reconcile_replica_identity(&models(), &registrations, &current);
        assert_eq!(plan.flips.len(), 1);
        assert_eq!(plan.flips[0].to, ReplicaIdentity::Default);
        assert!(plan.pending_old_image_gap().is_empty());
        assert!(
            ReplicaIdentityPlan::default()
                .pending_old_image_gap()
                .is_empty()
        );
    }

    #[test]
    fn reconcile_at_the_target_is_a_noop() {
        let converged = BTreeMap::from([
            (
                ("receiving".into(), "sales_orders".into()),
                ReplicaIdentity::Full,
            ),
            (
                ("receiving".into(), "line_items".into()),
                ReplicaIdentity::Default,
            ),
            (("audit".into(), "notes".into()), ReplicaIdentity::Default),
        ]);
        let registrations = vec![registration(
            "delete-orders",
            "orders",
            vec![Op::Delete],
            None,
        )];
        let again = reconcile_replica_identity(&models(), &registrations, &converged);
        assert!(again.is_noop());
        assert_eq!(again.unchanged.len(), 3);
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
    fn unreadable_registration_states_fail_closed_and_name_the_nonretroactive_risk() {
        let absent = UnreadableRegistrations {
            kind: UnreadableRegistrationsKind::Absent,
            package_id: "shop".into(),
        };
        let absent_message = absent.to_string();
        assert!(absent_message.starts_with("replica-identity-registrations-absent: "));
        for expected in [
            "package \"shop\"",
            "not registration-provisioned",
            "NOT retroactive",
        ] {
            assert!(absent_message.contains(expected), "{absent_message}");
        }

        let unreadable = UnreadableRegistrations {
            kind: UnreadableRegistrationsKind::Unreadable,
            package_id: "shop".into(),
        };
        let unreadable_message = unreadable.to_string();
        assert!(unreadable_message.starts_with("replica-identity-registrations-unreadable: "));
        for expected in [
            "BYPASSRLS",
            "FORCE ROW LEVEL SECURITY",
            "ZERO ROWS WITH NO ERROR",
        ] {
            assert!(
                unreadable_message.contains(expected),
                "{unreadable_message}"
            );
        }
        assert_ne!(
            UnreadableRegistrationsKind::Absent.name(),
            UnreadableRegistrationsKind::Unreadable.name()
        );
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

//! The per-entity REPLICA IDENTITY FULL reconciler (EVT-REPLICA-IDENT, wamn-l5i9.31).
//!
//! `REPLICA IDENTITY FULL` is a **per-entity knob the DDL engine manages** (the
//! l5i9.1 sign-off, decision d): set only on entities whose registered row-event
//! conditions need the OLD image, reconciled when registrations change; DEFAULT
//! (pkey-only) everywhere else keeps WAL minimal (the poc/cdc1 TOAST test ships a
//! 22.4KB old value under FULL — the global default is NEVER flipped). This module
//! is the PURE decision (the wamn-ddl / D24-orphan precedent — no DB, clock, or
//! wasm): given a catalog + the event registrations for its catalog (read across
//! ALL tenants by the superuser driver, since RI is per-TABLE and tables are
//! shared) + the tables' CURRENT identities (from `pg_class.relreplident`), it
//! produces the idempotent set of `ALTER TABLE … REPLICA IDENTITY FULL|DEFAULT`
//! flips. The `wamn-ctl` shell reads/executes; the throwaway-PG gate proves the
//! live `relreplident` transitions AND the non-retroactive WAL truth.
//!
//! **Which entities need FULL** (derived, never an author-facing knob): ANY
//! registration on the entity whose condition reads the ROOT `old` image
//! ("changed-to"), OR ANY registration subscribing to `delete` (delete
//! tenant-scoping + delete-payload conditions need the old image). The root-`old`
//! detection reuses the SINGLE detector in `wamn_event_reg`
//! ([`wamn_event_reg::condition_references_old`]) — the same one the materializer's
//! per-event old-absent guard keys on, so the two can never diverge.
//!
//! **NON-RETROACTIVE (the binding caveat):** `ALTER TABLE … REPLICA IDENTITY FULL`
//! enriches only WAL written AFTER the flip. Events captured before the flip
//! permanently lack the old image; a newly registered changed-to condition
//! evaluates only from the flip forward, and the materializer treats an absent old
//! image as CANNOT-EVALUATE (an alertable refusal), never condition-false.

use std::collections::{BTreeMap, BTreeSet};

use wamn_catalog::Catalog;
use wamn_event_reg::EventRegistration;

/// A table's REPLICA IDENTITY, as the reconciler models it. Only the FULL vs
/// not-FULL distinction is load-bearing: the reconciler sets a needed entity to
/// FULL and resets an unneeded one to DEFAULT, and never clobbers an
/// index/nothing identity it did not itself set (`'i'`/`'n'` read as `Default`,
/// so a table already at those with no FULL requirement is left untouched).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaIdentity {
    /// `pg_class.relreplident = 'f'`.
    Full,
    /// `'d'` (default: primary key). Also how `'n'` (nothing) / `'i'` (index)
    /// are modelled for the flip decision — we only ever emit `FULL` or reset to
    /// `DEFAULT`, and treating `n`/`i` as "not full" means we never touch a
    /// table's non-default identity unless a FULL requirement demands it.
    Default,
}

impl ReplicaIdentity {
    /// Map a `pg_class.relreplident` character. Anything other than `'f'` is
    /// `Default` for the flip decision.
    pub fn from_relreplident(c: char) -> ReplicaIdentity {
        match c {
            'f' => ReplicaIdentity::Full,
            _ => ReplicaIdentity::Default,
        }
    }

    /// The `ALTER TABLE … REPLICA IDENTITY <kw>` keyword.
    fn keyword(self) -> &'static str {
        match self {
            ReplicaIdentity::Full => "FULL",
            ReplicaIdentity::Default => "DEFAULT",
        }
    }
}

/// One reconcile action: flip an entity's table to a target REPLICA IDENTITY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaIdentityFlip {
    pub entity_id: String,
    pub table: String,
    pub from: ReplicaIdentity,
    pub to: ReplicaIdentity,
    /// The idempotent `ALTER TABLE "<schema>"."<table>" REPLICA IDENTITY …`.
    pub sql: String,
}

/// The reconcile plan: the flips to run, plus the entities already at their
/// target (`unchanged` — reported as no-ops, never executed) and the catalog
/// entities whose table does not exist yet (`skipped_absent` — floor not
/// applied). Idempotent: re-running against the post-flip state yields no flips.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReplicaIdentityPlan {
    pub flips: Vec<ReplicaIdentityFlip>,
    pub unchanged: Vec<(String, ReplicaIdentity)>,
    pub skipped_absent: Vec<String>,
}

impl ReplicaIdentityPlan {
    /// Whether the plan has any flip to apply (a pure no-op reconcile is common
    /// and worth reporting as such).
    pub fn is_noop(&self) -> bool {
        self.flips.is_empty()
    }

    /// The entity ids with an OPEN OLD-IMAGE GAP: a registration requires
    /// REPLICA IDENTITY FULL but the table is still at DEFAULT (the flips whose
    /// target is `Full`). This is the correctness-critical direction — until the
    /// flip is applied, the entity's old-value / delete conditions refuse
    /// `old-image-absent`, and the flip is NON-RETROACTIVE, so the gap is a
    /// permanent old-image hole for events captured meanwhile. It is the
    /// "entity needs RI reconcile" surface (EVT-RI-ORCH, l5i9.61): a read-only
    /// caller (an operator's `--dry-run`, the API registration path — which runs
    /// as `wamn_app` and cannot ALTER but CAN read `pg_class`) computes a plan and
    /// asks this to know a control-plane reconcile is due; run against a plan the
    /// reconciler JUST applied, it is the set of gaps that were closed. Distinct
    /// from [`Self::flips`], which also carries the harmless reset-to-DEFAULT
    /// direction that leaves no gap.
    pub fn pending_old_image_gap(&self) -> Vec<&str> {
        self.flips
            .iter()
            .filter(|f| f.to == ReplicaIdentity::Full)
            .map(|f| f.entity_id.as_str())
            .collect()
    }
}

/// The set of catalog **entity ids** that must run REPLICA IDENTITY FULL,
/// derived from the union of their registrations. An entity needs FULL when ANY
/// of its registrations reads the ROOT `old` image OR subscribes to `delete`
/// ([`EventRegistration::requires_replica_identity_full`]). A registration whose
/// entity is not in the catalog is ignored — a D24 orphan (refused by the orphan
/// guard on publish) with no table to flip.
pub fn entities_requiring_full<'a>(
    catalog: &'a Catalog,
    registrations: &[EventRegistration],
) -> BTreeSet<&'a str> {
    let known: BTreeSet<&str> = catalog.entities.iter().map(|e| e.id.as_str()).collect();
    registrations
        .iter()
        .filter(|r| r.requires_replica_identity_full())
        .filter_map(|r| known.get(r.entity.as_str()).copied())
        .collect()
}

/// Reconcile REPLICA IDENTITY for every catalog entity against its
/// registrations. `current` maps table name → its current identity (the driver
/// reads `pg_class.relreplident`; a table absent from the map does not exist yet
/// and is skipped). `schema` is the data schema the tables live in. Only entities
/// whose desired identity differs from the current one produce a flip.
pub fn reconcile_replica_identity(
    catalog: &Catalog,
    registrations: &[EventRegistration],
    current: &BTreeMap<String, ReplicaIdentity>,
    schema: &str,
) -> ReplicaIdentityPlan {
    let full = entities_requiring_full(catalog, registrations);
    let mut plan = ReplicaIdentityPlan::default();
    for e in &catalog.entities {
        let desired = if full.contains(e.id.as_str()) {
            ReplicaIdentity::Full
        } else {
            ReplicaIdentity::Default
        };
        match current.get(e.name.as_str()) {
            None => plan.skipped_absent.push(e.name.clone()),
            Some(&cur) if cur == desired => plan.unchanged.push((e.name.clone(), cur)),
            Some(&cur) => plan.flips.push(ReplicaIdentityFlip {
                entity_id: e.id.as_str().to_string(),
                table: e.name.clone(),
                from: cur,
                to: desired,
                sql: alter_replica_identity_sql(schema, &e.name, desired),
            }),
        }
    }
    plan
}

/// `ALTER TABLE "<schema>"."<table>" REPLICA IDENTITY FULL|DEFAULT`. Both
/// identifiers are quoted via the canonical `wamn_ddl` quoter (SR3: pure text,
/// quoted identifiers). ALTER needs table ownership — the `wamn_app` role cannot
/// run it, so the shell connects as the superuser/schema owner.
pub fn alter_replica_identity_sql(schema: &str, table: &str, to: ReplicaIdentity) -> String {
    format!(
        "ALTER TABLE {}.{} REPLICA IDENTITY {}",
        wamn_ddl::sql::quote_ident(schema),
        wamn_ddl::sql::quote_ident(table),
        to.keyword(),
    )
}

/// Read every ordinary table's REPLICA IDENTITY in `schema`: projects `relname`
/// and `relreplident::text` (a single-char string the driver folds through
/// [`ReplicaIdentity::from_relreplident`]). `$1` = schema (a value, not an
/// interpolated identifier). SR12: the pure decision has no `pg_class` — the
/// throwaway-PG gate covers that this really observes the live identities.
pub fn select_replica_identity_sql() -> &'static str {
    "SELECT c.relname, c.relreplident::text FROM pg_class c \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = $1 AND c.relkind = 'r'"
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAT: &str = r#"{
      "schema-version": "0.1", "catalog-id": "shop", "version": 1,
      "entities": [
        { "id": "orders", "name": "sales_orders", "fields": [
          { "id": "status", "name": "status", "type": { "kind": "text" } } ] },
        { "id": "lines", "name": "line_items", "fields": [
          { "id": "qty", "name": "qty", "type": { "kind": "int" } } ] },
        { "id": "notes", "name": "notes", "fields": [
          { "id": "body", "name": "body", "type": { "kind": "text" } } ] }
      ]
    }"#;

    fn catalog() -> Catalog {
        Catalog::from_json(CAT).expect("catalog parses")
    }

    fn reg(id: &str, entity: &str, ops: &[&str], condition: Option<&str>) -> EventRegistration {
        let ops_json = ops
            .iter()
            .map(|o| format!("\"{o}\""))
            .collect::<Vec<_>>()
            .join(",");
        let cond_json = match condition {
            Some(c) => format!("\"{c}\""),
            None => "null".to_string(),
        };
        let doc = format!(
            r#"{{"schema-version":"0.1","registration-id":"{id}","catalog-id":"shop",
               "flow-id":"f","entity":"{entity}","ops":[{ops_json}],"condition":{cond_json}}}"#
        );
        EventRegistration::from_json(&doc).expect("registration parses")
    }

    #[test]
    fn old_condition_delete_op_and_cross_tenant_union_all_require_full() {
        let cat = catalog();
        let regs = vec![
            // orders: a new-only condition — does NOT need FULL on its own.
            reg("r1", "orders", &["insert"], Some("new.status == 'ok'")),
            // orders: a SECOND tenant's changed-to condition — needs FULL. The
            // union across tenants is what flips the shared table.
            reg("r2", "orders", &["update"], Some("new.status != old.status")),
            // lines: a delete subscription — needs FULL (delete scoping).
            reg("r3", "lines", &["delete"], None),
            // notes: insert-only, new-only condition — stays DEFAULT.
            reg("r4", "notes", &["insert"], None),
        ];
        let full = entities_requiring_full(&cat, &regs);
        assert!(full.contains("orders"), "old-condition (any tenant) requires FULL");
        assert!(full.contains("lines"), "delete subscription requires FULL");
        assert!(!full.contains("notes"), "insert-only new-only stays DEFAULT");
    }

    #[test]
    fn none_required_derives_the_empty_set() {
        let cat = catalog();
        let regs = vec![
            reg("r1", "orders", &["insert", "update"], Some("new.status == 'ok'")),
            reg("r2", "notes", &["insert"], None),
        ];
        assert!(entities_requiring_full(&cat, &regs).is_empty());
    }

    /// Mutation guard (delete-op rule): a derivation that dropped the delete-op
    /// requirement — keying only on old-conditions — would return the EMPTY set
    /// here. A delete-only registration with no condition MUST require FULL.
    #[test]
    fn a_delete_only_registration_requires_full_even_without_a_condition() {
        let cat = catalog();
        let regs = vec![reg("r1", "orders", &["delete"], None)];
        assert!(entities_requiring_full(&cat, &regs).contains("orders"));
    }

    #[test]
    fn a_registration_on_an_unknown_entity_is_ignored() {
        let cat = catalog();
        let regs = vec![reg("r1", "ghost", &["delete"], None)];
        assert!(entities_requiring_full(&cat, &regs).is_empty());
    }

    #[test]
    fn reconcile_flips_both_directions_and_reports_noops_and_absent() {
        let cat = catalog();
        // orders needs FULL (delete); lines/notes want DEFAULT.
        let regs = vec![reg("r1", "orders", &["delete"], None)];
        // Current live state: sales_orders at DEFAULT (needs flip UP), line_items
        // already FULL from a since-removed registration (needs flip DOWN); notes
        // absent (floor not applied).
        let current = BTreeMap::from([
            ("sales_orders".to_string(), ReplicaIdentity::Default),
            ("line_items".to_string(), ReplicaIdentity::Full),
        ]);
        let plan = reconcile_replica_identity(&cat, &regs, &current, "app");

        // Two flips: sales_orders → FULL, line_items → DEFAULT.
        assert_eq!(plan.flips.len(), 2);
        let up = plan.flips.iter().find(|f| f.table == "sales_orders").unwrap();
        assert_eq!(up.from, ReplicaIdentity::Default);
        assert_eq!(up.to, ReplicaIdentity::Full);
        assert_eq!(up.sql, "ALTER TABLE \"app\".\"sales_orders\" REPLICA IDENTITY FULL");
        let down = plan.flips.iter().find(|f| f.table == "line_items").unwrap();
        assert_eq!(down.from, ReplicaIdentity::Full);
        assert_eq!(down.to, ReplicaIdentity::Default);
        assert_eq!(down.sql, "ALTER TABLE \"app\".\"line_items\" REPLICA IDENTITY DEFAULT");

        // notes has no table row → skipped, not flipped.
        assert_eq!(plan.skipped_absent, vec!["notes".to_string()]);
        assert!(plan.unchanged.is_empty());
    }

    /// The detect-and-surface primitive (EVT-RI-ORCH, l5i9.61): the pending
    /// old-image gap is EXACTLY the flip-UP-to-FULL direction, never the
    /// reset-to-DEFAULT one. It reports entity ids (the caller-meaningful name),
    /// not table names.
    #[test]
    fn pending_old_image_gap_is_the_flip_up_direction_by_entity_id() {
        let cat = catalog();
        // orders needs FULL (delete) → a gap while it is at DEFAULT; lines is at
        // FULL from a since-removed registration → resets to DEFAULT (no gap).
        let regs = vec![reg("r1", "orders", &["delete"], None)];
        let current = BTreeMap::from([
            ("sales_orders".to_string(), ReplicaIdentity::Default),
            ("line_items".to_string(), ReplicaIdentity::Full),
        ]);
        let plan = reconcile_replica_identity(&cat, &regs, &current, "app");
        // entity id "orders" (not the table "sales_orders"); the DEFAULT reset of
        // "lines" is NOT a gap.
        assert_eq!(plan.pending_old_image_gap(), vec!["orders"]);
    }

    /// A reconcile whose only flips reset to DEFAULT surfaces NO gap — the pure
    /// no-op case and the reset-only case must both report an empty gap.
    #[test]
    fn no_gap_when_nothing_needs_full() {
        let cat = catalog();
        // No registration needs FULL, but line_items is stray-FULL and resets.
        let regs = vec![reg("r1", "orders", &["insert"], None)];
        let current = BTreeMap::from([("line_items".to_string(), ReplicaIdentity::Full)]);
        let plan = reconcile_replica_identity(&cat, &regs, &current, "app");
        assert_eq!(plan.flips.len(), 1, "the stray FULL resets to DEFAULT");
        assert!(plan.pending_old_image_gap().is_empty(), "a reset is not a gap");
        // And a genuine no-op plan is trivially gap-free.
        assert!(ReplicaIdentityPlan::default().pending_old_image_gap().is_empty());
    }

    #[test]
    fn reconcile_is_idempotent_at_the_target_state() {
        let cat = catalog();
        let regs = vec![reg("r1", "orders", &["delete"], None)];
        // The post-flip state: orders FULL, the rest DEFAULT.
        let current = BTreeMap::from([
            ("sales_orders".to_string(), ReplicaIdentity::Full),
            ("line_items".to_string(), ReplicaIdentity::Default),
            ("notes".to_string(), ReplicaIdentity::Default),
        ]);
        let plan = reconcile_replica_identity(&cat, &regs, &current, "app");
        assert!(plan.is_noop(), "reconcile at target is a no-op");
        assert_eq!(plan.unchanged.len(), 3);
    }

    /// SQL/DDL string pins (drift guards): the ALTER keyword per target and the
    /// pg_class read — the live gate's relreplident probe rides these exact
    /// strings, and a superuser is required to run the ALTER (table ownership).
    #[test]
    fn alter_and_read_sql_are_pinned() {
        assert_eq!(
            alter_replica_identity_sql("app", "sales_orders", ReplicaIdentity::Full),
            "ALTER TABLE \"app\".\"sales_orders\" REPLICA IDENTITY FULL"
        );
        assert_eq!(
            alter_replica_identity_sql("app", "sales_orders", ReplicaIdentity::Default),
            "ALTER TABLE \"app\".\"sales_orders\" REPLICA IDENTITY DEFAULT"
        );
        // Hostile identifiers are quoted, not injected.
        assert_eq!(
            alter_replica_identity_sql("a\"b", "t", ReplicaIdentity::Full),
            "ALTER TABLE \"a\"\"b\".\"t\" REPLICA IDENTITY FULL"
        );
        assert_eq!(
            select_replica_identity_sql(),
            "SELECT c.relname, c.relreplident::text FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind = 'r'"
        );
    }

    #[test]
    fn relreplident_maps_only_f_to_full() {
        assert_eq!(ReplicaIdentity::from_relreplident('f'), ReplicaIdentity::Full);
        for c in ['d', 'n', 'i'] {
            assert_eq!(ReplicaIdentity::from_relreplident(c), ReplicaIdentity::Default);
        }
    }
}

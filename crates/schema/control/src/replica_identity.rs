//! The per-entity REPLICA IDENTITY FULL reconciler (EVT-REPLICA-IDENT, wamn-l5i9.31).
//!
//! `REPLICA IDENTITY FULL` is a **per-entity knob the DDL engine manages** (the
//! l5i9.1 sign-off, decision d): set only on entities whose registered row-event
//! conditions need the OLD image, reconciled when registrations change; DEFAULT
//! (pkey-only) everywhere else keeps WAL minimal (the wide-row proof ships a
//! 22.4KB old value under FULL — the global default is NEVER flipped). This module
//! is the PURE decision (the wamn-schema-compiler / D24-orphan precedent — no DB, clock, or
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
use std::fmt;

use wamn_event_reg::EventRegistration;
use wamn_schema_model::Catalog;

/// Why a driver could not obtain the FULL cross-tenant registration set for a
/// catalog (wamn-0h0g.12.103, extended by wamn-0h0g.12.119). Each variant is a
/// STABLE refusal name — the ctl verbs and the live gates assert on it, in the
/// shape [`crate::PublicationError`] uses.
///
/// TWO subsystems read that registration set, and both failed open on exactly the
/// same two states, so they share ONE taxonomy rather than growing a parallel
/// error type. The variants pair `<subsystem>` x `<absent | unreadable>`:
///
/// * [`Self::Absent`] / [`Self::Unreadable`] — the REPLICA IDENTITY reconcile
///   (wamn-0h0g.12.103). Their literals carry no subsystem prefix for historical
///   reasons: they were minted before the second owner existed, and live gates
///   key on them verbatim, so they stay frozen exactly as they are.
/// * [`Self::OrphanGuardAbsent`] / [`Self::OrphanGuardUnreadable`] — the D24
///   registration-orphan guard shared by publish-catalog and migrate-catalog
///   (wamn-0h0g.12.119).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadableRegistrationsKind {
    /// `catalog.event_registrations` does not exist: the project-env is not
    /// registration-provisioned (a catalog schema dropped and only partially
    /// rebuilt reaches this). NOT the same state as a provisioned table holding
    /// no rows, which reconciles normally.
    Absent,
    /// The cross-tenant read itself failed. Chiefly the silent case:
    /// `catalog.event_registrations` is `FORCE ROW LEVEL SECURITY`, so a session
    /// that does not BYPASS RLS reads zero rows with no error at all — the
    /// driver runs the read under `row_security = off` so Postgres raises
    /// (SQLSTATE 42501) instead of filtering, and the silence lands here.
    Unreadable,
    /// [`Self::Absent`] as the D24 registration-orphan guard sees it: with no
    /// registration table there is no evidence that the entities this catalog
    /// DROPS are unreferenced, and the guard gates a destructive apply.
    OrphanGuardAbsent,
    /// [`Self::Unreadable`] as the D24 registration-orphan guard sees it. The
    /// same silent RLS mechanism, with a worse consequence: the guard gates an
    /// apply that REMOVES entities.
    OrphanGuardUnreadable,
}

impl UnreadableRegistrationsKind {
    /// The stable refusal name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Absent => "replica-identity-registrations-absent",
            Self::Unreadable => "replica-identity-registrations-unreadable",
            Self::OrphanGuardAbsent => "orphan-guard-registrations-absent",
            Self::OrphanGuardUnreadable => "orphan-guard-registrations-unreadable",
        }
    }

    /// The operation the refusal aborts, as it reads in the message. Keeps each
    /// subsystem from claiming the other's verb: an operator running
    /// `migrate-catalog` must not be told a REPLICA IDENTITY reconcile refused.
    fn refused_action(self) -> &'static str {
        match self {
            Self::Absent | Self::Unreadable => "reconcile REPLICA IDENTITY",
            Self::OrphanGuardAbsent | Self::OrphanGuardUnreadable => {
                "run the D24 registration-orphan guard"
            }
        }
    }
}

/// A registration-set consumer REFUSED because it could not read the
/// cross-tenant registration set its decision depends on.
///
/// **Why this is a refusal and not an empty set.** Neither consumer can
/// distinguish "there is genuinely nothing here" from "I could not see it": both
/// states arrive as an empty row set, and the empty reading is the DANGEROUS one
/// in each case.
///
/// * REPLICA IDENTITY reconcile ([`reconcile_replica_identity`]): an empty set
///   plans a reset to DEFAULT for EVERY entity currently at FULL. The flip is
///   NON-RETROACTIVE, so every row event captured until the next repair
///   permanently lacks its old image, while the run reports as a clean,
///   idempotent-looking success.
/// * D24 registration-orphan guard ([`crate::check_registration_orphans`]): an
///   empty set finds no orphans and CLEARS a destructive apply, letting
///   migrate-catalog REMOVE an entity that event registrations still reference —
///   the precise outcome D24 exists to prevent — and report the run clean.
///
/// Both subsystems are fail-closed everywhere else (the materializer refuses
/// old-image-absent rather than evaluating condition-false); these were the
/// places they used to fail open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableRegistrations {
    pub kind: UnreadableRegistrationsKind,
    /// The catalog whose registrations could not be read (the refusing
    /// operation's scope).
    pub catalog_id: String,
}

impl fmt::Display for UnreadableRegistrations {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: refusing to {} for catalog {:?} — ",
            self.kind.name(),
            self.kind.refused_action(),
            self.catalog_id
        )?;
        match self.kind {
            UnreadableRegistrationsKind::Absent => write!(
                formatter,
                "catalog.event_registrations does not exist, so this project-env is not \
                 registration-provisioned. An unprovisioned registration set is NOT an empty \
                 one: reconciling would reset every entity to REPLICA IDENTITY DEFAULT, and the \
                 flip is NOT retroactive, so every row event captured until the next repair \
                 would permanently lack its old image. Provision the catalog schema first."
            ),
            UnreadableRegistrationsKind::Unreadable => write!(
                formatter,
                "the cross-tenant read of catalog.event_registrations failed. It must run as a \
                 role that BYPASSES row-level security (a superuser or a BYPASSRLS role): the \
                 table is FORCE ROW LEVEL SECURITY, so a non-bypassing session — the table's own \
                 non-superuser owner included — reads ZERO ROWS WITH NO ERROR, which would be \
                 planned as a non-retroactive reset of every entity to DEFAULT. The read runs \
                 under `row_security = off` so that silence surfaces as this refusal."
            ),
            UnreadableRegistrationsKind::OrphanGuardAbsent => write!(
                formatter,
                "catalog.event_registrations does not exist, so this project-env is not \
                 registration-provisioned. An unprovisioned registration set is NOT an empty \
                 one: the guard would find no orphans and CLEAR a DESTRUCTIVE apply, letting \
                 the migration REMOVE an entity that event registrations still reference — the \
                 exact outcome D24 prevents — while reporting the run clean. Provision the \
                 catalog schema first."
            ),
            UnreadableRegistrationsKind::OrphanGuardUnreadable => write!(
                formatter,
                "the cross-tenant read of catalog.event_registrations failed. It must run as a \
                 role that BYPASSES row-level security (a superuser or a BYPASSRLS role): the \
                 table is FORCE ROW LEVEL SECURITY, so a non-bypassing session — the table's own \
                 non-superuser owner included, since FORCE strips the owner's usual exemption and \
                 BYPASSRLS is checked on the current role only, never through inherited \
                 membership — reads ZERO ROWS WITH NO ERROR, which would CLEAR a DESTRUCTIVE \
                 apply and let the migration REMOVE a still-referenced entity. The read runs \
                 under `row_security = off` so that silence surfaces as this refusal."
            ),
        }
    }
}

impl std::error::Error for UnreadableRegistrations {}

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
/// identifiers are quoted via the canonical `wamn_schema_compiler` quoter (SR3: pure text,
/// quoted identifiers). ALTER needs table ownership — the `wamn_app` role cannot
/// run it, so the shell connects as the superuser/schema owner.
pub fn alter_replica_identity_sql(schema: &str, table: &str, to: ReplicaIdentity) -> String {
    format!(
        "ALTER TABLE {}.{} REPLICA IDENTITY {}",
        wamn_schema_compiler::sql::quote_ident(schema),
        wamn_schema_compiler::sql::quote_ident(table),
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
            reg(
                "r2",
                "orders",
                &["update"],
                Some("new.status != old.status"),
            ),
            // lines: a delete subscription — needs FULL (delete scoping).
            reg("r3", "lines", &["delete"], None),
            // notes: insert-only, new-only condition — stays DEFAULT.
            reg("r4", "notes", &["insert"], None),
        ];
        let full = entities_requiring_full(&cat, &regs);
        assert!(
            full.contains("orders"),
            "old-condition (any tenant) requires FULL"
        );
        assert!(full.contains("lines"), "delete subscription requires FULL");
        assert!(
            !full.contains("notes"),
            "insert-only new-only stays DEFAULT"
        );
    }

    #[test]
    fn none_required_derives_the_empty_set() {
        let cat = catalog();
        let regs = vec![
            reg(
                "r1",
                "orders",
                &["insert", "update"],
                Some("new.status == 'ok'"),
            ),
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
        let up = plan
            .flips
            .iter()
            .find(|f| f.table == "sales_orders")
            .unwrap();
        assert_eq!(up.from, ReplicaIdentity::Default);
        assert_eq!(up.to, ReplicaIdentity::Full);
        assert_eq!(
            up.sql,
            "ALTER TABLE \"app\".\"sales_orders\" REPLICA IDENTITY FULL"
        );
        let down = plan.flips.iter().find(|f| f.table == "line_items").unwrap();
        assert_eq!(down.from, ReplicaIdentity::Full);
        assert_eq!(down.to, ReplicaIdentity::Default);
        assert_eq!(
            down.sql,
            "ALTER TABLE \"app\".\"line_items\" REPLICA IDENTITY DEFAULT"
        );

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
        assert!(
            plan.pending_old_image_gap().is_empty(),
            "a reset is not a gap"
        );
        // And a genuine no-op plan is trivially gap-free.
        assert!(
            ReplicaIdentityPlan::default()
                .pending_old_image_gap()
                .is_empty()
        );
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

    /// wamn-0h0g.12.119: the D24 orphan guard's two unreadable-registration
    /// states refuse under their OWN stable names, distinct from the REPLICA
    /// IDENTITY pair, and each names the destructive apply it is preventing. The
    /// live gate (`orphan_guard_live`) keys on these literals.
    #[test]
    fn an_unreadable_registration_set_refuses_the_orphan_guard_by_its_own_name() {
        let absent = UnreadableRegistrations {
            kind: UnreadableRegistrationsKind::OrphanGuardAbsent,
            catalog_id: "shop".to_string(),
        };
        let msg = absent.to_string();
        assert!(
            msg.starts_with("orphan-guard-registrations-absent: "),
            "{msg}"
        );
        for needle in ["\"shop\"", "not registration-provisioned", "DESTRUCTIVE"] {
            assert!(
                msg.contains(needle),
                "absent orphan-guard refusal names {needle:?}: {msg}"
            );
        }

        let unreadable = UnreadableRegistrations {
            kind: UnreadableRegistrationsKind::OrphanGuardUnreadable,
            catalog_id: "shop".to_string(),
        };
        let msg = unreadable.to_string();
        assert!(
            msg.starts_with("orphan-guard-registrations-unreadable: "),
            "{msg}"
        );
        for needle in [
            "FORCE ROW LEVEL SECURITY",
            "ZERO ROWS WITH NO ERROR",
            "DESTRUCTIVE",
        ] {
            assert!(
                msg.contains(needle),
                "unreadable orphan-guard refusal names {needle:?}: {msg}"
            );
        }

        // The orphan guard never borrows the REPLICA IDENTITY verb: an operator
        // running migrate-catalog must not be told a reconcile refused.
        for kind in [
            UnreadableRegistrationsKind::OrphanGuardAbsent,
            UnreadableRegistrationsKind::OrphanGuardUnreadable,
        ] {
            let msg = UnreadableRegistrations {
                kind,
                catalog_id: "shop".to_string(),
            }
            .to_string();
            assert!(
                !msg.contains("reconcile REPLICA IDENTITY"),
                "orphan-guard refusal must not claim the reconcile verb: {msg}"
            );
        }

        // All four states are distinct refusals, never collapsed into one name.
        let names = [
            UnreadableRegistrationsKind::Absent.name(),
            UnreadableRegistrationsKind::Unreadable.name(),
            UnreadableRegistrationsKind::OrphanGuardAbsent.name(),
            UnreadableRegistrationsKind::OrphanGuardUnreadable.name(),
        ];
        let unique: BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len(), "{names:?}");
    }

    /// wamn-0h0g.12.103: the two unreadable-registration states refuse under
    /// their own stable names, and each names the non-retroactive reset it is
    /// preventing. The live gates key on these literals.
    #[test]
    fn an_unreadable_registration_set_refuses_by_name() {
        let absent = UnreadableRegistrations {
            kind: UnreadableRegistrationsKind::Absent,
            catalog_id: "shop".to_string(),
        };
        let msg = absent.to_string();
        assert!(
            msg.starts_with("replica-identity-registrations-absent: "),
            "{msg}"
        );
        for needle in [
            "\"shop\"",
            "not registration-provisioned",
            "NOT retroactive",
        ] {
            assert!(
                msg.contains(needle),
                "absent refusal names {needle:?}: {msg}"
            );
        }

        let unreadable = UnreadableRegistrations {
            kind: UnreadableRegistrationsKind::Unreadable,
            catalog_id: "shop".to_string(),
        };
        let msg = unreadable.to_string();
        assert!(
            msg.starts_with("replica-identity-registrations-unreadable: "),
            "{msg}"
        );
        for needle in ["FORCE ROW LEVEL SECURITY", "ZERO ROWS WITH NO ERROR"] {
            assert!(
                msg.contains(needle),
                "unreadable refusal names {needle:?}: {msg}"
            );
        }

        // The two states are distinct refusals, never collapsed into one name.
        assert_ne!(
            UnreadableRegistrationsKind::Absent.name(),
            UnreadableRegistrationsKind::Unreadable.name()
        );
    }

    #[test]
    fn relreplident_maps_only_f_to_full() {
        assert_eq!(
            ReplicaIdentity::from_relreplident('f'),
            ReplicaIdentity::Full
        );
        for c in ['d', 'n', 'i'] {
            assert_eq!(
                ReplicaIdentity::from_relreplident(c),
                ReplicaIdentity::Default
            );
        }
    }
}

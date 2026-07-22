//! The D24 registration-orphan guard (EVT-REG, wamn-rmxa).
//!
//! A catalog publish or migrate that would remove an entity still referenced by
//! a row in `catalog.event_registrations` is REFUSED, naming the orphaned
//! registrations; the owner deletes them via the registration API first
//! (publish/migrate never seed or prune registrations — D24). This module is the
//! PURE decision: given the entity ids the target catalog keeps and the
//! registrations for its catalog (read across ALL tenants by the superuser
//! driver), it returns the orphans. The `wamn-ctl` verbs run the
//! `$n`-parameterized read ([`crate::sql::select_registrations_for_catalog_sql`])
//! and surface the refusal.
//!
//! **Framing.** An entity is "removed" *relative to the target*: a registration
//! is orphaned iff the entity it references is absent from the target catalog.
//! This needs no separate read of the prior applied/published state and is
//! identical for both verbs — it is exactly D24's "a publish that keeps every
//! referenced entity proceeds unchanged". It also surfaces a registration that
//! references an entity the target never had (pre-existing drift), which D24's
//! anti-drift stance (the rejected "leave dangling registrations inert") wants
//! seen rather than silently carried forward.

use std::collections::BTreeSet;

/// One registration the guard inspects: its id, the owning tenant, and the
/// stable entity id it points at. Rows come from `catalog.event_registrations`
/// across ALL tenants (the entity table is shared, so a removal orphans every
/// tenant's registration on it — the refusal must name each).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationRef {
    pub registration_id: String,
    pub tenant: String,
    pub entity_id: String,
}

/// A publish/migrate refused because it would remove entities still referenced
/// by these registrations (D24). Mirrors [`wamn_ddl::RequiresConfirmation`]: a
/// canonical struct error carrying the offending list, surfaced by the driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphaningPublish {
    /// The registrations whose referenced entity is absent from the target, in
    /// the driver's read order (tenant, then registration id).
    pub orphans: Vec<RegistrationRef>,
}

impl std::fmt::Display for OrphaningPublish {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing to apply this catalog: {} event registration(s) still reference \
             entities it removes — delete them via the registration API first:",
            self.orphans.len()
        )?;
        for o in &self.orphans {
            write!(
                f,
                "\n  - registration {:?} (tenant {:?}) references removed entity {:?}",
                o.registration_id, o.tenant, o.entity_id
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for OrphaningPublish {}

/// The pure D24 decision: refuse if any `referenced` registration points at an
/// entity absent from `present` (the target catalog's entity ids), naming every
/// orphan in input order. `Ok(())` when the target keeps every referenced
/// entity (or there are no registrations).
pub fn check_registration_orphans(
    present: &BTreeSet<&str>,
    referenced: &[RegistrationRef],
) -> Result<(), OrphaningPublish> {
    let orphans: Vec<RegistrationRef> = referenced
        .iter()
        .filter(|r| !present.contains(r.entity_id.as_str()))
        .cloned()
        .collect();
    if orphans.is_empty() {
        Ok(())
    } else {
        Err(OrphaningPublish { orphans })
    }
}

// ---------------------------------------------------------------------------
// The 11.2 suite-orphan guard (test cases as catalog data, wamn-828).
//
// A definition copy (copy-project-env --include definition) carries a tenant's
// test suites, each pinning a concrete `(flow_id, flow_version)`. The copy also
// installs the src's flow registrations; a suite whose pinned version is present
// in NEITHER the src registry (what the copy installs) NOR the dst's existing
// flows would land as an orphan — the `test_suites → flows` FK ON DELETE CASCADE
// would reject the insert with a bare FK error. This is the PURE decision that
// refuses the copy FIRST, naming the orphaned suites, exactly as the D24
// registration guard above refuses an orphaning publish before any mutation.
// ---------------------------------------------------------------------------

/// One suite the guard inspects: its id, owning tenant, and the concrete flow
/// version it pins. Rows come from the src's `<schema>.test_suites`, scoped to
/// the copy's `--tenant` (the copy is per-tenant, unlike the cross-tenant D24
/// entity guard — a flow version is tenant-owned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuiteRef {
    pub suite_id: String,
    pub tenant: String,
    pub flow_id: String,
    pub flow_version: i32,
}

/// A definition copy refused because it would carry these suites onto flow
/// versions absent from the destination (11.2). Mirrors [`OrphaningPublish`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphaningSuiteCopy {
    /// The suites whose pinned `(flow_id, flow_version)` is absent from the
    /// destination, in the driver's read order.
    pub orphans: Vec<SuiteRef>,
}

impl std::fmt::Display for OrphaningSuiteCopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing this definition copy: {} test suite(s) pin a flow version the destination \
             will not have — the flow version must be copied (or already present) first:",
            self.orphans.len()
        )?;
        for o in &self.orphans {
            write!(
                f,
                "\n  - suite {:?} (tenant {:?}) pins {:?} v{}, which is absent",
                o.suite_id, o.tenant, o.flow_id, o.flow_version
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for OrphaningSuiteCopy {}

/// The pure 11.2 decision: refuse if any `referenced` suite pins a
/// `(flow_id, flow_version)` absent from `present` (the flow versions the copy
/// will install plus the dst's existing ones), naming every orphan in input
/// order. `Ok(())` when the destination will hold every pinned version (or there
/// are no suites).
pub fn check_suite_orphans(
    present: &BTreeSet<(String, i32)>,
    referenced: &[SuiteRef],
) -> Result<(), OrphaningSuiteCopy> {
    let orphans: Vec<SuiteRef> = referenced
        .iter()
        .filter(|s| !present.contains(&(s.flow_id.clone(), s.flow_version)))
        .cloned()
        .collect();
    if orphans.is_empty() {
        Ok(())
    } else {
        Err(OrphaningSuiteCopy { orphans })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(id: &str, tenant: &str, entity: &str) -> RegistrationRef {
        RegistrationRef {
            registration_id: id.into(),
            tenant: tenant.into(),
            entity_id: entity.into(),
        }
    }

    #[test]
    fn proceeds_when_target_keeps_every_referenced_entity() {
        let present = BTreeSet::from(["sales_orders", "line_items"]);
        let refs = vec![r("r1", "t1", "sales_orders"), r("r2", "t2", "line_items")];
        assert!(check_registration_orphans(&present, &refs).is_ok());
    }

    #[test]
    fn no_registrations_is_a_clean_proceed() {
        let present = BTreeSet::from(["x"]);
        assert!(check_registration_orphans(&present, &[]).is_ok());
    }

    #[test]
    fn refuses_and_names_every_orphan_across_tenants() {
        // `sales_orders` is gone from the target; two tenants reference it, one
        // registration references the kept `line_items`.
        let present = BTreeSet::from(["line_items"]);
        let refs = vec![
            r("r-a-t1", "t1", "sales_orders"),
            r("r-keep", "t1", "line_items"),
            r("r-a-t2", "t2", "sales_orders"),
        ];
        let err = check_registration_orphans(&present, &refs).unwrap_err();
        // Both tenants' orphans are named; the kept registration is not.
        assert_eq!(
            err.orphans,
            vec![
                r("r-a-t1", "t1", "sales_orders"),
                r("r-a-t2", "t2", "sales_orders")
            ]
        );
        let msg = err.to_string();
        for needle in ["r-a-t1", "r-a-t2", "t1", "t2", "sales_orders"] {
            assert!(msg.contains(needle), "message names {needle:?}: {msg}");
        }
        assert!(
            !msg.contains("r-keep"),
            "the kept registration is not named"
        );
    }

    /// Mutation guard (D24): a decision that ignored removals — the membership
    /// test inverted, or orphans never collected — would return `Ok` here. It
    /// MUST refuse a removal of a referenced entity.
    #[test]
    fn removal_of_a_referenced_entity_is_never_silently_allowed() {
        let present = BTreeSet::from(["kept"]);
        let refs = vec![r("r1", "t1", "dropped")];
        assert!(check_registration_orphans(&present, &refs).is_err());
    }

    // --- 11.2 suite-orphan guard ---

    fn s(suite: &str, tenant: &str, flow: &str, version: i32) -> SuiteRef {
        SuiteRef {
            suite_id: suite.into(),
            tenant: tenant.into(),
            flow_id: flow.into(),
            flow_version: version,
        }
    }

    #[test]
    fn suite_copy_proceeds_when_every_pinned_version_is_present() {
        let present = BTreeSet::from([("f1".to_string(), 1), ("f1".to_string(), 2)]);
        let suites = vec![s("smoke", "t1", "f1", 1), s("regress", "t1", "f1", 2)];
        assert!(check_suite_orphans(&present, &suites).is_ok());
    }

    #[test]
    fn no_suites_is_a_clean_proceed() {
        let present = BTreeSet::from([("f1".to_string(), 1)]);
        assert!(check_suite_orphans(&present, &[]).is_ok());
    }

    #[test]
    fn suite_copy_refuses_and_names_the_orphan() {
        // v1 is present; v99 (a drifted pin) is not — only that suite is named.
        let present = BTreeSet::from([("f1".to_string(), 1)]);
        let suites = vec![s("keep", "t1", "f1", 1), s("orphan", "t1", "f1", 99)];
        let err = check_suite_orphans(&present, &suites).unwrap_err();
        assert_eq!(err.orphans, vec![s("orphan", "t1", "f1", 99)]);
        let msg = err.to_string();
        assert!(
            msg.contains("orphan") && msg.contains("f1") && msg.contains("99"),
            "{msg}"
        );
        assert!(!msg.contains("keep"), "the present suite is not named");
    }

    /// Mutation guard (11.2): a version-blind check — the flow_id compared but
    /// not the version, or the membership test inverted — would pass a suite
    /// pinned to an absent version. It MUST refuse.
    #[test]
    fn suite_pinned_to_an_absent_version_is_never_silently_allowed() {
        // Same flow_id present at a DIFFERENT version: a flow-id-only check would
        // wrongly accept this.
        let present = BTreeSet::from([("f1".to_string(), 1)]);
        let suites = vec![s("s", "t1", "f1", 2)];
        assert!(check_suite_orphans(&present, &suites).is_err());
    }
}

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
}

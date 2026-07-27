//! CF-EXPOSURE authoritative-view and publication-boundary proofs.

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
    const PUBLISHER: &str = include_str!("../../../services/ctl/src/publish_catalog.rs");
    const COPIER: &str = include_str!("../../../services/ctl/src/copy_project_env.rs");

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    struct ExposureState {
        definitions: BTreeMap<(i32, String), String>,
        active_hash: BTreeMap<String, (String, bool)>,
        tombstones: BTreeSet<String>,
        events: Vec<(String, String)>,
        head: Option<i32>,
    }

    fn publish(
        stored: &mut ExposureState,
        version: i32,
        definitions: &[(&str, &str)],
        fail_after_definitions: bool,
    ) -> Result<(), &'static str> {
        let mut transaction = stored.clone();
        for (id, _) in definitions {
            if transaction.tombstones.contains(*id) {
                return Err("tombstoned-attachment-id");
            }
        }
        if let Some(previous) = transaction.head {
            let previous_ids = transaction
                .definitions
                .keys()
                .filter_map(|(release, id)| (*release == previous).then_some(id.clone()))
                .collect::<Vec<_>>();
            for id in previous_ids {
                if !definitions.iter().any(|(next, _)| *next == id) {
                    transaction.tombstones.insert(id.clone());
                    if let Some((_, enabled)) = transaction.active_hash.get_mut(&id) {
                        *enabled = false;
                    }
                    transaction.events.push((id, "removed".into()));
                }
            }
        }
        for (id, hash) in definitions {
            transaction
                .definitions
                .insert((version, (*id).into()), (*hash).into());
            match transaction.active_hash.get_mut(*id) {
                None => {
                    transaction
                        .active_hash
                        .insert((*id).into(), ((*hash).into(), false));
                    transaction.events.push(((*id).into(), "new".into()));
                }
                Some((confirmed, enabled)) if confirmed != hash => {
                    *confirmed = (*hash).into();
                    *enabled = false;
                    transaction
                        .events
                        .push(((*id).into(), "definition-changed".into()));
                }
                Some(_) => {}
            }
        }
        if fail_after_definitions {
            return Err("injected-after-members");
        }
        transaction.head = Some(version);
        *stored = transaction;
        Ok(())
    }

    #[test]
    fn unchanged_hash_carries_changed_hash_disables_and_recovery_remains_addressable() {
        let mut stored = ExposureState::default();
        publish(&mut stored, 1, &[("receipts", "h1")], false).unwrap();
        stored.active_hash.get_mut("receipts").unwrap().1 = true;
        publish(&mut stored, 2, &[("receipts", "h1")], false).unwrap();
        assert_eq!(
            stored.active_hash.get("receipts"),
            Some(&("h1".into(), true))
        );
        publish(&mut stored, 3, &[("receipts", "h2")], false).unwrap();
        assert_eq!(
            stored.active_hash.get("receipts"),
            Some(&("h2".into(), false))
        );
        assert_eq!(
            stored.definitions.get(&(1, "receipts".into())),
            Some(&"h1".into()),
            "disabled-definition recovery retains the old immutable definition"
        );
    }

    #[test]
    fn publication_fault_rolls_back_definitions_activation_events_and_head() {
        let mut stored = ExposureState::default();
        publish(&mut stored, 1, &[("receipts", "h1")], false).unwrap();
        let before = stored.clone();
        assert_eq!(
            publish(&mut stored, 2, &[("receipts", "h2")], true),
            Err("injected-after-members")
        );
        assert_eq!(stored, before);
    }

    #[test]
    fn removed_id_is_tombstoned_and_cannot_be_reused() {
        let mut stored = ExposureState::default();
        publish(&mut stored, 1, &[("receipts", "h1")], false).unwrap();
        publish(&mut stored, 2, &[], false).unwrap();
        assert!(stored.tombstones.contains("receipts"));
        assert_eq!(
            publish(&mut stored, 3, &[("receipts", "h3")], false),
            Err("tombstoned-attachment-id")
        );
    }

    #[test]
    fn ddl_and_both_production_writers_share_the_exposure_boundary() {
        for table in [
            "release_exposure_manifests",
            "release_sources",
            "release_attachments",
            "attachment_tombstones",
            "attachment_activation",
            "attachment_activation_events",
        ] {
            assert!(
                CATALOG_SCHEMA.contains(&format!("CREATE TABLE catalog.{table}")),
                "missing catalog.{table}"
            );
        }
        for view in [
            "attachment_definitions",
            "active_attachments",
            "http_routes",
            "cron_attachments",
        ] {
            assert!(
                CATALOG_SCHEMA.contains(&format!("CREATE VIEW catalog.{view}")),
                "missing authoritative catalog.{view}"
            );
        }
        for source in [PUBLISHER, COPIER] {
            for boundary in [
                "register_release_exposure_manifest_sql",
                "insert_release_source_sql",
                "insert_release_attachment_sql",
                "apply_release_exposure_sql",
                "after-members",
            ] {
                assert!(
                    source.contains(boundary),
                    "production writer misses {boundary}"
                );
            }
        }
        assert!(CATALOG_SCHEMA.contains("attachment-definition-not-current"));
        assert!(CATALOG_SCHEMA.contains("tombstoned-attachment-id"));
        assert!(
            CATALOG_SCHEMA
                .contains("activation.confirmed_definition_hash = definition.definition_hash")
        );
    }
}

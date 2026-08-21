// SPDX-License-Identifier: Apache-2.0

const PUBLISH_CATALOG_SOURCE: &str = include_str!("../../../services/ctl/src/publish_catalog.rs");
const CTL_MAIN_SOURCE: &str = include_str!("../../../services/ctl/src/main.rs");
const PROTECTED_RELATIONS_LIVE_SOURCE: &str =
    include_str!("../../../services/ctl/tests/protected_relations_live.rs");

#[test]
fn closed_publish_catalog_command_stays_deleted_without_collateral() {
    for retired in [
        "pub struct PublishCatalogArgs",
        "pub async fn run(args: PublishCatalogArgs)",
    ] {
        assert!(
            !PUBLISH_CATALOG_SOURCE.contains(retired),
            "publish_catalog.rs restored retired command surface {retired:?}",
        );
    }

    for (path, source) in [
        ("services/ctl/src/main.rs", CTL_MAIN_SOURCE),
        (
            "services/ctl/tests/protected_relations_live.rs",
            PROTECTED_RELATIONS_LIVE_SOURCE,
        ),
    ] {
        for retired in ["PublishCatalogArgs", "publish_catalog::run"] {
            assert!(
                !source.contains(retired),
                "{path} restored retired publish-catalog caller {retired:?}",
            );
        }
    }

    for survivor in [
        "pub async fn ensure_catalog_storage(",
        "pub async fn upsert_entity_map(",
    ] {
        assert!(
            PUBLISH_CATALOG_SOURCE.contains(survivor),
            "publish_catalog.rs lost live storage helper {survivor:?}",
        );
    }
}

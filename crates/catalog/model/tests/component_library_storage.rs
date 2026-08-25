//! Storage contract for byte-admitted component-library facts.

const PROJECT_SCHEMA: &str = include_str!("../../../../deploy/sql/catalog-schema.sql");
const CONTROL_SCHEMA: &str = include_str!("../../../../deploy/sql/control-portable-store.sql");
const RECONCILER: &str = include_str!("../../../../services/ctl/src/publish_catalog.rs");

#[test]
fn both_catalog_carriers_preserve_complete_component_facts() {
    for schema in [PROJECT_SCHEMA, CONTROL_SCHEMA] {
        let start = schema
            .find("catalog.component_library (")
            .expect("component library relation exists");
        let relation = &schema[start..];
        let relation = relation
            .split_once("\n);")
            .expect("component library relation terminates")
            .0;

        for field in [
            "tenant_id",
            "catalog_id",
            "catalog_version",
            "component",
            "interface_version",
            "operation",
            "component_digest",
            "imports",
            "imports_fingerprint",
            "effects",
            "input_ports",
            "output_ports",
            "parameters",
            "admitted_at",
        ] {
            assert!(relation.contains(field), "component fact loses {field}");
        }
        assert!(!relation.contains("environment"));
        assert!(relation.contains("component_library_one_operation_per_digest UNIQUE"));
        assert!(relation.contains("tenant_id, catalog_id, catalog_version, component_digest"));
    }
}

#[test]
fn project_storage_is_immutable_tenant_scoped_and_converged() {
    for required in [
        "ALTER TABLE catalog.component_library ENABLE ROW LEVEL SECURITY",
        "ALTER TABLE catalog.component_library FORCE ROW LEVEL SECURITY",
        "CREATE POLICY component_library_tenant ON catalog.component_library",
        "CREATE TRIGGER component_library_immutable",
        "BEFORE UPDATE OR DELETE ON catalog.component_library",
        "-- BEGIN COMPONENT LIBRARY STORAGE MIGRATION",
        "-- END COMPONENT LIBRARY STORAGE MIGRATION",
    ] {
        assert!(PROJECT_SCHEMA.contains(required), "missing {required:?}");
    }
    for required in [
        "to_regclass('catalog.component_library') IS NOT NULL",
        ".find(\"-- BEGIN COMPONENT LIBRARY STORAGE MIGRATION\")",
        ".find(\"-- END COMPONENT LIBRARY STORAGE MIGRATION\")",
        ".context(\"install component library storage\")",
    ] {
        assert!(RECONCILER.contains(required), "missing {required:?}");
    }
}

/// The effect projection reaches a library installed before it existed. The
/// relation-existence check that installs the base migration cannot add a
/// column, so the additive ALTER is its own converged block in both carriers.
#[test]
fn the_effect_projection_converges_onto_an_existing_component_library() {
    for required in [
        "-- BEGIN COMPONENT LIBRARY EFFECTS MIGRATION",
        "-- END COMPONENT LIBRARY EFFECTS MIGRATION",
        "ADD COLUMN IF NOT EXISTS effects jsonb NOT NULL DEFAULT '[]'::jsonb",
        "ALTER COLUMN effects DROP DEFAULT",
    ] {
        assert!(PROJECT_SCHEMA.contains(required), "missing {required:?}");
    }
    for required in [
        "ADD COLUMN IF NOT EXISTS effects jsonb NOT NULL DEFAULT '[]'::jsonb",
        "ALTER COLUMN effects DROP DEFAULT",
    ] {
        assert!(CONTROL_SCHEMA.contains(required), "missing {required:?}");
    }
    for required in [
        "AND column_name = 'effects')",
        ".find(\"-- BEGIN COMPONENT LIBRARY EFFECTS MIGRATION\")",
        ".context(\"install component library effect projection\")",
    ] {
        assert!(RECONCILER.contains(required), "missing {required:?}");
    }
}

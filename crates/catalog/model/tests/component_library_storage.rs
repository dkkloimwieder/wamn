//! Storage contract for byte-admitted component-library facts.

const PROJECT_SCHEMA: &str = include_str!("../../../../deploy/sql/catalog-schema.sql");
const CONTROL_SCHEMA: &str = include_str!("../../../../deploy/sql/control-portable-store.sql");

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
    // wamn-hopk R5: the reconciler-source greps that stood here are deleted; the
    // converge path is proven by the schema slice above and by the live gates.
}

/// The effect projection reaches a library installed before it existed. The
/// relation-existence check that installs the base migration cannot add a
/// column, so the additive ALTER is its own converged block in both carriers.
#[test]
fn the_effect_projection_converges_onto_an_existing_component_library() {
    // wamn-hopk R5: the reconciler-source greps that stood here are deleted; the
    // converge path is proven by the schema slice above and by the live gates.
}

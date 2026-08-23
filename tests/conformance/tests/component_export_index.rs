//! Cross-component fencing proof for the driver's node instances
//! (wamn-0h0g.17.8).
//!
//! The production driver stores the generated `bindings::Node` descriptor in
//! each `NodeInstance`; it does not keep a parallel export-name cache. Wasmtime
//! backs those generated descriptors with `ComponentExportIndex`. This proof
//! pins the security property that makes that representation safe: an index
//! minted by one compiled component cannot resolve against another component,
//! even when both export the same name.

use wamn_runtime::engine::build_engine;
use wash_runtime::engine::Engine;
use wash_runtime::wasmtime::component::Component;

fn component(engine: &Engine, marker: &str) -> Component {
    let bytes = wat::parse_str(format!(
        r#"
        (component
            (core module $guest
                (func (export "run"))
                (func (export "{marker}")))
            (core instance $guest (instantiate $guest))
            (func (export "run") (canon lift (core func $guest "run")))
            (func (export "{marker}") (canon lift (core func $guest "{marker}"))))
        "#
    ))
    .expect("encode the export-index fixture component");
    Component::new(engine.inner(), bytes).expect("compile the export-index fixture component")
}

#[test]
fn export_index_from_one_digest_does_not_resolve_against_another() {
    let engine = build_engine(&[]).expect("the production component engine");
    let component_a = component(&engine, "marker-a");
    let component_b = component(&engine, "marker-b");

    let run_from_a = component_a
        .get_export_index(None, "run")
        .expect("component A exports run");
    assert!(
        component_a.get_export_index(None, run_from_a).is_some(),
        "an export index resolves against the component that minted it"
    );
    assert!(
        component_b.get_export_index(None, "run").is_some(),
        "the shared export name exists on component B, so a name cache would resolve it"
    );
    assert!(
        component_b.get_export_index(None, run_from_a).is_none(),
        "a descriptor cached for component A crossed the component-digest boundary"
    );
}

#[test]
fn production_node_instances_keep_the_generated_typed_export_descriptor() {
    const DRIVER: &str = include_str!("../../../crates/execution/host/src/router_driver.rs");
    let node_instance = DRIVER
        .split("struct NodeInstance {")
        .nth(1)
        .and_then(|source| source.split("impl Drop for NodeInstance").next())
        .expect("production NodeInstance section");

    for required in [
        "node: bindings::Node",
        "bindings::Node::instantiate_async",
        ".wamn_node_handler()",
        ".call_run(",
    ] {
        assert!(
            node_instance.contains(required),
            "NodeInstance stopped using its typed export descriptor at {required:?}"
        );
    }
    for forbidden in ["get_export_index", "get_func(", "export_name"] {
        assert!(
            !node_instance.contains(forbidden),
            "NodeInstance added a name-keyed export path at {forbidden:?}"
        );
    }
}

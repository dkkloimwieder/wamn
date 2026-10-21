//! The consumer command is registered and owns the domain table.
#[path = "../../rest_connector/tests/support.rs"]
mod support;

use std::collections::BTreeSet;

#[test]
fn the_consumer_registers_its_array_command() {
    let package = support::repository().join("apps/rest_consumer_fixture");
    let admitted = support::admit(&package, "rest_consumer_fixture", None, BTreeSet::new());
    let operation = &admitted.facts().operations["rest-consumer-fixture:item/record@0.1.0"];
    assert_eq!(
        operation.registered_operation.as_deref(),
        Some("rest-consumer-fixture:item/record@0.1.0")
    );
    assert!(operation.host_transaction);
    assert_eq!(operation.statements.len(), 1);
    let manifest = support::manifest(&package);
    assert_eq!(manifest["models"]["item"]["owner"], "rest_consumer_fixture");
    assert_eq!(
        manifest["models"]["item"]["schema"],
        "rest_consumer_fixture"
    );
    assert_eq!(
        manifest["custom_operations"]["item.record"]["visibility"],
        "public"
    );
}

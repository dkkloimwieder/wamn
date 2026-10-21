//! Admit both applications and resolve their pull and push workflows in one release.
mod support;

use serde_json::json;
use std::collections::BTreeSet;
use wamn_catalog::{ComponentPackageScope, PackageCoordinate, WiringDocument};
use wamn_control::publish_release::{
    PublishReleaseRequest, ReleaseWiringTarget, assemble_local_release,
};
use wamn_schema_generator::operation_reference::{AuthoredDocument, read_authored_document};

#[test]
fn both_applications_publish_with_exact_dependencies_and_private_state() {
    let repository = support::repository();
    let integration = repository.join("apps/platform/wamn_integration");
    let consumer = repository.join("apps/rest_consumer_fixture");
    let connector = repository.join("apps/rest_connector");
    let roots = vec![integration.clone(), consumer.clone(), connector.clone()];
    let base = support::admit(&integration, "wamn_integration", None, BTreeSet::new());
    let destination = support::admit(&consumer, "rest_consumer_fixture", None, BTreeSet::new());
    let declared = support::manifest(&connector);
    for (alias, admitted) in [("integration", &base), ("consumer", &destination)] {
        assert_eq!(
            declared["base_dependencies"][alias]["version"],
            admitted.package_version()
        );
        assert_eq!(
            declared["base_dependencies"][alias]["digest"],
            admitted.component_digest()
        );
    }
    assert!(
        base.facts()
            .operations
            .values()
            .all(|op| op.registered_operation.is_none())
    );
    let component = support::admit(
        &connector,
        "rest_connector",
        None,
        BTreeSet::from([
            base.component_digest().into(),
            destination.component_digest().into(),
        ]),
    );
    assert!(
        component
            .facts()
            .operations
            .values()
            .all(|op| op.registered_operation.is_none())
    );
    assert_eq!(
        destination.facts().operations["rest-consumer-fixture:item/record@0.1.0"]
            .registered_operation
            .as_deref(),
        Some("rest-consumer-fixture:item/record@0.1.0")
    );
    let admissions = vec![
        base,
        destination,
        component,
        support::admit(
            &connector,
            "http-page",
            Some("apps/platform/no-std/http-page/declaration.json.in"),
            BTreeSet::new(),
        ),
        support::admit(
            &connector,
            "jsonata",
            Some("apps/platform/execution/jsonata/declaration.json.in"),
            BTreeSet::new(),
        ),
    ];
    let scope = ComponentPackageScope {
        tenant_id: "rest-publication".into(),
        package_id: "rest_connector".into(),
        package_version: "0.1.0".into(),
    };
    let documents: Vec<_> = ["rest_item", "rest_item_pushed"]
        .into_iter()
        .map(|name| {
            let document: WiringDocument = serde_json::from_value(
                read_authored_document(
                    &connector.join(format!("publication/wirings/{name}.json")),
                    AuthoredDocument::Wiring,
                )
                .unwrap(),
            )
            .unwrap();
            (scope.clone(), document)
        })
        .collect();
    let args = PublishReleaseRequest {
        database_url: String::new(),
        control_database_url: String::new(),
        org: "rest".into(),
        tenant: scope.tenant_id.clone(),
        environment: "test".into(),
        verified_publisher_principal: "publisher".into(),
        run_schema: "catalog".into(),
        packages: [
            "wamn_integration",
            "rest_connector",
            "rest_consumer_fixture",
        ]
        .into_iter()
        .map(|name| PackageCoordinate::new(name, "0.1.0").unwrap())
        .collect(),
        wirings: documents
            .iter()
            .map(|(_, document)| ReleaseWiringTarget {
                package_id: scope.package_id.clone(),
                package_version: scope.package_version.clone(),
                wiring_id: document.wiring_id.clone(),
                wiring_version: document.version,
            })
            .collect(),
    };
    let assembled = assemble_local_release(&args, &roots, &admissions, documents.clone()).unwrap();
    let release = &assembled.published.manifest;
    assert_eq!(release.release.packages.len(), 3);
    assert_eq!(release.workflow.schedules.len(), 1);
    assert_eq!(release.workflow.registrations.len(), 1);
    assert_eq!(
        release
            .workflow
            .registrations
            .values()
            .next()
            .unwrap()
            .source_package_id,
        "rest_consumer_fixture"
    );
    let pull = assembled
        .facts
        .wirings
        .iter()
        .find(|wiring| wiring.document.wiring_id == "rest_item")
        .unwrap();
    assert_eq!(
        pull.node_components["record"].scope.package_id,
        "rest_consumer_fixture"
    );
    assert_eq!(
        pull.document.nodes["page"].params["carry-input-pointer"],
        "/due"
    );
    assert_eq!(declared["models"]["setting"]["owner"], "wamn_integration");
    assert!(declared["models"].get("item").is_none());
    assert_eq!(
        support::manifest(&consumer)["custom_operations"]["item.record"]["permission"],
        "item.record"
    );
    let consumer_operation =
        &admissions[1].facts().operations["rest-consumer-fixture:item/record@0.1.0"];
    assert_eq!(
        consumer_operation.input_ports[0].schema.schema,
        json!({"type":"array"})
    );
    let mut wrong_port = documents.clone();
    wrong_port[0]
        .1
        .edges
        .iter_mut()
        .find(|edge| edge.from == "rows")
        .unwrap()
        .from_port = "main".into();
    assert!(assemble_local_release(&args, &roots, &admissions, wrong_port).is_err());
    let mut wrong_dependency = documents;
    wrong_dependency[0]
        .1
        .nodes
        .get_mut("record")
        .unwrap()
        .operation_dependency
        .as_mut()
        .unwrap()
        .operation = "item.other".into();
    assert!(assemble_local_release(&args, &roots, &admissions, wrong_dependency).is_err());
}

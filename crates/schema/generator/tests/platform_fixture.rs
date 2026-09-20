use std::collections::BTreeMap;

use wamn_schema_generator::client_ir::{ClientContractIr, leaf_fields};

#[path = "support/platform_fixture.rs"]
mod fixture;

#[test]
fn platform_fixture_generates_client_descriptors() {
    let package = fixture::generate_fixture();
    let client = ClientContractIr::from_release_contracts(
        "platform_fixture",
        &fixture::contracts(&package),
        &BTreeMap::new(),
    )
    .expect("generated contracts project into client descriptors");

    let widget = client
        .models
        .iter()
        .find(|model| model.name == "widget")
        .expect("widget descriptors");
    assert_eq!(
        widget
            .operations
            .iter()
            .map(|operation| operation.name.as_str())
            .collect::<Vec<_>>(),
        ["archive", "create", "delete", "get", "query", "update"]
    );

    let fields = leaf_fields(&widget.fields);
    let note = fields
        .iter()
        .find(|field| field.path == "note")
        .expect("nullable note descriptor");
    assert!(note.nullable);
    let revision = fields
        .iter()
        .find(|field| field.path == "edit_version")
        .expect("revision descriptor");
    assert!(revision.revision);

    let archive = widget
        .operations
        .iter()
        .find(|operation| operation.name == "archive")
        .expect("archive descriptor");
    assert!(
        archive
            .errors
            .iter()
            .any(|error| error.literal == "already_archived")
    );
    assert!(
        archive
            .errors
            .iter()
            .any(|error| error.literal == "permission_denied")
    );
}

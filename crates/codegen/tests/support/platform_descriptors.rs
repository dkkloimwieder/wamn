use wamn_codegen::client_ir::{ClientContractIr, leaf_fields};

use super::fixture;

#[test]
fn platform_fixture_generates_client_descriptors() {
    let package = fixture::generate_fixture();
    let client: ClientContractIr = fixture::client_release_of(&package);

    let widget = client
        .models
        .iter()
        .find(|model| model.name == "widget")
        .expect("widget descriptors");
    let expected: Vec<_> = package
        .operations()
        .iter()
        .filter(|(_, description)| {
            description.operation.type_ != wamn_catalog::OperationType::EventHandler
                && description.operation.visibility.as_deref() != Some("private")
        })
        .filter_map(|(local, _)| local.as_str().strip_prefix("widget."))
        .collect();
    assert_eq!(
        widget
            .operations
            .iter()
            .map(|operation| operation.name.as_str())
            .collect::<Vec<_>>(),
        expected
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
    assert!(revision.revision.is_revision());

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

//! Semantic proof for the Acme overlay publication inputs that close independently.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_catalog::{ComponentDeclaration, WiringDocument, WiringTerminal};

const TENANT: &str = "acme-overlay-publication-proof";
const PACKAGE_ID: &str = "client_acme_receiving";
const PACKAGE_VERSION: &str = "3.0.0";
const COMPONENT: &str = "client_acme_receiving";
const INTERFACE_VERSION: &str = "0.1.0";
const PRIVATE_OPERATION: &str = "client-acme-receiving:quality/create-inspection@3.0.0";
const DIRECT_OPERATIONS: [(&str, &str); 5] = [
    (
        "purchase_order_get",
        "client-acme-receiving:purchase-order/get@3.0.0",
    ),
    (
        "purchase_order_update",
        "client-acme-receiving:purchase-order/update@3.0.0",
    ),
    (
        "receiving_record_receipt",
        "client-acme-receiving:receiving/record-receipt@3.0.0",
    ),
    (
        "quality_load_purchase_order_detail",
        "client-acme-receiving:quality/load-purchase-order-detail@3.0.0",
    ),
    (
        "quality_approve_inspection",
        "client-acme-receiving:quality/approve-inspection@3.0.0",
    ),
];

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn package_root() -> PathBuf {
    repository_root().join("packages/client_acme_receiving")
}

fn publication_root() -> PathBuf {
    package_root().join("publication")
}

fn read_json(path: &Path) -> Value {
    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn declaration() -> ComponentDeclaration {
    let path = publication_root()
        .join("components")
        .join("client_acme_receiving.json.in");
    let mut document = read_json(&path);
    document["scope"]["tenant-id"] = Value::String(TENANT.to_owned());
    serde_json::from_value(document)
        .unwrap_or_else(|error| panic!("decode {}: {error}", path.display()))
}

fn wiring(name: &str) -> WiringDocument {
    let path = publication_root()
        .join("wirings")
        .join(format!("{name}.json"));
    WiringDocument::parse(&read_json(&path))
        .unwrap_or_else(|error| panic!("decode {}: {error}", path.display()))
}

#[test]
fn acme_direct_operations_and_private_handler_have_exact_publication_inputs() {
    let declaration = declaration();
    assert_eq!(declaration.scope.tenant_id, TENANT);
    assert_eq!(declaration.scope.package_id, PACKAGE_ID);
    assert_eq!(declaration.scope.package_version, PACKAGE_VERSION);
    assert_eq!(declaration.component, COMPONENT);
    assert_eq!(declaration.interface_version, INTERFACE_VERSION);
    assert_eq!(
        declaration
            .operations
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        DIRECT_OPERATIONS
            .iter()
            .map(|(_, operation)| *operation)
            .chain([PRIVATE_OPERATION])
            .collect::<BTreeSet<_>>()
    );

    for (wiring_id, operation) in DIRECT_OPERATIONS {
        let fact = &declaration.operations[operation];
        assert_eq!(fact.registered_operation.as_deref(), Some(operation));
        let document = wiring(wiring_id);
        assert_eq!(document.wiring_id, wiring_id);
        assert_eq!(document.version, 1);
        assert_eq!(document.nodes.len(), 1);
        assert!(document.edges.is_empty());
        assert!(document.cases.is_empty());
        let node = &document.nodes[&document.entry];
        assert_eq!(node.component, COMPONENT);
        assert_eq!(node.interface_version, INTERFACE_VERSION);
        assert_eq!(node.operation, operation);
        assert!(node.operation_dependency.is_none());
        assert_eq!(node.terminal, Some(WiringTerminal::Respond));
    }

    let private_fact = &declaration.operations[PRIVATE_OPERATION];
    assert!(
        private_fact.registered_operation.is_none(),
        "the private handler must not fabricate an originating caller permission"
    );
    let handler = wiring("quality_create_inspection");
    assert_eq!(handler.wiring_id, "quality_create_inspection");
    assert_eq!(handler.version, 1);
    assert_eq!(handler.nodes.len(), 1);
    assert!(handler.edges.is_empty());
    assert!(handler.cases.is_empty());
    let entry = &handler.nodes[&handler.entry];
    assert_eq!(entry.component, COMPONENT);
    assert_eq!(entry.interface_version, INTERFACE_VERSION);
    assert_eq!(entry.operation, PRIVATE_OPERATION);
    assert!(entry.operation_dependency.is_none());
    assert!(
        entry.terminal.is_none(),
        "the callerless event handler must not fabricate a response terminal"
    );
}

#[test]
fn receipt_insert_registration_selects_one_private_owner_wiring() {
    let manifest = read_json(&package_root().join("wamn.json"));
    let handler = &manifest["custom_operations"]["quality.create_inspection"];
    assert_eq!(handler["kind"], "event_handler");
    assert_eq!(handler["visibility"], "private");
    assert!(handler["permission"].is_null());
    assert_eq!(
        handler["registration"],
        serde_json::json!({
            "source_package": "wamn_receiving",
            "entity": "receipt",
            "ops": ["insert"]
        })
    );
    let generated = read_json(
        &package_root().join("generated/contracts/quality/create_inspection.operation.json"),
    );
    assert!(generated["grant"].is_null());
    assert!(generated["permission_token"].is_null());
    assert_eq!(generated["operation"], PRIVATE_OPERATION);

    let mut owner_entries = Vec::new();
    let directory = publication_root().join("wirings");
    for entry in std::fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
    {
        let path = entry.expect("read wiring directory entry").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let document = WiringDocument::parse(&read_json(&path))
            .unwrap_or_else(|error| panic!("decode {}: {error}", path.display()));
        let node = &document.nodes[&document.entry];
        if node.component == COMPONENT && node.operation == PRIVATE_OPERATION {
            owner_entries.push((document.wiring_id, document.version));
        }
    }
    assert_eq!(
        owner_entries,
        vec![("quality_create_inspection".to_owned(), 1)],
        "release registration derivation requires one exact owner entry wiring"
    );
}

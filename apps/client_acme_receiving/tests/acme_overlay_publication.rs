//! Semantic test for the Acme overlay publication inputs that close independently.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_catalog::{
    AttachmentKind, AttachmentTarget, ComponentDeclaration, ComponentOperationDependency,
    WiringDocument,
};
use wamn_test_infrastructure::operations::{package_version, sealed};

const TENANT: &str = "acme-overlay-publication-test";
const PACKAGE_ID: &str = "client_acme_receiving";
const COMPONENT: &str = "client_acme_receiving";
const INTERFACE_VERSION: &str = "0.1.0";
// Each operation is named by reference, as the authored files name it, and
// `sealed` reads its version from the package wamn.json.
const PRIVATE_OPERATION: &str = "client-acme-receiving:quality/create-inspection";
const PARTICIPANT_OPERATION: &str = "client-acme-receiving:receiving/record-receipt-participant";
const RECORD_RECEIPT: &str = "client-acme-receiving:receiving/record-receipt";
const BASE_RECORD_RECEIPT: &str = "wamn-receiving:receiving/record-receipt";
const RAW_BODY_MAXIMUM: u64 = 1_048_576;
struct DirectOperation {
    token: &'static str,
    attachment: &'static str,
    route: &'static str,
}

const DIRECT_OPERATIONS: [DirectOperation; 5] = [
    DirectOperation {
        token: "client-acme-receiving:purchase-order/get",
        attachment: "client-acme-receiving-purchase-order-get-http",
        route: "/acme/purchase_order/get",
    },
    DirectOperation {
        token: "client-acme-receiving:purchase-order/update",
        attachment: "client-acme-receiving-purchase-order-update-http",
        route: "/acme/purchase_order/update",
    },
    DirectOperation {
        token: "client-acme-receiving:receiving/record-receipt",
        attachment: "client-acme-receiving-receiving-record-receipt-http",
        route: "/acme/receiving/record_receipt",
    },
    DirectOperation {
        token: "client-acme-receiving:quality/load-purchase-order-detail",
        attachment: "client-acme-receiving-quality-load-purchase-order-detail-http",
        route: "/acme/quality/load_purchase_order_detail",
    },
    DirectOperation {
        token: "client-acme-receiving:quality/approve-inspection",
        attachment: "client-acme-receiving-quality-approve-inspection-http",
        route: "/acme/quality/approve_inspection",
    },
];

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn package_root() -> PathBuf {
    repository_root().join("apps/client_acme_receiving")
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

/// The base component digest, read from the ONE file that authors it.
///
/// wamn-10yt.50: this test used to restate the same `sha256:` literal the
/// overlay manifest pins, and the template carried a third copy. The template
/// now leaves a placeholder, so the render is what makes it a declaration.
fn base_digests() -> BTreeMap<Box<str>, Box<str>> {
    wamn_control::component_declaration::authored_base_digests(&package_root())
        .expect("the overlay manifest authors its base digest")
}

fn declaration() -> ComponentDeclaration {
    let path = publication_root()
        .join("components")
        .join("client_acme_receiving.json.in");
    let document = wamn_control::component_declaration::render_declaration_document(
        &path,
        TENANT,
        &base_digests(),
    )
    .unwrap_or_else(|error| panic!("render {}: {error}", path.display()));
    serde_json::from_value(document)
        .unwrap_or_else(|error| panic!("decode {}: {error}", path.display()))
}

fn wiring(name: &str) -> WiringDocument {
    let path = publication_root()
        .join("wirings")
        .join(format!("{name}.json"));
    WiringDocument::parse(&authored_wiring(&path))
        .unwrap_or_else(|error| panic!("decode {}: {error}", path.display()))
}

/// One authored wiring, with its operation references resolved.
fn authored_wiring(path: &Path) -> serde_json::Value {
    wamn_schema_generator::operation_reference::read_authored_document(
        path,
        wamn_schema_generator::operation_reference::AuthoredDocument::Wiring,
    )
    .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn acme_direct_operations_and_private_handler_have_exact_publication_inputs() {
    let declaration = declaration();
    // Resolved, as the rendered declaration is: both state the served schema.
    let attachments: BTreeMap<String, wamn_catalog::ServingAttachment> =
        wamn_schema_generator::route_schema::read_package_attachments(&package_root())
            .expect("the attachment map has the serving wire shape");
    assert_eq!(attachments.len(), DIRECT_OPERATIONS.len());
    assert_eq!(declaration.scope.tenant_id, TENANT);
    assert_eq!(declaration.scope.package_id, PACKAGE_ID);
    assert_eq!(
        declaration.scope.package_version,
        package_version(PACKAGE_ID)
    );
    assert_eq!(declaration.component, COMPONENT);
    assert_eq!(declaration.interface_version, INTERFACE_VERSION);
    assert_eq!(
        declaration
            .operations
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        DIRECT_OPERATIONS
            .iter()
            .map(|operation| operation.token)
            .chain([PRIVATE_OPERATION, PARTICIPANT_OPERATION])
            .map(sealed)
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(
        declaration.operations[&sealed(PARTICIPANT_OPERATION)].registered_operation,
        Some(sealed(PARTICIPANT_OPERATION))
    );

    let base_version = package_version("wamn_receiving");
    for operation in &DIRECT_OPERATIONS {
        let token = sealed(operation.token);
        let fact = &declaration.operations[&token];
        assert_eq!(fact.registered_operation.as_deref(), Some(token.as_str()));
        let expected_dependencies = (operation.token == RECORD_RECEIPT)
            .then(|| ComponentOperationDependency {
                package: "wamn_receiving".to_owned(),
                version: base_version.clone(),
                digest: base_digests()[format!("wamn_receiving@{base_version}").as_str()]
                    .to_string(),
                operation: sealed(BASE_RECORD_RECEIPT),
                participant: Some(sealed(PARTICIPANT_OPERATION)),
            })
            .into_iter()
            .collect::<Vec<_>>();
        assert_eq!(fact.dependencies, expected_dependencies);
        let attachment = attachments
            .get(operation.attachment)
            .expect("the exact operation attachment exists");
        assert_eq!(attachment.kind, AttachmentKind::Http);
        assert_eq!(attachment.package_id, PACKAGE_ID);
        assert_eq!(
            attachment.target,
            AttachmentTarget::Route {
                component: COMPONENT.into(),
                operation: token.clone(),
            }
        );
        assert_eq!(
            attachment.registered_operation.as_deref(),
            Some(token.as_str())
        );
        assert_eq!(
            attachment.auth_policy,
            serde_json::json!({"modes": ["pat", "session"]})
        );
        assert_eq!(attachment.definition["id"], operation.attachment);
        assert_eq!(attachment.definition["kind"], "http");
        assert!(
            attachment.definition["route"].get("host").is_none(),
            "package attachments leave route hostnames to the deployment overlay"
        );
        // No author writes a method: publish derives it from the operation kind.
        assert!(attachment.definition["route"].get("method").is_none());
        assert_eq!(attachment.definition["route"]["path"], operation.route);
        assert_eq!(
            attachment.definition["raw-body-bytes"]["maximum"],
            RAW_BODY_MAXIMUM
        );
        assert_eq!(
            attachment.definition["input-schema"],
            fact.input_ports[0].schema
        );
        assert_eq!(
            attachment.definition_hash.as_str(),
            wamn_execution_contract::canonical_json_sha256(&attachment.definition)
        );
    }

    let private_fact = &declaration.operations[&sealed(PRIVATE_OPERATION)];
    assert!(
        private_fact.registered_operation.is_none(),
        "the private handler must not fabricate an originating caller permission"
    );
    assert!(private_fact.dependencies.is_empty());
    let handler = wiring("quality_create_inspection");
    assert_eq!(handler.wiring_id, "quality_create_inspection");
    assert_eq!(handler.version, 1);
    assert_eq!(handler.nodes.len(), 1);
    assert!(handler.edges.is_empty());
    assert!(handler.cases.is_empty());
    let entry = &handler.nodes[&handler.entry];
    assert_eq!(entry.component, COMPONENT);
    assert_eq!(entry.interface_version, INTERFACE_VERSION);
    assert_eq!(entry.operation, sealed(PRIVATE_OPERATION));
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
    let mut owner_entries = Vec::new();
    let directory = publication_root().join("wirings");
    for entry in std::fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
    {
        let path = entry.expect("read wiring directory entry").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let document = WiringDocument::parse(&authored_wiring(&path))
            .unwrap_or_else(|error| panic!("decode {}: {error}", path.display()));
        let node = &document.nodes[&document.entry];
        if node.component == COMPONENT && node.operation == sealed(PRIVATE_OPERATION) {
            owner_entries.push((document.wiring_id, document.version));
        }
    }
    assert_eq!(
        owner_entries,
        vec![("quality_create_inspection".to_owned(), 1)],
        "release registration derivation requires one exact owner entry wiring"
    );
}

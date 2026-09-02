//! Semantic proof for the Acme overlay publication inputs that close independently.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_catalog::{
    AttachmentKind, ComponentDeclaration, ComponentOperationDependency, WiringDocument,
    WiringTerminal,
};

const TENANT: &str = "acme-overlay-publication-proof";
const PACKAGE_ID: &str = "client_acme_receiving";
const PACKAGE_VERSION: &str = "3.0.0";
const COMPONENT: &str = "client_acme_receiving";
const INTERFACE_VERSION: &str = "0.1.0";
const PRIVATE_OPERATION: &str = "client-acme-receiving:quality/create-inspection@3.0.0";
const BASE_RECORD_RECEIPT: &str = "wamn-receiving:receiving/record-receipt@1.0.0";
const RAW_BODY_MAXIMUM: u64 = 1_048_576;
const BASE_COMPONENT_DIGEST: &str =
    "sha256:68d81d2d0b895aaafbe7cd952974377c65801dcf0ab68db42b9298e94adaef3e";
struct DirectOperation {
    wiring: &'static str,
    token: &'static str,
    attachment: &'static str,
    route: &'static str,
}

const DIRECT_OPERATIONS: [DirectOperation; 5] = [
    DirectOperation {
        wiring: "purchase_order_get",
        token: "client-acme-receiving:purchase-order/get@3.0.0",
        attachment: "client-acme-receiving-purchase-order-get-http",
        route: "/acme/purchase_order/get",
    },
    DirectOperation {
        wiring: "purchase_order_update",
        token: "client-acme-receiving:purchase-order/update@3.0.0",
        attachment: "client-acme-receiving-purchase-order-update-http",
        route: "/acme/purchase_order/update",
    },
    DirectOperation {
        wiring: "receiving_record_receipt",
        token: "client-acme-receiving:receiving/record-receipt@3.0.0",
        attachment: "client-acme-receiving-receiving-record-receipt-http",
        route: "/acme/receiving/record_receipt",
    },
    DirectOperation {
        wiring: "quality_load_purchase_order_detail",
        token: "client-acme-receiving:quality/load-purchase-order-detail@3.0.0",
        attachment: "client-acme-receiving-quality-load-purchase-order-detail-http",
        route: "/acme/quality/load_purchase_order_detail",
    },
    DirectOperation {
        wiring: "quality_approve_inspection",
        token: "client-acme-receiving:quality/approve-inspection@3.0.0",
        attachment: "client-acme-receiving-quality-approve-inspection-http",
        route: "/acme/quality/approve_inspection",
    },
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
    let attachments: BTreeMap<String, wamn_catalog::ServingAttachment> =
        serde_json::from_value(read_json(&publication_root().join("attachments.json")))
            .expect("the attachment map has the serving wire shape");
    assert_eq!(attachments.len(), DIRECT_OPERATIONS.len());
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
            .map(|operation| operation.token)
            .chain([PRIVATE_OPERATION])
            .collect::<BTreeSet<_>>()
    );

    for operation in &DIRECT_OPERATIONS {
        let fact = &declaration.operations[operation.token];
        assert_eq!(fact.registered_operation.as_deref(), Some(operation.token));
        let expected_dependencies = (operation.token
            == "client-acme-receiving:receiving/record-receipt@3.0.0")
            .then(|| ComponentOperationDependency {
                package: "wamn_receiving".to_owned(),
                version: "1.0.0".to_owned(),
                digest: BASE_COMPONENT_DIGEST.to_owned(),
                operation: BASE_RECORD_RECEIPT.to_owned(),
            })
            .into_iter()
            .collect::<Vec<_>>();
        assert_eq!(fact.dependencies, expected_dependencies);
        let document = wiring(operation.wiring);
        assert_eq!(document.wiring_id, operation.wiring);
        assert_eq!(document.version, 1);
        assert_eq!(document.nodes.len(), 1);
        assert!(document.edges.is_empty());
        assert!(document.cases.is_empty());
        let node = &document.nodes[&document.entry];
        assert_eq!(node.component, COMPONENT);
        assert_eq!(node.interface_version, INTERFACE_VERSION);
        assert_eq!(node.operation, operation.token);
        assert!(node.operation_dependency.is_none());
        assert_eq!(node.terminal, Some(WiringTerminal::Respond));

        let attachment = attachments
            .get(operation.attachment)
            .expect("the exact operation attachment exists");
        assert_eq!(attachment.kind, AttachmentKind::Http);
        assert_eq!(attachment.package_id, PACKAGE_ID);
        assert_eq!(attachment.wiring_id, operation.wiring);
        assert_eq!(attachment.wiring_version, 1);
        assert_eq!(
            attachment.registered_operation.as_deref(),
            Some(operation.token)
        );
        assert_eq!(attachment.auth_policy, serde_json::json!({"mode": "pat"}));
        assert_eq!(attachment.definition["id"], operation.attachment);
        assert_eq!(attachment.definition["kind"], "http");
        assert!(
            attachment.definition["route"].get("host").is_none(),
            "package attachments leave route hostnames to the deployment overlay"
        );
        assert_eq!(attachment.definition["route"]["method"], "POST");
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

    let private_fact = &declaration.operations[PRIVATE_OPERATION];
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

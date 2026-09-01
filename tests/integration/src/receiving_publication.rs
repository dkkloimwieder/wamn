//! Semantic gate for the package-owned Receiving publication inputs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_catalog::{AttachmentKind, ComponentDeclaration, WiringDocument, WiringTerminal};

const TENANT: &str = "receiving-publication-proof";
const PACKAGE_ID: &str = "wamn_receiving";
const PACKAGE_VERSION: &str = "1.0.0";
const INTERFACE_VERSION: &str = "0.1.0";
const RAW_BODY_MAXIMUM: u64 = 1_048_576;

struct Operation {
    component: &'static str,
    local: &'static str,
    attachment: &'static str,
    route: &'static str,
}

const OPERATIONS: [Operation; 6] = [
    Operation {
        component: "purchase_order_get",
        local: "purchase_order.get",
        attachment: "purchase-order-get-http",
        route: "/purchase_order/get",
    },
    Operation {
        component: "purchase_order_query",
        local: "purchase_order.query",
        attachment: "purchase-order-query-http",
        route: "/purchase_order/query",
    },
    Operation {
        component: "purchase_order_update",
        local: "purchase_order.update",
        attachment: "purchase-order-update-http",
        route: "/purchase_order/update",
    },
    Operation {
        component: "receipt_get",
        local: "receipt.get",
        attachment: "receipt-get-http",
        route: "/receipt/get",
    },
    Operation {
        component: "receipt_query",
        local: "receipt.query",
        attachment: "receipt-query-http",
        route: "/receipt/query",
    },
    Operation {
        component: "receiving_record_receipt",
        local: "receiving.record_receipt",
        attachment: "receiving-record-receipt-http",
        route: "/receiving/record_receipt",
    },
];

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn publication_root() -> PathBuf {
    repository_root().join("packages/receiving/publication")
}

fn read_json(path: &Path) -> Value {
    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn declaration(operation: &Operation) -> ComponentDeclaration {
    let path = publication_root()
        .join("components")
        .join(format!("{}.json.in", operation.component));
    let mut document = read_json(&path);
    document["scope"]["tenant-id"] = Value::String(TENANT.to_owned());
    serde_json::from_value(document)
        .unwrap_or_else(|error| panic!("decode {}: {error}", path.display()))
}

fn wiring(operation: &Operation) -> WiringDocument {
    let path = publication_root()
        .join("wirings")
        .join(format!("{}.json", operation.component));
    WiringDocument::parse(&read_json(&path))
        .unwrap_or_else(|error| panic!("decode {}: {error}", path.display()))
}

fn canonical_operation(local: &str) -> String {
    format!("{PACKAGE_ID}@{PACKAGE_VERSION}::{local}")
}

#[test]
fn package_owned_inputs_declare_the_exact_six_route_closure() {
    let package_manifest = read_json(&repository_root().join("packages/receiving/wamn.json"));
    let attachments: BTreeMap<String, wamn_catalog::ServingAttachment> =
        serde_json::from_value(read_json(&publication_root().join("attachments.json")))
            .expect("the attachment map has the serving wire shape");
    assert_eq!(attachments.len(), OPERATIONS.len());

    for operation in &OPERATIONS {
        let declaration = declaration(operation);
        let registered_operation = canonical_operation(operation.local);
        assert_eq!(declaration.scope.tenant_id, TENANT);
        assert_eq!(declaration.scope.package_id, PACKAGE_ID);
        assert_eq!(declaration.scope.package_version, PACKAGE_VERSION);
        assert_eq!(declaration.component, operation.component);
        assert_eq!(declaration.interface_version, INTERFACE_VERSION);
        assert_eq!(declaration.operation, "run");
        assert_eq!(
            declaration.registered_operation.as_deref(),
            Some(registered_operation.as_str())
        );
        assert_eq!(declaration.input_ports.len(), 1);
        assert_eq!(declaration.output_ports.len(), 1);
        assert!(declaration.parameters.is_empty());
        assert!(declaration.connections.is_empty());
        assert_eq!(
            package_manifest["components"][operation.component]["operations"],
            serde_json::json!([operation.local])
        );

        let document = wiring(operation);
        assert_eq!(document.wiring_id, operation.component);
        assert_eq!(document.version, 1);
        assert_eq!(document.entry, "operation");
        assert!(document.edges.is_empty());
        assert!(document.cases.is_empty());
        let node = document.nodes.get("operation").expect("one entry node");
        assert_eq!(document.nodes.len(), 1);
        assert_eq!(node.component, operation.component);
        assert_eq!(node.interface_version, INTERFACE_VERSION);
        assert_eq!(node.operation, "run");
        assert_eq!(node.terminal, Some(WiringTerminal::Respond));

        let attachment = attachments
            .get(operation.attachment)
            .expect("the exact operation attachment exists");
        assert_eq!(attachment.kind, AttachmentKind::Http);
        assert_eq!(attachment.package_id, PACKAGE_ID);
        assert_eq!(attachment.wiring_id, operation.component);
        assert_eq!(attachment.wiring_version, 1);
        assert_eq!(
            attachment.registered_operation.as_deref(),
            Some(registered_operation.as_str())
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
            declaration.input_ports[0].schema
        );
        assert_eq!(
            attachment.definition_hash.as_str(),
            wamn_execution_contract::canonical_json_sha256(&attachment.definition)
        );
    }
}

//! Semantic gate for the package-owned Receiving publication inputs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_catalog::{AttachmentKind, AttachmentTarget, ComponentDeclaration};

const TENANT: &str = "receiving-publication-test";
const PACKAGE_ID: &str = "wamn_receiving";
const PACKAGE_VERSION: &str = "1.0.0";
const COMPONENT: &str = "receiving";
const INTERFACE_VERSION: &str = "0.1.0";
const RAW_BODY_MAXIMUM: u64 = 1_048_576;

struct Operation {
    token: &'static str,
    attachment: &'static str,
    route: &'static str,
}

const OPERATIONS: [Operation; 11] = [
    Operation {
        token: "wamn-receiving:purchase-order/get@1.0.0",
        attachment: "purchase-order-get-http",
        route: "/purchase_order/get",
    },
    Operation {
        token: "wamn-receiving:purchase-order/query@1.0.0",
        attachment: "purchase-order-query-http",
        route: "/purchase_order/query",
    },
    Operation {
        token: "wamn-receiving:purchase-order/update@1.0.0",
        attachment: "purchase-order-update-http",
        route: "/purchase_order/update",
    },
    Operation {
        token: "wamn-receiving:receipt/get@1.0.0",
        attachment: "receipt-get-http",
        route: "/receipt/get",
    },
    Operation {
        token: "wamn-receiving:receipt/query@1.0.0",
        attachment: "receipt-query-http",
        route: "/receipt/query",
    },
    Operation {
        token: "wamn-receiving:receiving/load-receipt-screen@1.0.0",
        attachment: "receiving-load-receipt-screen-http",
        route: "/receiving/load_receipt_screen",
    },
    Operation {
        token: "wamn-receiving:receiving/load-purchase-order-history@1.0.0",
        attachment: "receiving-load-purchase-order-history-http",
        route: "/receiving/load_purchase_order_history",
    },
    Operation {
        token: "wamn-receiving:location/list@1.0.0",
        attachment: "location-list-http",
        route: "/location/list",
    },
    Operation {
        token: "wamn-receiving:receiving/record-receipt@1.0.0",
        attachment: "receiving-record-receipt-http",
        route: "/receiving/record_receipt",
    },
    Operation {
        token: "wamn-receiving:supplier/query@1.0.0",
        attachment: "supplier-query-http",
        route: "/supplier/query",
    },
    Operation {
        token: "wamn-receiving:supplier/create@1.0.0",
        attachment: "supplier-create-http",
        route: "/supplier/create",
    },
];

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn publication_root() -> PathBuf {
    repository_root().join("apps/wamn_receiving/publication")
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
        .join("receiving.json.in");
    let mut document = read_json(&path);
    document["scope"]["tenant-id"] = Value::String(TENANT.to_owned());
    serde_json::from_value(document)
        .unwrap_or_else(|error| panic!("decode {}: {error}", path.display()))
}

#[test]
fn package_owned_inputs_declare_the_exact_eleven_route_closure() {
    let package_manifest = read_json(&repository_root().join("apps/wamn_receiving/wamn.json"));
    let attachments: BTreeMap<String, wamn_catalog::ServingAttachment> =
        serde_json::from_value(read_json(&publication_root().join("attachments.json")))
            .expect("the attachment map has the serving wire shape");
    assert_eq!(attachments.len(), OPERATIONS.len());

    let declaration = declaration();
    assert_eq!(declaration.scope.tenant_id, TENANT);
    assert_eq!(declaration.scope.package_id, PACKAGE_ID);
    assert_eq!(declaration.scope.package_version, PACKAGE_VERSION);
    assert_eq!(declaration.component, COMPONENT);
    assert_eq!(declaration.interface_version, INTERFACE_VERSION);
    assert!(declaration.connections.is_empty());
    assert_eq!(
        declaration
            .operations
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        OPERATIONS
            .iter()
            .map(|operation| operation.token)
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(
        package_manifest["components"],
        serde_json::json!({(COMPONENT): {"connections": ["postgres"]}})
    );

    for operation in &OPERATIONS {
        let fact = declaration
            .operations
            .get(operation.token)
            .expect("the component declares the exact operation token");
        assert_eq!(
            fact.registered_operation.as_deref(),
            Some(operation.token),
            "the redundant authorization identity must equal its export selector"
        );
        assert_eq!(fact.input_ports.len(), 1);
        assert_eq!(fact.output_ports.len(), 1);
        assert!(fact.parameters.is_empty());

        let attachment = attachments
            .get(operation.attachment)
            .expect("the exact operation attachment exists");
        assert_eq!(attachment.kind, AttachmentKind::Http);
        assert_eq!(attachment.package_id, PACKAGE_ID);
        assert_eq!(
            attachment.target,
            AttachmentTarget::Route {
                component: COMPONENT.into(),
                operation: operation.token.into(),
            }
        );
        assert_eq!(
            attachment.registered_operation.as_deref(),
            Some(operation.token)
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
}

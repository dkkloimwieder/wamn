//! Semantic gate for the package-owned WMS publication inputs.
//!
//! The same test `receiving_publication` makes, for the second package. It is
//! written rather than generalized: two packages is where a shape starts to
//! look reusable, and the toolkit-promotion rule says the third is where it is
//! promoted, not the second.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_catalog::{AttachmentKind, AttachmentTarget, ComponentDeclaration};

const TENANT: &str = "wms-publication-test";
const PACKAGE_ID: &str = "wamn_wms";
const COMPONENT: &str = "wms";
const INTERFACE_VERSION: &str = "0.1.0";
const RAW_BODY_MAXIMUM: u64 = 1_048_576;

struct Operation {
    token: &'static str,
    attachment: &'static str,
    route: &'static str,
}

// RULED wamn-362o.39: A PACKAGE DECLARES WHAT IT SHIPS. Admission compares
// the declaration against the bytes and refuses any difference, so this
// table, the declaration and attachments.json widen together --
// wamn-362o.10 grew the guest from one operation to seven and widened all
// four in one commit.
const OPERATIONS: [Operation; 21] = [
    Operation {
        token: "wamn-wms:location/get@1.0.0",
        attachment: "location-get-http",
        route: "/location/get",
    },
    Operation {
        token: "wamn-wms:location/query@1.0.0",
        attachment: "location-query-http",
        route: "/location/query",
    },
    Operation {
        token: "wamn-wms:location/create@1.0.0",
        attachment: "location-create-http",
        route: "/location/create",
    },
    Operation {
        token: "wamn-wms:location/update@1.0.0",
        attachment: "location-update-http",
        route: "/location/update",
    },
    Operation {
        token: "wamn-wms:product/get@1.0.0",
        attachment: "product-get-http",
        route: "/product/get",
    },
    Operation {
        token: "wamn-wms:product/query@1.0.0",
        attachment: "product-query-http",
        route: "/product/query",
    },
    Operation {
        token: "wamn-wms:product/create@1.0.0",
        attachment: "product-create-http",
        route: "/product/create",
    },
    Operation {
        token: "wamn-wms:product/update@1.0.0",
        attachment: "product-update-http",
        route: "/product/update",
    },
    Operation {
        token: "wamn-wms:packaging/get@1.0.0",
        attachment: "packaging-get-http",
        route: "/packaging/get",
    },
    Operation {
        token: "wamn-wms:packaging/query@1.0.0",
        attachment: "packaging-query-http",
        route: "/packaging/query",
    },
    Operation {
        token: "wamn-wms:inventory/get@1.0.0",
        attachment: "inventory-get-http",
        route: "/inventory/get",
    },
    Operation {
        token: "wamn-wms:inventory/query@1.0.0",
        attachment: "inventory-query-http",
        route: "/inventory/query",
    },
    Operation {
        token: "wamn-wms:inventory-transaction/get@1.0.0",
        attachment: "inventory-transaction-get-http",
        route: "/inventory_transaction/get",
    },
    Operation {
        token: "wamn-wms:inventory-transaction/query@1.0.0",
        attachment: "inventory-transaction-query-http",
        route: "/inventory_transaction/query",
    },
    Operation {
        token: "wamn-wms:inventory/move@1.0.0",
        attachment: "inventory-move-http",
        route: "/inventory/move",
    },
    Operation {
        token: "wamn-wms:inventory/adjust@1.0.0",
        attachment: "inventory-adjust-http",
        route: "/inventory/adjust",
    },
    Operation {
        token: "wamn-wms:inventory/split@1.0.0",
        attachment: "inventory-split-http",
        route: "/inventory/split",
    },
    Operation {
        token: "wamn-wms:inventory/merge@1.0.0",
        attachment: "inventory-merge-http",
        route: "/inventory/merge",
    },
    Operation {
        token: "wamn-wms:packaging/create@1.0.0",
        attachment: "packaging-create-http",
        route: "/packaging/create",
    },
    Operation {
        token: "wamn-wms:packaging/close@1.0.0",
        attachment: "packaging-close-http",
        route: "/packaging/close",
    },
    Operation {
        token: "wamn-wms:inventory/aggregate@1.0.0",
        attachment: "inventory-aggregate-http",
        route: "/inventory/aggregate",
    },
];

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn publication_root() -> PathBuf {
    repository_root().join("apps/wamn_wms/publication")
}

fn read_json(path: &Path) -> Value {
    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn declaration() -> ComponentDeclaration {
    let path = publication_root().join("components").join("wms.json.in");
    let mut document = read_json(&path);
    document["scope"]["tenant-id"] = Value::String(TENANT.to_owned());
    serde_json::from_value(document)
        .unwrap_or_else(|error| panic!("decode {}: {error}", path.display()))
}

#[test]
fn package_owned_inputs_declare_the_exact_shipped_route_closure() {
    let attachments: BTreeMap<String, wamn_catalog::ServingAttachment> =
        serde_json::from_value(read_json(&publication_root().join("attachments.json")))
            .expect("the WMS attachment map decodes");
    assert_eq!(attachments.len(), OPERATIONS.len());

    let declaration = declaration();
    assert_eq!(declaration.component, COMPONENT);
    assert_eq!(declaration.interface_version, INTERFACE_VERSION);
    assert_eq!(declaration.operations.len(), OPERATIONS.len());

    for operation in &OPERATIONS {
        let target = AttachmentTarget::Route {
            component: COMPONENT.into(),
            operation: operation.token.into(),
        };

        let fact = declaration
            .operations
            .get(operation.token)
            .unwrap_or_else(|| panic!("{} is not declared on the component", operation.token));
        assert_eq!(fact.input_ports.len(), 1);

        let attachment = attachments
            .get(operation.attachment)
            .unwrap_or_else(|| panic!("{} is not attached", operation.attachment));
        assert_eq!(attachment.kind, AttachmentKind::Http);
        assert_eq!(attachment.package_id, PACKAGE_ID);
        assert_eq!(attachment.target, target);
        assert_eq!(
            attachment.registered_operation.as_deref(),
            Some(operation.token)
        );
        assert_eq!(
            attachment.auth_policy,
            serde_json::json!({"modes": ["pat", "session"]})
        );
        // No author writes a method: publish derives it from the operation kind.
        assert!(attachment.definition["route"].get("method").is_none());
        assert_eq!(attachment.definition["route"]["path"], operation.route);
        assert_eq!(
            attachment.definition["raw-body-bytes"]["maximum"],
            RAW_BODY_MAXIMUM
        );
        // The published schema IS the component's declared input port. Two
        // spellings of what a caller may send is two validators, and the one
        // that was wrong would admit a body the operation refuses.
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

/// The contended command's published schema demands every field the command
/// needs, and admits nothing else. A route that accepted a body the operation
/// refuses would turn a caller's mistake into a server error.
#[test]
fn the_move_route_demands_exactly_what_the_command_requires() {
    let attachments: BTreeMap<String, wamn_catalog::ServingAttachment> =
        serde_json::from_value(read_json(&publication_root().join("attachments.json")))
            .expect("decodes");
    let value = &attachments["inventory-move-http"].definition["input-schema"]["items"]["properties"]
        ["value"];

    let required: Vec<&str> = value["required"]
        .as_array()
        .expect("required")
        .iter()
        .map(|entry| entry.as_str().expect("string"))
        .collect();
    assert_eq!(
        required,
        [
            "idempotency_key",
            "inventory_id",
            "to_packaging_id",
            "to_location_id",
            "expected_row_version",
            "occurred_at"
        ]
    );
    assert_eq!(value["additionalProperties"], false);
}

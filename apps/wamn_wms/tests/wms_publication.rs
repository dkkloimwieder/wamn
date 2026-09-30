//! Semantic gate for the package-owned WMS publication inputs.
//!
//! The same test `receiving_publication` makes, for the second package. It is
//! written rather than generalized: two packages is where a shape starts to
//! look reusable, and the toolkit-promotion rule says the third is where it is
//! promoted, not the second.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_catalog::{AttachmentTarget, AttachmentType, ComponentDeclaration};
use wamn_test_infrastructure::operations::sealed;

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
const OPERATIONS: [Operation; 20] = [
    Operation {
        token: "wamn-wms:packaging/get",
        attachment: "packaging-get-http",
        route: "/packaging/get",
    },
    Operation {
        token: "wamn-wms:packaging/query",
        attachment: "packaging-query-http",
        route: "/packaging/query",
    },
    Operation {
        // RULED 2026-09-24 (wamn-nq1b): the move route calls the export like
        // every other operation. The label wiring is not on the request path.
        token: "wamn-wms:inventory/move",
        attachment: "inventory-move-http",
        route: "/inventory/move",
    },
    Operation {
        token: "wamn-wms:inventory/adjust",
        attachment: "inventory-adjust-http",
        route: "/inventory/adjust",
    },
    Operation {
        token: "wamn-wms:inventory/merge",
        attachment: "inventory-merge-http",
        route: "/inventory/merge",
    },
    Operation {
        token: "wamn-wms:inventory/split",
        attachment: "inventory-split-http",
        route: "/inventory/split",
    },
    Operation {
        token: "wamn-wms:inventory/aggregate",
        attachment: "inventory-aggregate-http",
        route: "/inventory/aggregate",
    },
    Operation {
        token: "wamn-wms:inventory-transaction/get",
        attachment: "inventory-transaction-get-http",
        route: "/inventory_transaction/get",
    },
    Operation {
        token: "wamn-wms:inventory-transaction/query",
        attachment: "inventory-transaction-query-http",
        route: "/inventory_transaction/query",
    },
    Operation {
        token: "wamn-wms:location/create",
        attachment: "location-create-http",
        route: "/location/create",
    },
    Operation {
        token: "wamn-wms:location/get",
        attachment: "location-get-http",
        route: "/location/get",
    },
    Operation {
        token: "wamn-wms:location/query",
        attachment: "location-query-http",
        route: "/location/query",
    },
    Operation {
        token: "wamn-wms:location/update",
        attachment: "location-update-http",
        route: "/location/update",
    },
    Operation {
        token: "wamn-wms:packaging/create",
        attachment: "packaging-create-http",
        route: "/packaging/create",
    },
    Operation {
        token: "wamn-wms:packaging-quantity/get",
        attachment: "packaging-quantity-get-http",
        route: "/packaging_quantity/get",
    },
    Operation {
        token: "wamn-wms:packaging-quantity/query",
        attachment: "packaging-quantity-query-http",
        route: "/packaging_quantity/query",
    },
    Operation {
        token: "wamn-wms:product/create",
        attachment: "product-create-http",
        route: "/product/create",
    },
    Operation {
        token: "wamn-wms:product/get",
        attachment: "product-get-http",
        route: "/product/get",
    },
    Operation {
        token: "wamn-wms:product/query",
        attachment: "product-query-http",
        route: "/product/query",
    },
    Operation {
        token: "wamn-wms:product/update",
        attachment: "product-update-http",
        route: "/product/update",
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

/// One authored publication file, with its operation references resolved.
fn authored(
    path: &Path,
    kind: wamn_schema_generator::operation_reference::AuthoredDocument,
) -> Value {
    wamn_schema_generator::operation_reference::read_authored_document(path, kind)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// One generated publication file of the package.
fn generated(file: &str) -> Value {
    read_json(&publication_root().join("..").join(file))
}

/// The authored attachments with the generated route entries.
fn attachments_document() -> Value {
    let mut document = authored(
        &publication_root().join("attachments.json"),
        wamn_schema_generator::operation_reference::AuthoredDocument::Attachments,
    );
    let generated = generated(wamn_schema_generator::route_schema::GENERATED_ATTACHMENTS);
    document
        .as_object_mut()
        .expect("the attachments are an object")
        .extend(
            generated
                .as_object()
                .expect("the generated attachments are an object")
                .clone(),
        );
    document
}

fn declaration() -> ComponentDeclaration {
    let path = publication_root().join("components").join("wms.json.in");
    let mut document = authored(
        &path,
        wamn_schema_generator::operation_reference::AuthoredDocument::Declaration,
    );
    wamn_schema_generator::route_schema::merge_operations(
        &mut document,
        Some(&generated(
            wamn_schema_generator::route_schema::GENERATED_COMPONENT_OPERATIONS,
        )),
    )
    .expect("the generated operations join the authored ones");
    document["scope"]["tenant-id"] = Value::String(TENANT.to_owned());
    serde_json::from_value(document)
        .unwrap_or_else(|error| panic!("decode {}: {error}", path.display()))
}

#[test]
fn package_owned_inputs_declare_the_exact_shipped_route_closure() {
    let attachments: BTreeMap<String, wamn_catalog::ServingAttachment> =
        serde_json::from_value(attachments_document()).expect("the WMS attachment map decodes");
    assert_eq!(attachments.len(), OPERATIONS.len());

    let declaration = declaration();
    assert_eq!(declaration.component, COMPONENT);
    assert_eq!(declaration.interface_version, INTERFACE_VERSION);
    assert_eq!(declaration.operations.len(), OPERATIONS.len());

    for operation in &OPERATIONS {
        // The table names each operation by reference, as the authored files do.
        let token = sealed(operation.token);
        let target = AttachmentTarget::Route {
            component: COMPONENT.into(),
            operation: token.clone(),
        };

        let fact = declaration
            .operations
            .get(&token)
            .unwrap_or_else(|| panic!("{token} is not declared on the component"));
        assert_eq!(fact.input_ports.len(), 1);

        let attachment = attachments
            .get(operation.attachment)
            .unwrap_or_else(|| panic!("{} is not attached", operation.attachment));
        assert_eq!(attachment.type_, AttachmentType::Http);
        assert_eq!(attachment.package_id, PACKAGE_ID);
        assert_eq!(attachment.target, target);
        assert_eq!(
            attachment.registered_operation.as_deref(),
            Some(token.as_str())
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
        serde_json::from_value(attachments_document()).expect("decodes");
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
            "packaging_id",
            "to_location_id",
            "expected_row_version",
            "occurred_at"
        ]
    );
    assert_eq!(value["additionalProperties"], false);
}

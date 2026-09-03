//! Semantic gate for the package-owned WMS publication inputs.
//!
//! The same proof `receiving_publication` makes, for the second package. It is
//! written rather than generalized: two packages is where a shape starts to
//! look reusable, and the toolkit-promotion rule says the third is where it is
//! promoted, not the second.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_catalog::{AttachmentKind, ComponentDeclaration, WiringDocument, WiringTerminal};

const TENANT: &str = "wms-publication-proof";
const PACKAGE_ID: &str = "wamn_wms";
const COMPONENT: &str = "wms";
const INTERFACE_VERSION: &str = "0.1.0";
const RAW_BODY_MAXIMUM: u64 = 1_048_576;

struct Operation {
    wiring: &'static str,
    token: &'static str,
    attachment: &'static str,
    route: &'static str,
}

const OPERATIONS: [Operation; 7] = [
    Operation {
        wiring: "pallet_get",
        token: "wamn-wms:pallet/get@1.0.0",
        attachment: "pallet-get-http",
        route: "/pallet/get",
    },
    Operation {
        wiring: "pallet_query",
        token: "wamn-wms:pallet/query@1.0.0",
        attachment: "pallet-query-http",
        route: "/pallet/query",
    },
    Operation {
        wiring: "inventory_adjust",
        token: "wamn-wms:inventory/adjust@1.0.0",
        attachment: "inventory-adjust-http",
        route: "/inventory/adjust",
    },
    Operation {
        wiring: "inventory_merge",
        token: "wamn-wms:inventory/merge@1.0.0",
        attachment: "inventory-merge-http",
        route: "/inventory/merge",
    },
    Operation {
        wiring: "inventory_split",
        token: "wamn-wms:inventory/split@1.0.0",
        attachment: "inventory-split-http",
        route: "/inventory/split",
    },
    Operation {
        wiring: "inventory_aggregate",
        token: "wamn-wms:inventory/aggregate@1.0.0",
        attachment: "inventory-aggregate-http",
        route: "/inventory/aggregate",
    },
    Operation {
        wiring: "inventory_move",
        token: "wamn-wms:inventory/move@1.0.0",
        attachment: "inventory-move-http",
        route: "/inventory/move",
    },
];

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn publication_root() -> PathBuf {
    repository_root().join("packages/wms/publication")
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
fn package_owned_inputs_declare_the_exact_seven_route_closure() {
    let attachments: BTreeMap<String, wamn_catalog::ServingAttachment> =
        serde_json::from_value(read_json(&publication_root().join("attachments.json")))
            .expect("the WMS attachment map decodes");
    assert_eq!(attachments.len(), OPERATIONS.len());

    let declaration = declaration();
    assert_eq!(declaration.component, COMPONENT);
    assert_eq!(declaration.interface_version, INTERFACE_VERSION);
    assert_eq!(declaration.operations.len(), OPERATIONS.len());

    for operation in &OPERATIONS {
        // The wiring names the operation the attachment registers, so a route
        // cannot reach a component operation nobody declared.
        let wiring = WiringDocument::parse(&read_json(
            &publication_root()
                .join("wirings")
                .join(format!("{}.json", operation.wiring)),
        ))
        .unwrap_or_else(|error| panic!("{}: {error}", operation.wiring));
        assert_eq!(wiring.wiring_id, operation.wiring);
        let node = wiring
            .nodes
            .get(&wiring.entry)
            .unwrap_or_else(|| panic!("{} has no entry node", operation.wiring));
        assert_eq!(node.component, COMPONENT);
        assert_eq!(node.operation, operation.token);
        assert_eq!(node.terminal, Some(WiringTerminal::Respond));

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
        assert_eq!(attachment.wiring_id, operation.wiring);
        assert_eq!(
            attachment.registered_operation.as_deref(),
            Some(operation.token)
        );
        assert_eq!(attachment.definition["route"]["method"], "POST");
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
            "pallet_id",
            "to_location_id",
            "expected_row_version",
            "occurred_at"
        ]
    );
    assert_eq!(value["additionalProperties"], false);

    // The declared contract is the authority: every field the operation's
    // input contract names must be demanded by the route that carries it.
    let contract = read_json(
        &repository_root().join("packages/wms/generated/contracts/inventory/move.input.json"),
    );
    for field in contract["fields"].as_array().expect("fields") {
        let path = field["path"].as_str().expect("path");
        let Some(name) = path.strip_prefix("value.") else {
            continue;
        };
        assert!(
            required.contains(&name),
            "the route does not demand {path}, which the command requires"
        );
    }
}

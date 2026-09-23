//! Semantic gate for the package-owned WMS publication inputs.
//!
//! The same test `receiving_publication` makes, for the second package. It is
//! written rather than generalized: two packages is where a shape starts to
//! look reusable, and the toolkit-promotion rule says the third is where it is
//! promoted, not the second.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_catalog::{
    AttachmentKind, AttachmentTarget, ComponentDeclaration, WiringDocument, WiringTerminal,
};

const TENANT: &str = "wms-publication-test";
const PACKAGE_ID: &str = "wamn_wms";
const COMPONENT: &str = "wms";
const INTERFACE_VERSION: &str = "0.1.0";
const RAW_BODY_MAXIMUM: u64 = 1_048_576;

struct Operation {
    token: &'static str,
    attachment: &'static str,
    route: &'static str,
    /// The composed wiring that serves the attachment and the node that
    /// RESPONDS, where the entry node hands off and a later node answers.
    /// None means a route: the attachment calls the export once.
    wiring: Option<(&'static str, &'static str)>,
}

// RULED wamn-362o.39: A PACKAGE DECLARES WHAT IT SHIPS. Admission compares
// the declaration against the bytes and refuses any difference, so this
// table, the declaration, the wirings and attachments.json widen together --
// wamn-362o.10 grew the guest from one operation to seven and widened all
// four in one commit.
const OPERATIONS: [Operation; 7] = [
    Operation {
        token: "wamn-wms:pallet/get@1.0.0",
        attachment: "pallet-get-http",
        route: "/pallet/get",
        wiring: None,
    },
    Operation {
        token: "wamn-wms:pallet/query@1.0.0",
        attachment: "pallet-query-http",
        route: "/pallet/query",
        wiring: None,
    },
    Operation {
        // RULED wamn-362o.35: the move route serves the COMPOSED wiring --
        // move -> label-render -> blob-put -- so both gate properties are
        // claims about that path. The entry is still the command; the answer
        // comes from the store node.
        token: "wamn-wms:inventory/move@1.0.0",
        attachment: "inventory-move-http",
        route: "/inventory/move",
        wiring: Some(("inventory_move_and_label", "store")),
    },
    Operation {
        token: "wamn-wms:inventory/adjust@1.0.0",
        attachment: "inventory-adjust-http",
        route: "/inventory/adjust",
        wiring: None,
    },
    Operation {
        token: "wamn-wms:inventory/merge@1.0.0",
        attachment: "inventory-merge-http",
        route: "/inventory/merge",
        wiring: None,
    },
    Operation {
        token: "wamn-wms:inventory/split@1.0.0",
        attachment: "inventory-split-http",
        route: "/inventory/split",
        wiring: None,
    },
    Operation {
        token: "wamn-wms:inventory/aggregate@1.0.0",
        attachment: "inventory-aggregate-http",
        route: "/inventory/aggregate",
        wiring: None,
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
        let target = match operation.wiring {
            Some((wiring_id, respond)) => {
                // The wiring names the operation the attachment registers, so
                // a route cannot reach a component operation nobody declared.
                let wiring = WiringDocument::parse(&read_json(
                    &publication_root()
                        .join("wirings")
                        .join(format!("{wiring_id}.json")),
                ))
                .unwrap_or_else(|error| panic!("{wiring_id}: {error}"));
                assert_eq!(wiring.wiring_id, wiring_id);
                let node = wiring
                    .nodes
                    .get(&wiring.entry)
                    .unwrap_or_else(|| panic!("{wiring_id} has no entry node"));
                assert_eq!(node.component, COMPONENT);
                assert_eq!(node.operation, operation.token);
                // Exactly one node responds, and it is the one the closure
                // names.
                let responders: Vec<&String> = wiring
                    .nodes
                    .iter()
                    .filter(|(_, node)| node.terminal == Some(WiringTerminal::Respond))
                    .map(|(id, _)| id)
                    .collect();
                assert_eq!(
                    responders,
                    vec![respond],
                    "{wiring_id} responds from {respond}"
                );
                AttachmentTarget::Wiring {
                    wiring_id: wiring_id.into(),
                    wiring_version: 1,
                }
            }
            None => AttachmentTarget::Route {
                component: COMPONENT.into(),
                operation: operation.token.into(),
            },
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
}

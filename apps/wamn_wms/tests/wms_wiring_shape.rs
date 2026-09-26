//! WMS owns its authored label workflow: shape, label, and storage.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn read_json(path: &Path) -> Value {
    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// THE WORKFLOW WIRING parses, and its graph is the one the spec describes
/// (docs/plan/workflow-feature.md 4.4).
///
/// Three palette nodes joined by two edges. The move is not a node: it is the
/// route that commits the movement row, and that row's event starts the
/// graph. No node is terminal, because no caller waits for an event.
#[test]
fn the_workflow_wiring_is_a_three_node_graph() {
    let document = read_json(
        &repository_root().join("apps/wamn_wms/publication/wirings/inventory_move_and_label.json"),
    );
    let wiring = wamn_catalog::WiringDocument::parse(&document)
        .expect("the workflow wiring is a valid document");

    let jsonata =
        read_json(&repository_root().join("apps/platform/execution/jsonata/declaration.json.in"));
    let label =
        read_json(&repository_root().join("apps/platform/no-std/label-render/declaration.json.in"));
    assert_eq!(
        jsonata["operations"]["wamn:node/handler@0.1.0"]["output-ports"][1],
        serde_json::json!({"name": "items", "schema": {"type": "array"}}),
    );
    assert_eq!(
        label["operations"]["wamn:node/handler@0.1.0"]["input-ports"][0]["schema"],
        serde_json::json!({"type": "array"}),
        "the shape node's items port and the label input carry one schema"
    );
    assert_eq!(wiring.wiring_id, "inventory_move_and_label");
    assert_eq!(wiring.version, 2);
    assert_eq!(wiring.entry, "shape");
    assert_eq!(wiring.nodes.len(), 3);

    let hops: Vec<(&str, &str, &str)> = wiring
        .edges
        .iter()
        .map(|edge| {
            (
                edge.from.as_str(),
                edge.from_port.as_str(),
                edge.to.as_str(),
            )
        })
        .collect();
    assert_eq!(
        hops,
        [("shape", "items", "label"), ("label", "main", "store")]
    );
    assert!(wiring.nodes.values().all(|node| node.terminal.is_none()));
}

/// WMS declares the wiring as a workflow that the movement insert starts.
#[test]
fn the_movement_insert_starts_the_label_workflow() {
    let manifest = read_json(&repository_root().join("apps/wamn_wms/wamn.json"));
    assert_eq!(
        manifest["workflows"]["movement_label"],
        serde_json::json!({"wiring": "inventory_move_and_label", "registration":
            {"source_package": "wamn_wms", "entity": "inventory_movement", "ops": ["insert"]}})
    );
}

/// The wiring's PARAMS are what make the composition work, so they are pinned.
///
/// `template_id` chooses the label once, at authoring, so a gate case can pin
/// golden output. `key_field` and `body_field` are where blob-put looks. The
/// key is the move's idempotency key, which every movement row of one move
/// carries, so one move stores one label and a redelivery overwrites it.
#[test]
fn the_wirings_params_carry_the_mapping() {
    let document = read_json(
        &repository_root().join("apps/wamn_wms/publication/wirings/inventory_move_and_label.json"),
    );
    let wiring = wamn_catalog::WiringDocument::parse(&document).expect("parses");

    assert!(
        wiring.nodes["shape"].params["expression"]
            .as_str()
            .is_some_and(|expression| expression.contains("new.kind = \"move\"")),
        "only a move labels a pallet"
    );
    assert_eq!(wiring.nodes["label"].params["template_id"], "pallet");

    let store = &wiring.nodes["store"].params;
    assert_eq!(store["store_alias"], "labels");
    assert!(
        wiring.nodes["shape"].params["expression"]
            .as_str()
            .is_some_and(|expression| expression.contains("\"label_key\": new.idempotency_key")),
        "the shape keys the label by the move's command"
    );
    assert_eq!(
        store["key_field"], "/label_key",
        "the object key must be the move's idempotency key, or a move with \
         several product lines or a redelivery writes a second label"
    );
    assert_eq!(store["body_field"], "/zpl");
}

//! WMS owns its authored move, label, and storage composition.

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

/// THE COMPOSED WIRING parses, and its graph is the one the gate describes.
///
/// Three nodes joined by two edges: a package command, a pure transform, and
/// an effect node. This is the low-code claim in one document — nobody wrote
/// code to join them — and it is checked here before any cluster, so a cluster
/// failure means the RUN is wrong rather than the document.
#[test]
fn the_composed_wiring_is_a_three_node_graph() {
    let document = read_json(
        &repository_root().join("apps/wamn_wms/publication/wirings/inventory_move_and_label.json"),
    );
    let wiring = wamn_catalog::WiringDocument::parse(&document)
        .expect("the composed wiring is a valid document");

    let component =
        read_json(&repository_root().join("apps/wamn_wms/publication/components/wms.json.in"));
    assert_eq!(
        component["operations"]["wamn-wms:inventory/move@1.0.0"]["output-ports"][0]["schema"],
        serde_json::json!({"type": "array"}),
        "the application entry emits the route envelope"
    );
    assert_eq!(wiring.wiring_id, "inventory_move_and_label");
    assert_eq!(wiring.entry, "move");
    assert_eq!(wiring.nodes.len(), 3);
    assert_eq!(wiring.edges.len(), 2);

    // The walk order the router will take: entry, then each edge's target.
    let hops: Vec<(&str, &str)> = wiring
        .edges
        .iter()
        .map(|edge| (edge.from.as_str(), edge.to.as_str()))
        .collect();
    assert_eq!(hops, [("move", "label"), ("label", "store")]);

    // ONLY the last node is terminal. A command that ended the delivery would
    // never reach the label, and this is exactly what `terminal: Option`
    // exists for — most of a graph is intermediate work.
    assert!(wiring.nodes["move"].terminal.is_none());
    assert!(wiring.nodes["label"].terminal.is_none());
    assert!(wiring.nodes["store"].terminal.is_some());
}

/// The wiring's PARAMS are what make the composition work, so they are pinned.
///
/// `template_id` chooses the label once, at authoring, so a gate case can pin
/// golden output. `key_field` and `body_field` are where blob-put looks, and
/// pointing the key at `movement_id` is what makes a redelivery overwrite one
/// object instead of writing a second.
#[test]
fn the_wirings_params_carry_the_mapping() {
    let document = read_json(
        &repository_root().join("apps/wamn_wms/publication/wirings/inventory_move_and_label.json"),
    );
    let wiring = wamn_catalog::WiringDocument::parse(&document).expect("parses");

    assert_eq!(wiring.nodes["label"].params["template_id"], "pallet");

    let store = &wiring.nodes["store"].params;
    assert_eq!(store["store_alias"], "labels");
    assert_eq!(
        store["key_field"], "/movement_id",
        "the object key must be the claim-generated movement id, or a \
         redelivery writes a second label"
    );
    assert_eq!(store["body_field"], "/zpl");
}

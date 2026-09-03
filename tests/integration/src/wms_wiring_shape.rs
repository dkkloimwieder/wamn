//! The three-node WMS chain composes, hop by hop, against the real
//! declarations.
//!
//! An edge carries a payload VERBATIM — `enqueue_successors` in
//! `crates/execution/router/src/walk.rs` clones it into the successor token
//! and nothing maps it. So a chain composes only if each node's output
//! satisfies the next node's input, and the only adjustment available is a
//! WIRING PARAMETER. This asserts that for
//!
//!     inventory.move  ->  label-render  ->  blob-put
//!
//! before the wiring is authored, because the alternative is discovering a
//! shape gap on the cluster.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(path: &Path) -> Value {
    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn declaration(relative: &str) -> Value {
    read_json(&repository_root().join(relative))
}

fn handler(declaration: &Value) -> &Value {
    &declaration["operations"]["wamn:node/handler@0.1.0"]
}

fn parameter_names(node: &Value) -> Vec<String> {
    node["parameters"]
        .as_array()
        .expect("parameters")
        .iter()
        .map(|parameter| {
            parameter["name"]
                .as_str()
                .expect("parameter name")
                .to_owned()
        })
        .collect()
}

/// HOP 1 — the command's result satisfies what the pallet template names.
///
/// `label-render` accepts any object and ignores fields it does not use, so
/// the requirement is one-directional: every field the template NAMES must be
/// present in the command's result.
#[test]
fn the_move_result_carries_every_field_the_pallet_label_names() {
    let result = read_json(
        &repository_root().join("packages/wms/generated/contracts/inventory/move.result.json"),
    );
    let emitted: Vec<&str> = result["fields"]
        .as_array()
        .expect("result fields")
        .iter()
        .map(|field| field["path"].as_str().expect("path"))
        .collect();

    // The `pallet` template's fields, from label-template's own spec:
    // barcode `pallet_id`, lines `pallet_id` and `location_id`.
    for named in ["pallet_id", "location_id"] {
        assert!(
            emitted.contains(&named),
            "the pallet label names {named}, which inventory.move does not emit: {emitted:?}"
        );
    }
}

/// HOP 2 — label-render ENRICHES, so upstream context survives it.
///
/// This is the palette rule the chain depends on. A pure transform that
/// replaced its payload would destroy `movement_id` at this hop, and no wiring
/// parameter downstream could recover it — a pointer can find a field, not
/// resurrect one.
#[test]
fn label_render_declares_an_enriching_output() {
    let document = declaration("components/no-std/label-render/declaration.json.in");
    let node = handler(&document);
    let output = &node["output-ports"][0]["schema"];

    assert_eq!(output["required"], serde_json::json!(["zpl"]));
    assert_eq!(
        output["additionalProperties"],
        serde_json::json!(true),
        "a replacing output would strand every upstream field at this hop"
    );
    assert_eq!(parameter_names(node), ["template_id"]);
}

/// HOP 3 — blob-put maps its own inputs with pointers, so it composes with a
/// predecessor it was not shaped around.
#[test]
fn blob_put_locates_its_key_and_body_by_wiring_parameter() {
    let document = declaration("components/execution/blob-put/declaration.json.in");
    let node = handler(&document);
    assert_eq!(
        parameter_names(node),
        ["store_alias", "key_field", "body_field"]
    );

    // Both are JSON pointers, the same shape `transform` uses — one mapping
    // mechanism in the palette, not two.
    let transform_document = declaration("components/no-std/transform/declaration.json.in");
    let transform = handler(&transform_document);
    let pointer_pattern = transform["parameters"][0]["schema"]["pattern"].clone();
    assert!(
        pointer_pattern.is_string(),
        "transform declares a pointer pattern"
    );
    for parameter in node["parameters"].as_array().expect("parameters") {
        let name = parameter["name"].as_str().expect("name");
        if name.ends_with("_field") {
            assert_eq!(
                parameter["schema"]["pattern"], pointer_pattern,
                "{name} is not the palette's pointer shape"
            );
            assert_eq!(parameter["required"], serde_json::json!(true), "{name}");
        }
    }

    // The input names no members: what it needs is found by pointer, so the
    // node imposes no shape on whatever precedes it.
    let input = &node["input-ports"][0]["schema"];
    assert!(
        input.get("required").is_none(),
        "blob-put still demands member names, which re-couples it to its predecessor: {input}"
    );
}

/// THE CHAIN, end to end on paper: the fields a wirer would point at exist.
///
/// `key_field` must resolve in what label-render emits, which is the move's
/// result plus `zpl`. This is the assertion that would have caught the
/// original gap — `{zpl}` alone carried no key.
#[test]
fn the_wirer_can_point_at_a_key_and_a_body() {
    let result = read_json(
        &repository_root().join("packages/wms/generated/contracts/inventory/move.result.json"),
    );
    let mut available: Vec<&str> = result["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|field| field["path"].as_str().expect("path"))
        .collect();
    // What label-render adds.
    available.push("zpl");

    // The wiring points key at the movement id — stable under replay, because
    // the command table pre-generates it — and body at the rendered label.
    assert!(
        available.contains(&"movement_id"),
        "no stable object key survives to blob-put: {available:?}"
    );
    assert!(available.contains(&"zpl"), "{available:?}");
}

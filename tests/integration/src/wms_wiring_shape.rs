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
fn label_render_declares_the_envelope_on_both_ports() {
    // RULED wamn-362o.42: an edge carries the route envelope, one value of one
    // schema, and the gate compares port-schema DIGESTS for equality. So the
    // palette's ports are the entry's ports, byte for byte -- the first edge
    // the gate ever evaluated refused on exactly this. What the node does to
    // each item's value (enrich it with zpl) is the guest's contract, held in
    // its docs and by the composed route's runtime assertions, not in a port
    // schema the gate would then refuse.
    let document = declaration("components/no-std/label-render/declaration.json.in");
    let node = handler(&document);
    let envelope = entry_envelope_schema();
    assert_eq!(node["input-ports"][0]["schema"], envelope, "input is the envelope");
    assert_eq!(node["output-ports"][0]["schema"], envelope, "output is the envelope");
    assert_eq!(parameter_names(node), ["template_id"]);
}

/// The schema the entry operation emits on `main`: the route envelope. Every
/// edge in the composed wiring must carry exactly this.
fn entry_envelope_schema() -> serde_json::Value {
    let document = declaration("packages/wms/publication/components/wms.json.in");
    let operation = &document["operations"]["wamn-wms:inventory/move@1.0.0"];
    let schema = operation["output-ports"][0]["schema"].clone();
    assert_eq!(schema, serde_json::json!({"type": "array"}), "the entry emits the envelope");
    schema
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
    // node imposes no shape on whatever precedes it -- and, RULED
    // wamn-362o.42, both ports are the route envelope the entry emits, which
    // is what the gate's digest rule compares on each edge.
    let input = &node["input-ports"][0]["schema"];
    assert!(
        input.get("required").is_none(),
        "blob-put still demands member names, which re-couples it to its predecessor: {input}"
    );
    let envelope = entry_envelope_schema();
    assert_eq!(*input, envelope, "input is the envelope");
    assert_eq!(node["output-ports"][0]["schema"], envelope, "output is the envelope");
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

/// The aggregate excludes CONSUMED pallets, and its SQL says why.
///
/// This is a DOMAIN LAW, not a preference. The platform admits no `DELETE`
/// (`sql_lex.rs` `refuse_unsupported_effects`), so a merge tombstones: the
/// source pallet keeps its quantity rows as history of what it held when it
/// was absorbed, while that same quantity now also sits on the target.
/// Counting both double-counts every merge the warehouse has ever done.
///
/// Measured on a real database while authoring: one live pallet holding 100
/// and one consumed pallet holding 40 aggregate to **100**, where a naive
/// `sum(quantity)` over `pallet_quantity` returns 140. The assertion here
/// pins the clause that makes the difference, because losing it would be
/// silent — the totals would simply be wrong, with nothing failing.
#[test]
fn the_aggregate_excludes_consumed_pallets() {
    let sql = std::fs::read_to_string(
        repository_root().join("packages/wms/query/inventory_aggregate.sql"),
    )
    .expect("the aggregate query is authored");

    let statement: String = sql
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<&str>>()
        .join(" ");

    assert!(
        statement.contains("pallet.status <> 'consumed'"),
        "the aggregate counts tombstoned pallets, which double-counts every \
         merge: {statement}"
    );
    // It must actually JOIN pallet to be able to filter on its status — a
    // clause referencing a table the query never joined would not compile,
    // but a query that dropped the join and kept a stale comment would.
    assert!(statement.contains("JOIN pallet"), "{statement}");
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
        &repository_root().join("packages/wms/publication/wirings/inventory_move_and_label.json"),
    );
    let wiring = wamn_catalog::WiringDocument::parse(&document)
        .expect("the composed wiring is a valid document");

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
        &repository_root().join("packages/wms/publication/wirings/inventory_move_and_label.json"),
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

    // Every pointer must resolve in what actually arrives at that node: the
    // move's result, plus the `zpl` label-render adds. This is the assertion
    // that ties the params to the contracts rather than to intent.
    let result = read_json(
        &repository_root().join("packages/wms/generated/contracts/inventory/move.result.json"),
    );
    let mut available: Vec<String> = result["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|field| format!("/{}", field["path"].as_str().expect("path")))
        .collect();
    available.push("/zpl".to_owned());

    for pointer in ["key_field", "body_field"] {
        let target = store[pointer].as_str().expect("a pointer string");
        assert!(
            available.contains(&target.to_owned()),
            "{pointer} points at {target}, which nothing upstream emits: {available:?}"
        );
    }
}

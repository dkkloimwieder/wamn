//! Palette declarations retain envelope ports and parameter contracts.

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

fn async_handler(declaration: &Value) -> &Value {
    &declaration["operations"]["wamn:node/async-handler@0.1.0"]
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

#[test]
fn label_render_declares_the_envelope_on_both_ports() {
    let document = declaration("apps/platform/no-std/label-render/declaration.json.in");
    let node = handler(&document);
    let envelope = serde_json::json!({"type": "array"});
    assert_eq!(
        node["input-ports"][0]["schema"], envelope,
        "input is the envelope"
    );
    assert_eq!(
        node["output-ports"][0]["schema"], envelope,
        "output is the envelope"
    );
    assert_eq!(parameter_names(node), ["template_id"]);
}

#[test]
fn blob_put_locates_its_key_and_body_by_wiring_parameter() {
    let document = declaration("apps/platform/execution/blob-put/declaration.json.in");
    let node = async_handler(&document);
    assert_eq!(
        parameter_names(node),
        ["store_alias", "key_field", "body_field"]
    );

    let transform_document = declaration("apps/platform/no-std/transform/declaration.json.in");
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

    let input = &node["input-ports"][0]["schema"];
    assert!(
        input.get("required").is_none(),
        "blob-put still demands member names, which re-couples it to its predecessor: {input}"
    );
    let envelope = serde_json::json!({"type": "array"});
    assert_eq!(*input, envelope, "input is the envelope");
    assert_eq!(
        node["output-ports"][0]["schema"], envelope,
        "output is the envelope"
    );
}

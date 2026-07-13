//! Contract tests for the `wamn.node.manifest` annotation: fixture round-trip,
//! structural validation negatives, JSON-Schema conformance (boon), and the
//! committed-schema drift guard (the wamn-flow/wamn-catalog pattern).

use boon::{Compiler, Schemas};
use wamn_node_manifest::{ANNOTATION_KEY, NodeManifest, OrderingPolicy};

const FIXTURE: &str = include_str!("fixtures/sample-echo.manifest.json");

fn fixture() -> NodeManifest {
    NodeManifest::from_json(FIXTURE).expect("fixture parses")
}

#[test]
fn fixture_parses_validates_and_round_trips() {
    let m = fixture();
    assert!(
        m.is_valid(),
        "fixture must validate clean: {:?}",
        m.issues()
    );
    assert_eq!(m.node_type, "sample-echo");
    assert_eq!(m.contract, "0.1.0");
    assert_eq!(m.ordering, vec![OrderingPolicy::Unordered]);
    // Defaults fill unlisted fields.
    assert_eq!(m.output_ports, vec!["main"]);
    let again = NodeManifest::from_json(&m.to_json()).expect("re-parses");
    assert_eq!(m, again);
}

#[test]
fn minimal_manifest_gets_the_defaults() {
    let m = NodeManifest::from_json(
        r#"{"schema-version":"0.1","node-type":"t","name":"T","version":"1.0.0","contract":"0.1.0"}"#,
    )
    .expect("parses");
    assert!(m.is_valid(), "{:?}", m.issues());
    assert_eq!(
        m.ordering,
        vec![
            OrderingPolicy::Strict,
            OrderingPolicy::Partitioned,
            OrderingPolicy::Unordered
        ]
    );
    assert_eq!(m.output_ports, vec!["main"]);
}

#[test]
fn structural_negatives_are_rejected() {
    let mut m = fixture();
    m.node_type = "Not:A:Slug".into();
    assert!(m.issues().iter().any(|i| i.code == "invalid-node-type"));

    let mut m = fixture();
    m.schema_version = "0.2".into();
    assert!(
        m.issues()
            .iter()
            .any(|i| i.code == "unsupported-schema-version")
    );

    let mut m = fixture();
    m.contract = "0.1".into();
    assert!(
        m.issues()
            .iter()
            .any(|i| i.code == "invalid-contract-version")
    );

    let mut m = fixture();
    m.config_schema = Some(serde_json::json!(5));
    assert!(m.issues().iter().any(|i| i.code == "invalid-json-schema"));

    let mut m = fixture();
    m.ordering = vec![OrderingPolicy::Strict, OrderingPolicy::Strict];
    assert!(m.issues().iter().any(|i| i.code == "duplicate-ordering"));

    let mut m = fixture();
    m.ordering.clear();
    assert!(m.issues().iter().any(|i| i.code == "empty-ordering"));

    let mut m = fixture();
    m.output_ports = vec!["error".into()];
    assert!(m.issues().iter().any(|i| i.code == "reserved-output-port"));

    let mut m = fixture();
    m.output_ports = vec!["main".into(), "main".into()];
    assert!(m.issues().iter().any(|i| i.code == "duplicate-output-port"));

    let mut m = fixture();
    m.name = "  ".into();
    assert!(m.issues().iter().any(|i| i.code == "empty-name"));
}

#[test]
fn unknown_fields_are_rejected() {
    let json = r#"{"schema-version":"0.1","node-type":"t","name":"T","version":"1.0.0","contract":"0.1.0","grants":["http"]}"#;
    // Grants are DERIVED from WIT imports (design-note 7), never declared in
    // the manifest — an attempt to declare them must not parse.
    assert!(NodeManifest::from_json(json).is_err());
}

#[test]
fn fixture_conforms_to_the_published_schema() {
    let mut schemas = Schemas::new();
    let mut compiler = Compiler::new();
    let schema_doc: serde_json::Value =
        serde_json::from_str(include_str!("../../../docs/wamn-node-manifest.schema.json"))
            .expect("published schema parses");
    compiler
        .add_resource("manifest-schema", schema_doc)
        .expect("schema resource");
    let idx = compiler
        .compile("manifest-schema", &mut schemas)
        .expect("schema compiles");
    let doc: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture is JSON");
    schemas
        .validate(&doc, idx)
        .expect("fixture conforms to the published schema");
}

#[test]
fn schema_drift() {
    let committed = include_str!("../../../docs/wamn-node-manifest.schema.json");
    assert_eq!(
        committed,
        wamn_node_manifest::json_schema_string(),
        "docs/wamn-node-manifest.schema.json is out of sync with the types; \
         regenerate: cargo run -p wamn-node-manifest --example print-schema > docs/wamn-node-manifest.schema.json"
    );
}

#[test]
fn annotation_key_is_pinned() {
    // Design-note 8: the registry palette scans this exact key.
    assert_eq!(ANNOTATION_KEY, "wamn.node.manifest");
}

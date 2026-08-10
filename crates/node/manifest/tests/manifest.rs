//! Contract tests for the `wamn.node.manifest` annotation: fixture round-trip,
//! structural validation negatives, JSON-Schema conformance (boon), and the
//! committed-schema drift guard (the wamn-flow/wamn-schema-model pattern).

use boon::{Compiler, Schemas};
use wamn_node_manifest::{
    ANNOTATION_KEY, NodeManifest, NodeWorld, OrderingPolicy, Purity, RecoveryClass, ResolvedPurity,
};

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
    assert_eq!(m.purity, Some(Purity::Pure));
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
    assert_eq!(m.purity, None);
}

#[test]
fn t_nr_absent_purity_resolves_to_effectful_never_replay() {
    let mut m = fixture();
    m.purity = None;
    let resolved = m
        .resolved_component(format!("sha256:{}", "1".repeat(64)))
        .expect("valid manifest resolves");
    assert_eq!(
        resolved.contract.interface.interface_contract,
        "wamn:node/node@0.1.0"
    );
    assert_eq!(
        resolved.contract.executable_recovery.purity,
        ResolvedPurity::Effectful
    );
    assert_eq!(
        resolved.contract.executable_recovery.conservative_class,
        RecoveryClass::NeverReplay
    );
}

#[test]
fn stream_world_resolution_pins_the_authoritative_p2_import_closure() {
    let m = fixture();
    let digest = format!("sha256:{}", "1".repeat(64));
    let plain = m
        .resolved_component(digest.clone())
        .expect("zero-import world resolves");
    let streamed = m
        .resolved_component_for_world(NodeWorld::StreamNode, digest)
        .expect("stream world resolves");

    assert_eq!(
        streamed.contract.interface.interface_contract,
        "wamn:node/stream-node@0.1.0"
    );
    assert_eq!(
        NodeWorld::StreamNode.external_imports(),
        ["wasi:io/streams@0.2.12"]
    );
    assert_ne!(plain.identity_hash(), streamed.identity_hash());
    m.validate_resolved_interface_for_world(NodeWorld::StreamNode, &streamed.contract.interface)
        .expect("stream world validates against the same strict selection");
    assert!(
        m.validate_resolved_interface(&streamed.contract.interface)
            .is_err(),
        "a stream-world contract must not validate as the zero-import world"
    );

    let authority = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../docs/archive/contracts/wamn-node.wit"),
    )
    .expect("authoritative node WIT reads");
    let payloads = authority
        .split_once("interface payloads {")
        .and_then(|(_, rest)| rest.split_once("interface credentials {"))
        .map(|(block, _)| block)
        .expect("payloads interface is bounded by credentials");
    assert!(payloads.contains("use wasi:io/streams@0.2.12.{input-stream, output-stream};"));
    let stream_world = authority
        .split_once("world stream-node {")
        .and_then(|(_, rest)| rest.split_once('}'))
        .map(|(block, _)| block)
        .expect("stream-node world exists");
    for declaration in ["import payloads;", "import control;", "export handler;"] {
        assert!(
            stream_world.contains(declaration),
            "stream-node world lost {declaration}"
        );
    }
}

#[test]
fn declared_pure_resolves_to_replay_and_sorted_ports() {
    let mut m = fixture();
    m.output_ports = vec!["retry".to_string(), "main".to_string()];
    let resolved = m.resolved_interface().expect("valid manifest resolves");
    assert_eq!(resolved.output_ports, vec!["main", "retry"]);
    assert!(resolved.permits_output_port("retry"));
    assert!(!resolved.permits_output_port("undeclared"));
}

#[test]
fn mismatched_resolved_interface_is_rejected() {
    let m = fixture();
    let mut resolved = m.resolved_interface().expect("valid manifest resolves");
    resolved.output_ports.push("undeclared".to_string());
    let issues = m
        .validate_resolved_interface(&resolved)
        .expect_err("an undeclared port must not enter the pinned interface");
    assert_eq!(issues[0].code, "resolved-interface-mismatch");
}

#[test]
fn artifact_identity_pins_interface_and_component_digest() {
    let m = fixture();
    let digest_a = format!("sha256:{}", "1".repeat(64));
    let digest_b = format!("sha256:{}", "2".repeat(64));
    let a = m
        .resolved_component(digest_a)
        .expect("complete identity inputs resolve");
    let changed_digest = m
        .resolved_component(digest_b)
        .expect("complete identity inputs resolve");

    let mut changed_manifest = m.clone();
    changed_manifest.output_ports.push("retry".to_string());
    let changed_interface = changed_manifest
        .resolved_component(format!("sha256:{}", "1".repeat(64)))
        .expect("complete identity inputs resolve");

    assert_ne!(a.identity_hash(), changed_digest.identity_hash());
    assert_ne!(a.identity_hash(), changed_interface.identity_hash());
    assert!(
        a.identity_bytes()
            .windows(b"interface".len())
            .any(|window| window == b"interface")
    );
    assert!(
        a.identity_bytes()
            .windows(b"executable".len())
            .any(|window| { window == b"executable" })
    );
}

#[test]
fn missing_or_malformed_component_digest_is_rejected() {
    let m = fixture();
    for digest in ["", "sha256:", "sha256:ABC", "sha512:abc"] {
        let issues = m
            .resolved_component(digest)
            .expect_err("identity must pin a valid supplied-component digest");
        assert_eq!(issues[0].code, "invalid-component-digest");
    }
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
    m.contract = "0.2.0".into();
    assert!(
        m.issues()
            .iter()
            .any(|i| i.code == "unsupported-contract-version")
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
fn invalid_purity_value_is_rejected() {
    let json = r#"{"schema-version":"0.1","node-type":"t","name":"T","version":"1.0.0","contract":"0.1.0","purity":"effectful"}"#;
    assert!(NodeManifest::from_json(json).is_err());
}

#[test]
fn fixture_conforms_to_the_published_schema() {
    let mut schemas = Schemas::new();
    let mut compiler = Compiler::new();
    let schema_doc: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../docs/archive/contracts/wamn-node-manifest.schema.json"
    ))
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
    let committed = include_str!("../../../../docs/archive/contracts/wamn-node-manifest.schema.json");
    assert_eq!(
        committed,
        wamn_node_manifest::json_schema_string(),
        "docs/archive/contracts/wamn-node-manifest.schema.json is out of sync with the types; \
         regenerate: cargo run -p wamn-node-manifest --example print-node-manifest-schema > docs/archive/contracts/wamn-node-manifest.schema.json"
    );
}

#[test]
fn annotation_key_is_pinned() {
    // Design-note 8: the registry palette scans this exact key.
    assert_eq!(ANNOTATION_KEY, "wamn.node.manifest");
}

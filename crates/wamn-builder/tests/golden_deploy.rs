//! 5.5f golden tests: the emitted serve-node deployment manifest for a
//! world-node (empty grants, no `--allowed-hosts`) vs an http-node (wasi:http +
//! allowedHosts), over SYNTHESIZED components (the socketguard pattern) whose
//! grants are DERIVED from the compiled imports.
//!
//! Regenerate the golden files with `BLESS=1 cargo test -p wamn-builder golden`.

use std::path::PathBuf;

use wamn_builder::deploy_emit::{EmitInputs, render_serve_node_deployment};
use wamn_host::egress_guard::derive_grants_from_component;
use wamn_host::engine::build_engine;

/// Synthesize a minimal, valid component importing exactly `import_names`.
fn synth_component(import_names: &[&str]) -> Vec<u8> {
    use wasm_encoder::{
        Component, ComponentImportSection, ComponentTypeRef, ComponentTypeSection, InstanceType,
    };
    let mut types = ComponentTypeSection::new();
    for _ in import_names {
        types.instance(&InstanceType::new());
    }
    let mut imports = ComponentImportSection::new();
    for (i, name) in import_names.iter().enumerate() {
        imports.import(*name, ComponentTypeRef::Instance(i as u32));
    }
    let mut component = Component::new();
    component.section(&types);
    component.section(&imports);
    component.finish()
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn check_golden(name: &str, rendered: &str) {
    let path = golden_path(name);
    if std::env::var("BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, rendered).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "read golden {}: {e} (run BLESS=1 to generate)",
            path.display()
        )
    });
    assert_eq!(rendered, expected, "rendered manifest drifted from {name}");
}

fn render_for(import_names: &[&str], inputs: EmitInputs) -> String {
    let engine = build_engine(&[]).expect("engine");
    let wasm = synth_component(import_names);
    let grants =
        derive_grants_from_component(engine.inner(), &wasm, &inputs.node_type).expect("grants");
    render_serve_node_deployment(&inputs, &grants).expect("render")
}

#[test]
fn golden_world_node_empty_grants() {
    // `world node`: imports NOTHING -> empty grants, no --allowed-hosts.
    let rendered = render_for(
        &[],
        EmitInputs {
            node_type: "sample-echo".to_string(),
            image: "registry.wamn-system.svc.cluster.local:5000/wamn/sample-node:dev".to_string(),
            signed_digest:
                "sha256:1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
            signature: Some("abcdef".to_string()),
            project: "default".to_string(),
            allowed_hosts: vec![],
        },
    );
    check_golden("world-node.deployment.yaml", &rendered);
}

#[test]
fn golden_disposition_node_empty_grants() {
    // POC-F2 (wamn-1ab): the disposition-recommendation node is a `world node`
    // — imports NOTHING — so it emits empty grants and NO --allowed-hosts, the
    // shape that proves builder grant derivation for the F2 traceability row.
    let rendered = render_for(
        &[],
        EmitInputs {
            node_type: "disposition-recommendation".to_string(),
            image: "registry.wamn-system.svc.cluster.local:5000/wamn/disposition-node:dev"
                .to_string(),
            signed_digest:
                "sha256:3333333333333333333333333333333333333333333333333333333333333333"
                    .to_string(),
            signature: Some("fedcba".to_string()),
            project: "default".to_string(),
            allowed_hosts: vec![],
        },
    );
    check_golden("disposition-node.deployment.yaml", &rendered);
}

/// The compiled disposition-node artifact (built by `cargo build --release
/// --target wasm32-wasip2 -p disposition-node` inside the components workspace).
/// Absent on a host that has not built it — the test then SKIPS (the nodebench
/// `.exists()` precedent), so `cargo test -p wamn-builder` never requires a wasm
/// toolchain, while a built tree exercises the real bytes.
fn disposition_node_wasm() -> Option<Vec<u8>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../components/target/wasm32-wasip2/release/disposition_node.wasm");
    std::fs::read(&path).ok()
}

/// The dd71ccc lesson: a SYNTHESIZED empty-import component can miss a real
/// artifact's import shape. So screen the REAL compiled disposition-node through
/// the SAME builder import lint the pipeline runs, DERIVE its grants from the
/// real component type, and assert they are empty / no --allowed-hosts.
///
/// MUTATION (ii) TARGET: marking `requires_allowed_hosts` for a zero-import node,
/// or dropping the `if grants.requires_allowed_hosts` guard in the emitter, is
/// caught here (empty-grants assert + a spurious --allowed-hosts refusal) AND by
/// the two empty-grants goldens above.
#[test]
fn disposition_node_real_artifact_lints_and_derives_empty_grants() {
    let Some(wasm) = disposition_node_wasm() else {
        eprintln!("SKIP: disposition_node.wasm not built; run the wasm build to exercise it");
        return;
    };
    // The real import lint the 5.5 pipeline runs — a zero-import node passes.
    wamn_builder::build::lint_artifact(&wasm, "disposition_node.wasm")
        .expect("the real disposition-node passes the builder import lint");

    let engine = build_engine(&[]).expect("engine");
    let grants = derive_grants_from_component(engine.inner(), &wasm, "disposition-recommendation")
        .expect("derive grants");
    assert!(
        grants.host_interfaces.is_empty(),
        "a world node grants no host interfaces, got {:?}",
        grants.host_interfaces
    );
    assert!(
        !grants.requires_allowed_hosts,
        "a world node must not require --allowed-hosts"
    );

    // The emitted manifest carries no --allowed-hosts for these empty grants.
    let inputs = EmitInputs {
        node_type: "disposition-recommendation".to_string(),
        image: "registry.wamn-system.svc.cluster.local:5000/wamn/disposition-node:dev".to_string(),
        signed_digest: "sha256:0".to_string(),
        signature: None,
        project: "default".to_string(),
        allowed_hosts: vec![],
    };
    let rendered = render_serve_node_deployment(&inputs, &grants).expect("render");
    assert!(
        !rendered.contains("--allowed-hosts"),
        "a world node's deployment must not carry --allowed-hosts"
    );
}

#[test]
fn golden_http_node_with_allowed_hosts() {
    // `http-node`: wasi:http/outgoing-handler + credentials + control -> requires
    // allowedHosts, so the emitted manifest carries --allowed-hosts.
    let rendered = render_for(
        &[
            "wasi:http/outgoing-handler@0.2.6",
            "wamn:node/credentials@0.1.0",
            "wamn:node/control@0.1.0",
        ],
        EmitInputs {
            node_type: "http-caller".to_string(),
            image: "registry.wamn-system.svc.cluster.local:5000/wamn/http-caller:dev".to_string(),
            signed_digest:
                "sha256:2222222222222222222222222222222222222222222222222222222222222222"
                    .to_string(),
            signature: Some("012345".to_string()),
            project: "default".to_string(),
            allowed_hosts: vec!["api.example.com:443".to_string()],
        },
    );
    check_golden("http-node.deployment.yaml", &rendered);
}

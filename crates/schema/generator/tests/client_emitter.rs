//! The emitted client must COMPILE, not merely look right.
//!
//! A generated file is only a deliverable if rustc accepts it. These tests
//! emit the platform fixture, write the output into a scratch crate that
//! depends on the real `wamn-client`, and run `cargo check` over it — so a
//! type spelling, an identifier or an import that the emitter gets wrong fails
//! here rather than in the first consumer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use wamn_schema_generator::client_ir::ClientContractIr;
use wamn_schema_generator::client_rust::emit_rust_client;

#[path = "support/platform_fixture.rs"]
mod fixture;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("the generator sits three levels below the repository root")
        .to_path_buf()
}

fn release() -> ClientContractIr {
    fixture::client_release()
}

/// Emit `package` into a scratch crate and `cargo check` it.
fn check_compiles(ir: &ClientContractIr, scratch_name: &str) -> String {
    let package = &ir.package;
    let root = repository_root();
    let scratch = std::env::temp_dir().join(scratch_name);
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(scratch.join("src")).expect("scratch");

    let files = emit_rust_client(ir).expect("the release emits");
    let mut lib = String::new();
    let mut combined = String::new();
    for file in &files {
        let module = Path::new(file.path())
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("a module file name");
        let source = String::from_utf8(file.bytes().to_vec()).expect("emitted UTF-8");
        combined.push_str(&source);
        std::fs::write(scratch.join(format!("src/{module}.rs")), &source).expect("write module");
        lib.push_str("pub mod ");
        lib.push_str(module);
        lib.push_str(";\n");
    }
    std::fs::write(scratch.join("src/lib.rs"), lib).expect("write lib");
    std::fs::write(
        scratch.join("Cargo.toml"),
        format!(
            "[package]\nname = \"emitted-client-check\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
             \n[dependencies]\n\
             wamn-client = {{ path = \"{}\" }}\n\
             serde_json = \"1\"\nuuid = \"1\"\nchrono = \"0.4\"\nrust_decimal = \"1\"\n\
             \n[workspace]\n",
            root.join("crates/client/core").display()
        ),
    )
    .expect("write manifest");

    let output = Command::new(env!("CARGO"))
        .args(["check", "--offline", "--quiet"])
        .current_dir(&scratch)
        .output()
        .expect("cargo check runs");
    assert!(
        output.status.success(),
        "the emitted {package} client does not compile:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_dir_all(&scratch);
    combined
}

/// EXIT GATE: the emitted client compiles against the real `wamn-client`.
#[test]
fn the_emitted_platform_client_compiles() {
    let source = check_compiles(&release(), "wamn-emitted-client-platform");
    // Guard the guard: an empty emission would compile trivially.
    assert!(
        source.contains("pub struct WidgetUpdateRequest"),
        "the emission is empty or missing its types"
    );
    assert!(
        source.contains("/widget/archive"),
        "the custom operation route is absent from its client"
    );
}

/// RULING 3: no deployment fact reaches generated code.
///
/// Not a tautology over an input that has none — the assertion is paired with
/// the positive half, that the release's method and template ARE emitted. A
/// client that emitted neither would pass the negative half alone.
#[test]
fn generated_code_carries_release_facts_and_no_deployment_facts() {
    for package in [release()] {
        let files = emit_rust_client(&package).expect("emits");
        let source: String = files
            .iter()
            .map(|file| String::from_utf8(file.bytes().to_vec()).expect("UTF-8"))
            .collect();

        for deployment_fact in [
            "localhost",
            "http://",
            "https://",
            "svc.cluster.local",
            "wamn-system",
            "base_url",
            "host:",
        ] {
            assert!(
                !source.contains(deployment_fact),
                "generated code names the deployment fact {deployment_fact:?}"
            );
        }

        // The positive half: release facts ARE emitted.
        assert!(source.contains("method: \"POST\""), "no method");
        assert!(source.contains("template: \"/"), "no path template");
    }
}

/// ACCEPTANCE: a new field appears in the types from the IR alone.
///
/// The contract is edited, nothing else. If any hand-authored table mapped
/// operations to fields, the new field would be missing from the emission.
#[test]
fn a_field_added_to_a_contract_appears_without_a_hand_edit() {
    let package = fixture::generate_fixture();
    let mut contracts = fixture::contracts(&package);
    let path = "widget/update.input.json";
    let mut document: serde_json::Value = serde_json::from_slice(&contracts[path]).expect("parse");
    document["writable_fields"]
        .as_array_mut()
        .expect("writable fields")
        .push(serde_json::json!({
            "explicit_null": "invalid_input",
            "field": "freight_terms",
            "omitted": "unchanged",
            "type": "text",
        }));
    contracts.insert(
        path.to_owned(),
        serde_json::to_vec(&document).expect("serialize"),
    );

    // This fixture changes only the unserved contract. A published input
    // schema must also change before a served route can accept the new field.
    let ir = ClientContractIr::from_release_contracts(
        "platform_fixture",
        &contracts,
        &BTreeMap::default(),
    )
    .expect("projects");
    let source: String = emit_rust_client(&ir)
        .expect("emits")
        .iter()
        .map(|file| String::from_utf8(file.bytes().to_vec()).expect("UTF-8"))
        .collect();

    assert!(
        source.contains("pub freight_terms: Option<String>"),
        "a contract field did not reach the emitted request type"
    );
    assert!(
        source.contains("path: \"freight_terms\""),
        "a contract field did not reach the emitted descriptors"
    );
}

/// ACCEPTANCE: a new error appears in the types from the IR alone.
#[test]
fn an_error_added_to_a_contract_appears_without_a_hand_edit() {
    let package = fixture::generate_fixture();
    let mut contracts = fixture::contracts(&package);
    let path = "widget/get.errors.json";
    let mut document: serde_json::Value = serde_json::from_slice(&contracts[path]).expect("parse");
    document["cases"]
        .as_array_mut()
        .expect("the errors contract carries a cases array")
        .push(serde_json::json!({ "detail": {}, "literal": "quota_exhausted" }));
    contracts.insert(
        path.to_owned(),
        serde_json::to_vec(&document).expect("serialize"),
    );

    let ir = ClientContractIr::from_release_contracts(
        "platform_fixture",
        &contracts,
        &BTreeMap::default(),
    )
    .expect("projects");
    let source: String = emit_rust_client(&ir)
        .expect("emits")
        .iter()
        .map(|file| String::from_utf8(file.bytes().to_vec()).expect("UTF-8"))
        .collect();

    assert!(
        source.contains("\"quota_exhausted\""),
        "a contract error did not reach the emitted refusals"
    );
}

/// Emission is byte-stable: the same IR emits the same bytes.
#[test]
fn emission_is_byte_stable() {
    let ir = release();
    let first = emit_rust_client(&ir).expect("emits");
    let second = emit_rust_client(&ir).expect("emits");
    assert_eq!(first, second);
}

#[test]
fn valid_operations_cannot_collide_with_route_helpers_or_their_fallbacks() {
    let mut ir = release();
    ir.models.retain(|model| model.name == "widget");
    let model = &mut ir.models[0];
    let template = model
        .operations
        .iter()
        .find(|operation| operation.name == "get")
        .unwrap()
        .clone();
    model.operations = [
        "get",
        "get_route",
        "get_route_route",
        "__wamn_route_get",
        "__wamn_route_get_1",
    ]
    .into_iter()
    .map(|name| {
        let mut operation = template.clone();
        operation.name = name.into();
        operation.operation = format!("example:widget/{name}@1.0.0");
        operation.route.as_mut().unwrap().template = format!("/widget/{name}");
        operation
    })
    .collect();
    let first = emit_rust_client(&ir).unwrap();
    ir.models[0].operations.reverse();
    let reversed = emit_rust_client(&ir).unwrap();
    for files in [&first, &reversed] {
        let source = std::str::from_utf8(files[0].bytes()).unwrap();
        for (name, helper) in [
            ("get", "__wamn_route_get_2"),
            ("get_route", "__wamn_route_get_route_1"),
            ("get_route_route", "get_route_route_route"),
            ("__wamn_route_get", "__wamn_route_get_route"),
            ("__wamn_route_get_1", "__wamn_route_get_1_route"),
        ] {
            assert!(source.contains(&format!("pub async fn {name}(")));
            assert!(source.contains(&format!("pub fn {helper}() -> RouteMetadata")));
            // The invoke call is formatted, so a long helper name wraps its
            // arguments onto their own lines. The argument itself is the claim.
            assert!(source.contains(&format!("&{helper}(),")));
        }
    }
    check_compiles(&ir, "wamn-emitted-client-route-collision");
}

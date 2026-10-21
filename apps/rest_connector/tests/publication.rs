//! Admit the built template guests and gate their scheduled wiring together.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Value, json};
use wamn_catalog::{ComponentPackageScope, WiringDocument};
use wamn_control::component_declaration::{
    render_declaration_document, render_palette_declaration,
};
use wamn_schema_generator::operation_reference::{AuthoredDocument, read_authored_document};

#[test]
fn scheduled_rest_template_admits_and_connects_the_array_command() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let package = repository.join("apps/rest_connector");
    let output = wamn_schema_generator::output_root(&package);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(output.join("wamn.json")).unwrap()).unwrap();
    let version = manifest["package"]["version"].as_str().unwrap();
    assert_eq!(
        manifest["workflows"]["item"],
        json!({"wiring":"rest_item","schedule":"* * * * *"})
    );
    assert_eq!(
        manifest["custom_operations"]["item.record"]["visibility"],
        "private"
    );
    assert!(manifest["custom_operations"]["item.record"]["permission"].is_null());
    let scope = ComponentPackageScope {
        tenant_id: "rest-publication".into(),
        package_id: "rest_connector".into(),
        package_version: version.into(),
    };
    let engine = wamn_engine::build_engine(&[]).unwrap();
    let mut facts = Vec::new();
    for (name, template) in [
        (
            "http-page",
            repository.join("apps/platform/no-std/http-page/declaration.json.in"),
        ),
        (
            "jsonata",
            repository.join("apps/platform/execution/jsonata/declaration.json.in"),
        ),
        (
            "rest_connector",
            package.join("publication/components/rest_connector.json.in"),
        ),
    ] {
        let declaration = if name == "rest_connector" {
            render_declaration_document(&template, &scope.tenant_id).unwrap()
        } else {
            render_palette_declaration(&template, &scope, None).unwrap()
        };
        if name == "rest_connector" {
            assert_eq!(
                declaration["operations"][format!("rest-connector:item/record@{version}")]["input-ports"]
                    [0]["schema"],
                json!({"type":"array"})
            );
        }
        let bytes = std::fs::read(output.join(format!("components/{name}.wasm")))
            .expect("run wamn build apps/rest_connector before the publication test");
        if name == "rest_connector" {
            // The full admission also binds the manifest's statements: a private
            // command must not declare a registered operation.
            let rendered = std::env::temp_dir().join(format!(
                "rest-connector-declaration-{}.json",
                std::process::id()
            ));
            std::fs::write(&rendered, serde_json::to_vec(&declaration).unwrap()).unwrap();
            let admitted = wamn_control::component_admission::admit_component(
                wamn_control::component_admission::AdmitComponentRequest {
                    package: package.clone(),
                    component_bytes: output.join("components/rest_connector.wasm"),
                    declaration: rendered.clone(),
                    admitted_platform_packages: vec![
                        "wamn:node".to_owned(),
                        "wamn:postgres".to_owned(),
                    ],
                    base_digests: BTreeSet::new(),
                },
            );
            std::fs::remove_file(&rendered).unwrap();
            admitted.unwrap_or_else(|error| panic!("admit the rest_connector package: {error:#}"));
        }
        let admitted = wamn_engine::component_admission::validate_component_admission(
            &engine,
            &bytes,
            wamn_engine::component_admission::ComponentAdmissionRequest {
                declaration: serde_json::from_value(declaration).unwrap(),
                admitted_platform_packages: [
                    "wamn:node".to_owned(),
                    "wamn:connection".to_owned(),
                    "wamn:postgres".to_owned(),
                ]
                .into(),
                base_digests: BTreeSet::new(),
            },
        )
        .unwrap_or_else(|error| panic!("admit {name}: {error:#}"));
        facts.push(admitted.component);
    }
    let graph: WiringDocument = serde_json::from_value(
        read_authored_document(
            &package.join("publication/wirings/rest_item.json"),
            AuthoredDocument::Wiring,
        )
        .unwrap(),
    )
    .unwrap();
    wamn_control::author_wiring::gate_wiring_document(&graph, &scope, &facts).unwrap();
    assert_eq!(graph.entry, "page");
    assert_eq!(graph.nodes["rows"].params["port"], "items");
    assert_eq!(graph.edges[1].from_port, "items");
    // The exact-schema gate must reject the generic JSONata port for this command.
    let mut wrong_port = graph;
    wrong_port.edges[1].from_port = "main".into();
    assert!(
        wamn_control::author_wiring::gate_wiring_document(&wrong_port, &scope, &facts).is_err()
    );
}

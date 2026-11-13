//! `wamn build` of Receiving records the components that step 7 compiled,
//! and every one it records is the virtualized guest
//! (docs/operations/building.md).
//!
//! The build reads the virtualization allowlist from
//! the xtask virtualization list and adds to it each component that
//! the package owns. A virtualized guest imports no ambient `wasi:cli`
//! interface. This test builds Receiving into its workspace output against a
//! server of its own and reads the result from `build.json` alone.

use std::collections::BTreeSet;
use std::fs;

use serde_json::Value;
use wamn_build::{BuildRequest, build_package, default_output_root, package_output};

/// The top-level imports of a component.
fn component_imports(bytes: &[u8]) -> Vec<String> {
    let mut imports = Vec::new();
    let mut depth = 0_u32;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        match payload.expect("parse a recorded component") {
            wasmparser::Payload::ModuleSection { .. }
            | wasmparser::Payload::ComponentSection { .. } => depth += 1,
            wasmparser::Payload::End(_) => depth = depth.saturating_sub(1),
            wasmparser::Payload::ComponentImportSection(section) if depth == 0 => {
                for import in section {
                    imports.push(import.expect("read an import").name.name.to_owned());
                }
            }
            _ => {}
        }
    }
    imports
}

#[tokio::test(flavor = "current_thread")]
async fn the_receiving_build_records_its_components_and_virtualizes_every_recorded_component() {
    let database = wamn_test_postgres::database();
    let package = wamn_fixture_package::repository_root().join("apps/wamn_receiving");
    let output_root = default_output_root(&package);
    build_package(&BuildRequest::new(&package, &output_root, database.url()))
        .await
        .expect("wamn build of Receiving");
    let output = package_output(&output_root, "wamn_receiving");
    let build_record: Value =
        serde_json::from_slice(&fs::read(output.join("build.json")).expect("read build.json"))
            .expect("parse build.json");
    let manifest: Value =
        serde_json::from_slice(&fs::read(output.join("wamn.json")).expect("read wamn.json"))
            .expect("parse wamn.json");

    let owned = manifest["components"]
        .as_object()
        .expect("Receiving owns components")
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert!(!owned.is_empty(), "Receiving owns a component");
    let recorded = build_record["outputs"]["components"]
        .as_array()
        .expect("build.json records its components");
    let names = recorded
        .iter()
        .map(|component| {
            component["name"]
                .as_str()
                .expect("a component name")
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(names, owned, "build.json records package-owned components");

    // The owned components are in the allowlist, so each recorded guest of
    // theirs is the virtualized one.
    for component in recorded {
        let name = component["name"].as_str().expect("a component name");
        let path = output.join(component["path"].as_str().expect("a component path"));
        let ambient = component_imports(&fs::read(&path).expect("read a recorded component"))
            .into_iter()
            .filter(|import| import.starts_with("wasi:cli/"))
            .collect::<Vec<_>>();
        assert!(
            ambient.is_empty(),
            "{name} is not virtualized: it imports {ambient:?}"
        );
    }
}

//! `wamn build` of Receiving records the components that step 7 compiled,
//! and every one it records is the virtualized guest
//! (docs/operations/building.md).
//!
//! The build reads the virtualization allowlist from
//! `tools/component-virtualization.json` and adds to it each component that
//! the package owns. A virtualized guest imports no ambient `wasi:cli`
//! interface. This test builds Receiving into a scratch output against a
//! server of its own and reads the result from `build.json` alone.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde_json::Value;
use wamn_schema_generator::build::{BuildRequest, build_package, package_output};

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

/// The palette components that the package's wirings name.
fn wired_components(package: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let Ok(entries) = fs::read_dir(package.join("publication/wirings")) else {
        return names;
    };
    for entry in entries {
        let wiring: Value = serde_json::from_slice(
            &fs::read(entry.expect("read a wiring entry").path()).expect("read a wiring"),
        )
        .expect("parse a wiring");
        for node in wiring["nodes"]
            .as_object()
            .into_iter()
            .flat_map(|nodes| nodes.values())
        {
            if let Some(component) = node["component"].as_str() {
                names.insert(component.to_owned());
            }
        }
    }
    names
}

#[tokio::test(flavor = "current_thread")]
async fn the_receiving_build_records_its_components_and_virtualizes_every_recorded_component() {
    let database = wamn_test_postgres::database();
    let output_root = std::env::temp_dir().join(format!(
        "wamn-conformance-receiving-build-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&output_root);
    let package = wamn_fixture_package::repository_root().join("apps/wamn_receiving");
    build_package(&BuildRequest::new(&package, &output_root, database.url()))
        .await
        .expect("wamn build of Receiving");
    let output = package_output(&output_root, "wamn_receiving");
    let receipt: Value =
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
    let palette = wired_components(&package)
        .into_iter()
        .filter(|name| !owned.contains(name))
        .collect::<BTreeSet<_>>();

    let recorded = receipt["outputs"]["components"]
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
    let expected = owned.union(&palette).cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        names, expected,
        "build.json records the owned components and the wired palette"
    );

    // The owned components are in the allowlist, so each recorded guest of
    // theirs is the virtualized one.
    for component in recorded {
        let name = component["name"].as_str().expect("a component name");
        if palette.contains(name) {
            continue;
        }
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
    fs::remove_dir_all(&output_root).expect("remove the build output");
}

//! Fixture input for control unit tests.

pub fn fixture_manifest() -> Vec<u8> {
    wamn_schema_generator::build_record::require_current(&wamn_fixture_package::package_root())
        .unwrap_or_else(|error| panic!("{error:#}"));
    wamn_fixture_package::manifest_bytes()
}

/// Copy a handwritten schema fixture, which has no generated package output.
#[cfg(test)]
pub(crate) fn copy_schema_fixture(root: &std::path::Path) {
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/apply_package/base");
    let directory = crate::apply_package::read_package_directory(&source).unwrap();
    // The empty local directory keeps contract lookup within this fixture.
    std::fs::create_dir_all(root.join("generated")).unwrap();
    std::fs::create_dir_all(root.join("migrations")).unwrap();
    std::fs::write(root.join("wamn.json"), directory.manifest_bytes).unwrap();
    for migration in directory.migrations {
        std::fs::write(root.join(migration.relative_path), migration.bytes).unwrap();
    }
}

use std::collections::BTreeMap;
use wamn_catalog::{AdmittedComponent, AdmittedComponentOperation, ComponentPackageCoordinate};

pub const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

pub fn closure_component(component: &str, registered_operation: Option<&str>) -> AdmittedComponent {
    let operation = registered_operation.unwrap_or("run");
    AdmittedComponent {
        target: ComponentPackageCoordinate {
            project_env_key: "org-a--fixture--dev".to_owned(),
            package_id: "base".to_owned(),
            package_version: "1.0.0".to_owned(),
        },
        component: component.to_owned(),
        interface_version: "0.1.0".to_owned(),
        operations: BTreeMap::from([(
            operation.to_owned(),
            AdmittedComponentOperation {
                committed_result_schema: None,
                registered_operation: registered_operation.map(str::to_owned),
                host_transaction: false,
                input_ports: Vec::new(),
                output_ports: Vec::new(),
                parameters: Vec::new(),
                statements: BTreeMap::new(),
            },
        )]),
        component_digest: DIGEST.to_owned(),
        imports: Vec::new(),
        imports_fingerprint: DIGEST.to_owned(),
        effects: Vec::new(),
    }
}

pub fn source_manifest() -> wamn_catalog::manifest::PackageManifest {
    serde_json::from_str(include_str!(
        "../tests/fixtures/component_package/wamn.json"
    ))
    .expect("the fixture source manifest parses")
}

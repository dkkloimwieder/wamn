//! Fixture input for control unit tests.

pub(crate) fn fixture_manifest() -> Vec<u8> {
    wamn_schema_generator::receipt::check_current(&wamn_fixture_package::package_root())
        .unwrap_or_else(|error| panic!("{error:#}"));
    wamn_fixture_package::manifest_bytes()
}

/// Record output that a test writes, without compiling a guest.
pub(crate) fn write_receipt(root: &std::path::Path) {
    use wamn_schema_generator::receipt;

    let manifest_path = wamn_schema_generator::package_manifest_path(root);
    let manifest_bytes = std::fs::read(manifest_path).unwrap();
    let manifest = wamn_schema_generator::PackageManifest::from_slice(&manifest_bytes).unwrap();
    let output = wamn_schema_generator::output_root(root);
    std::fs::create_dir_all(&output).unwrap();
    let receipt = receipt::BuildReceipt {
        schema: receipt::RECEIPT_SCHEMA.to_owned(),
        package: receipt::ReceiptPackage {
            id: manifest.package.id,
            version: manifest.package.version,
        },
        inputs: receipt::ReceiptInputs {
            digest: receipt::input_digest(root).unwrap(),
            files: std::collections::BTreeMap::new(),
        },
        dependencies: Vec::new(),
        generator: receipt::ReceiptGenerator {
            id: "test".into(),
            toolchain: "test".into(),
            sqlx: "0.9.0".into(),
        },
        outputs: receipt::ReceiptOutputs {
            manifest: wamn_engine::component_admission::component_digest(&manifest_bytes),
            files: std::collections::BTreeMap::new(),
            components: Vec::new(),
            web: None,
        },
    };
    std::fs::write(output.join(receipt::RECEIPT_FILE), receipt.to_bytes()).unwrap();
}

/// Copy a handwritten schema fixture, which has no generated package output.
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

/// Compile the authored fixture and record the output that this test writes.
pub(crate) fn compile_package(root: &std::path::Path) {
    let bytes = wamn_schema_generator::compile_manifest(root).unwrap();
    let output = wamn_schema_generator::output_root(root);
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(wamn_schema_generator::package_manifest_path(root), bytes).unwrap();
    write_receipt(root);
}

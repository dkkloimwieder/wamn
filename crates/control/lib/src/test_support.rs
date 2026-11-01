//! Fixture input for control unit tests.

pub(crate) fn fixture_manifest() -> Vec<u8> {
    wamn_schema_generator::receipt::check_current(&wamn_fixture_package::package_root())
        .unwrap_or_else(|error| panic!("{error:#}"));
    wamn_fixture_package::manifest_bytes()
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

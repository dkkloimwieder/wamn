//! Shared fixture and its local checks.

include!("environment_fixture.rs");
#[test]
fn disposable_component_declarations_follow_built_base_bytes() -> anyhow::Result<()> {
    let root = ScratchRoot(
        std::env::temp_dir().join(format!("journey-built-base-digests-{}", std::process::id())),
    );
    std::fs::create_dir(root.path())?;
    let artifacts = root.path().join("components");
    std::fs::create_dir(&artifacts)?;
    let manifest = wamn_schema_generator::package_manifest_path(&overlay_package_root());
    wamn_schema_generator::build_record::check_current(&overlay_package_root())?;
    let template = overlay_package_root()
        .join("publication/components")
        .join(format!("{OVERLAY_COMPONENT}.json.in"));
    let authored_manifest = std::fs::read(&manifest)?;
    let authored_template = std::fs::read(&template)?;
    assert!(render_component_declarations(root.path(), &artifacts).is_err());

    // Two distinct component binaries; the second adds an empty custom section.
    for bytes in [
        b"\0asm\x0d\0\x01\0".as_slice(),
        b"\0asm\x0d\0\x01\0\0\x02\x01x".as_slice(),
    ] {
        std::fs::write(artifacts.join(format!("{BASE_COMPONENT}.wasm")), bytes)?;
        let declarations = render_component_declarations(root.path(), &artifacts)?;
        let overlay = declarations
            .iter()
            .find(|declaration| declaration.package.id == OVERLAY_PACKAGE_ID)
            .expect("the journey renders its overlay");
        assert_eq!(
            overlay.base_digests,
            std::collections::BTreeSet::from([wamn_engine::component_admission::component_digest(
                bytes
            )]),
        );
    }
    assert_eq!(std::fs::read(manifest)?, authored_manifest);
    assert_eq!(std::fs::read(template)?, authored_template);
    Ok(())
}

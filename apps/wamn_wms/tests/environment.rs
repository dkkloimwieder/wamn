//! Shared fixture and its local checks.

include!("environment_fixture.rs");
#[test]
fn the_wms_declaration_takes_its_version_from_the_package() -> anyhow::Result<()> {
    let output = std::env::temp_dir().join(format!("wms-declaration-{}.json", std::process::id()));
    let rendered = wms_component_declaration(&package_root(), "wms", &output)
        .and_then(|()| Ok(serde_json::from_slice::<Value>(&fs::read(&output)?)?));
    let _ = fs::remove_file(&output);
    let scope = &rendered?["scope"];
    assert_eq!(scope["env_key"], "wms");
    assert_eq!(scope["package-id"], "wamn_wms");
    assert_eq!(
        scope["package-version"].as_str(),
        Some(package_coordinate()?.package_version())
    );
    Ok(())
}

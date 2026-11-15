use super::{CompositionError, PackageInput, PathBuf, SelectedComponentArtifact};

pub(super) fn component_entry<'a>(
    package: &PackageInput,
    component: &str,
    build_record: &'a wamn_compiler::build_record::BuildRecord,
) -> Result<(PathBuf, &'a wamn_compiler::build_record::BuildComponent), CompositionError> {
    let output = wamn_compiler::output_root(&package.root);
    let entry = build_record
        .outputs
        .components
        .iter()
        .find(|entry| entry.name == component)
        .ok_or_else(|| {
            CompositionError::invalid(
                "select component artifact",
                format!(
                    "the build of {}@{} records no component {component}; run wamn build",
                    package.manifest.package.id, package.manifest.package.version
                ),
            )
        })?;
    let path = output.join(&entry.path);
    Ok((path, entry))
}

pub(super) fn built_component(
    package: &PackageInput,
    component: &str,
    path: PathBuf,
    entry: &wamn_compiler::build_record::BuildComponent,
    bytes: &[u8],
) -> Result<SelectedComponentArtifact, CompositionError> {
    let digest = wamn_digest::bytes_sha256(bytes);
    if digest != entry.sha256 {
        return Err(CompositionError::invalid(
            "read built component",
            format!(
                "{} is {digest}, and build.json records {}; run wamn build",
                path.display(),
                entry.sha256
            ),
        ));
    }
    Ok(SelectedComponentArtifact {
        package_id: package.manifest.package.id.clone().into_boxed_str(),
        package_version: package.manifest.package.version.clone().into_boxed_str(),
        component: component.into(),
        path,
        digest: digest.into_boxed_str(),
    })
}

pub(super) fn selected_component(package: &PackageInput) -> Result<&str, CompositionError> {
    if package.manifest.components.len() != 1 {
        return Err(CompositionError::invalid(
            "select package component artifact",
            format!(
                "{}@{} must declare exactly one component for the POC loop",
                package.manifest.package.id, package.manifest.package.version
            ),
        ));
    }
    let component = package
        .manifest
        .components
        .keys()
        .next()
        .expect("one package component was required above");
    Ok(component)
}

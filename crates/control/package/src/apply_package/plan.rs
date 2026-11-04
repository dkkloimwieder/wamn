use super::{
    PackageDirectory, PackageManifest, PackageRoot, PreparedPackage, plan_package_migrations,
    validate_migration_policy,
};
use anyhow::Context as _;

pub(super) fn prepare_directory(
    root: &PackageRoot,
    directory: PackageDirectory,
) -> anyhow::Result<PreparedPackage> {
    let presented = plan_package_migrations(&directory, None)
        .context("validate package directory before database work")?;
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes)
        .context("parse strict package manifest for definition ownership")?;
    wamn_catalog::manifest::validate_operation_vocabulary(&manifest)
        .context("validate package manifest for registration projection")?;
    let policy = validate_migration_policy(&root.path, &directory, &presented)?;
    Ok(PreparedPackage {
        component_source: None,
        root: root.path.clone(),
        directory,
        manifest,
        policy,
    })
}

//! Package manifest bytes and generated metadata.

use super::{BTreeMap, MintManifestError, MintManifestErrorKind, PathBuf, sha256};

pub(super) fn read_package_manifests(
    paths: &[PathBuf],
) -> Result<
    (
        BTreeMap<String, wamn_schema_generator::PackageManifest>,
        BTreeMap<String, String>,
    ),
    MintManifestError,
> {
    let mut manifests = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    for path in paths {
        let bytes = std::fs::read(path).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::PackageManifest,
                format!("read package manifest {}", path.display()),
                error,
            )
        })?;
        let manifest =
            wamn_schema_generator::PackageManifest::from_slice(&bytes).map_err(|error| {
                MintManifestError::with_source(
                    MintManifestErrorKind::PackageManifest,
                    format!("parse package manifest {}", path.display()),
                    error,
                )
            })?;
        wamn_schema_generator::validate_operation_vocabulary(&manifest).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::PackageManifest,
                format!("validate package manifest {}", path.display()),
                error,
            )
        })?;
        let root = path.parent().ok_or_else(|| {
            MintManifestError::new(
                MintManifestErrorKind::GeneratedPackageMetadata,
                format!(
                    "package manifest {} has no package directory; pass package-owned wamn.json",
                    path.display()
                ),
            )
        })?;
        let metadata_path = root.join("generated/package-weld.json");
        let metadata_bytes = std::fs::read(&metadata_path).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::GeneratedPackageMetadata,
                format!(
                    "package {}@{} requires generated/package-weld.json at {}; regenerate the package evidence",
                    manifest.package.id,
                    manifest.package.version,
                    metadata_path.display()
                ),
                error,
            )
        })?;
        let metadata = wamn_schema_generator::GeneratedPackageMetadata::from_slice(&metadata_bytes).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::GeneratedPackageMetadata,
                format!(
                    "package {}@{} carries an invalid generated/package-weld.json; regenerate the package evidence",
                    manifest.package.id, manifest.package.version
                ),
                error,
            )
        })?;
        validate_package_metadata(&manifest, &metadata)?;
        let package_id = manifest.package.id.clone();
        if manifests.insert(package_id.clone(), manifest).is_some() {
            return Err(MintManifestError::new(
                MintManifestErrorKind::PackageManifest,
                format!("more than one package manifest names {package_id:?}"),
            ));
        }
        hashes.insert(package_id, sha256(&bytes));
    }
    Ok((manifests, hashes))
}

pub(super) fn validate_package_metadata(
    manifest: &wamn_schema_generator::PackageManifest,
    metadata: &wamn_schema_generator::GeneratedPackageMetadata,
) -> Result<(), MintManifestError> {
    let coordinate = format!("{}@{}", manifest.package.id, manifest.package.version);
    if metadata.required_platform_policy_contract() != &manifest.required_platform_policy_contract {
        return Err(MintManifestError::new(
            MintManifestErrorKind::GeneratedPackageMetadata,
            format!(
                "package {coordinate} manifest and generated weld disagree on the required platform policy contract; regenerate the package evidence"
            ),
        ));
    }
    if !metadata.promotion_eligible() {
        return Err(MintManifestError::new(
            MintManifestErrorKind::PolicyContractUnsatisfied,
            format!(
                "package {coordinate} requires platform policy contract {:?} in state unsatisfied; reconcile its generated policy and regenerate with state satisfied",
                manifest.required_platform_policy_contract.id
            ),
        ));
    }
    Ok(())
}

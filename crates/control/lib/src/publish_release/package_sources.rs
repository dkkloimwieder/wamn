//! Package manifest bytes and generated metadata.

use std::path::Path;

use super::{
    BTreeMap, MintManifestError, MintManifestErrorKind, OperationKind, PathBuf, RouteKinds, sha256,
};

/// Every package's parsed manifest, every package's manifest digest, and the
/// contract kind of every operation the packages generate.
pub(super) type PackageManifestSources = (
    BTreeMap<String, wamn_schema_generator::PackageManifest>,
    BTreeMap<String, String>,
    RouteKinds,
);

pub(super) fn read_package_manifests(
    paths: &[PathBuf],
) -> Result<PackageManifestSources, MintManifestError> {
    let mut manifests = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    let mut kinds = RouteKinds::new();
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
        for (operation, kind) in read_operation_kinds(root)? {
            kinds.insert((package_id.clone(), operation), kind);
        }
        if manifests.insert(package_id.clone(), manifest).is_some() {
            return Err(MintManifestError::new(
                MintManifestErrorKind::PackageManifest,
                format!("more than one package manifest names {package_id:?}"),
            ));
        }
        hashes.insert(package_id, sha256(&bytes));
    }
    Ok((manifests, hashes, kinds))
}

/// The `kind` of every generated contract `operation.json` under the package.
///
/// The generated contract is the only source of an operation kind. A package
/// that generates no contract has no operation a route can call.
fn read_operation_kinds(root: &Path) -> Result<Vec<(String, OperationKind)>, MintManifestError> {
    #[derive(serde::Deserialize)]
    struct Contract {
        operation: String,
        kind: OperationKind,
    }
    let unreadable = |path: &Path, error: std::io::Error| {
        MintManifestError::with_source(
            MintManifestErrorKind::GeneratedPackageMetadata,
            format!("read generated contracts {}", path.display()),
            error,
        )
    };
    let contracts = root.join("generated/contracts");
    let models = match std::fs::read_dir(&contracts) {
        Ok(models) => models,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(unreadable(&contracts, error)),
    };
    let mut kinds = Vec::new();
    for model in models {
        let model = model.map_err(|error| unreadable(&contracts, error))?.path();
        if !model.is_dir() {
            continue;
        }
        for contract in std::fs::read_dir(&model).map_err(|error| unreadable(&model, error))? {
            let path = contract.map_err(|error| unreadable(&model, error))?.path();
            if !path.to_string_lossy().ends_with(".operation.json") {
                continue;
            }
            let bytes = std::fs::read(&path).map_err(|error| unreadable(&path, error))?;
            let contract: Contract = serde_json::from_slice(&bytes).map_err(|error| {
                MintManifestError::with_source(
                    MintManifestErrorKind::GeneratedPackageMetadata,
                    format!(
                        "generated contract {} names no operation kind; regenerate the package evidence",
                        path.display()
                    ),
                    error,
                )
            })?;
            kinds.push((contract.operation, contract.kind));
        }
    }
    Ok(kinds)
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
                "package {coordinate} manifest and generated package contract disagree on the required platform policy contract; regenerate the package evidence"
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

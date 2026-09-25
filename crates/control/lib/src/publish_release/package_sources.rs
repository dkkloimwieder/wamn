//! Package manifest bytes and generated metadata.

use std::path::Path;

use wamn_record_history::{HISTORY_TABLE_SUFFIX, is_history_table_name};

use super::{
    BTreeMap, BTreeSet, MintManifestError, MintManifestErrorKind, OperationKind, PathBuf,
    RouteContract, RouteContracts, ServingRelation, ServingRoute, sha256,
};

/// Every package's parsed manifest, every package's manifest digest, and the
/// contract kind of every operation the packages generate.
pub(super) type PackageManifestSources = (
    BTreeMap<String, wamn_schema_generator::PackageManifest>,
    BTreeMap<String, String>,
    RouteContracts,
);

pub(super) fn read_package_manifests(
    paths: &[PathBuf],
) -> Result<PackageManifestSources, MintManifestError> {
    let mut manifests = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    let mut kinds = RouteContracts::new();
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
        for (operation, contract) in read_operation_contracts(root)? {
            kinds.insert((package_id.clone(), operation), contract);
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

/// The manifest route of one route attachment of the package at `root`, as
/// publish writes it from the generated contract of its operation.
///
/// A local application assembly calls it, so a test release carries the same
/// route facts as a published one.
///
/// # Errors
///
/// Returns [`MintManifestError`] when no generated contract declares the
/// operation, or a contract cannot be read.
pub fn package_route(
    root: &Path,
    package_id: &str,
    component: &str,
    operation: &str,
) -> Result<ServingRoute, MintManifestError> {
    let (_, contract) = read_operation_contracts(root)?
        .into_iter()
        .find(|(declared, _)| declared == operation)
        .ok_or_else(|| {
            MintManifestError::new(
                MintManifestErrorKind::GeneratedPackageMetadata,
                format!(
                    "no generated contract of package {package_id:?} declares operation {operation:?}; regenerate the package evidence"
                ),
            )
        })?;
    Ok(ServingRoute {
        package_id: package_id.to_owned(),
        component: component.to_owned(),
        operation: operation.to_owned(),
        kind: contract.kind,
        reads: contract.reads,
        revision: contract.revision,
    })
}

/// The route facts of every generated contract `operation.json` under the package.
///
/// The generated contract is the only source of an operation kind, of the
/// relations a read reads, and of the revision field of a `get`. A package
/// that generates no contract has no operation a route can call.
fn read_operation_contracts(
    root: &Path,
) -> Result<Vec<(String, RouteContract)>, MintManifestError> {
    #[derive(serde::Deserialize)]
    struct Contract {
        operation: String,
        kind: OperationKind,
        #[serde(default)]
        relations: Vec<Relation>,
        #[serde(default)]
        record: Option<Record>,
    }
    #[derive(serde::Deserialize)]
    struct Relation {
        schema: String,
        table: String,
    }
    #[derive(serde::Deserialize)]
    struct Record {
        revision_field: Option<String>,
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
            let reads = if contract.kind.is_read() {
                contract
                    .relations
                    .into_iter()
                    .map(|relation| {
                        // A history table changes in the same transaction as
                        // its model relation, whose version a read keys on.
                        let table = if is_history_table_name(&relation.table) {
                            relation.table[..relation.table.len() - HISTORY_TABLE_SUFFIX.len()]
                                .to_owned()
                        } else {
                            relation.table
                        };
                        ServingRelation {
                            schema: relation.schema,
                            relation: table,
                        }
                    })
                    .collect()
            } else {
                BTreeSet::new()
            };
            let revision = contract
                .record
                .and_then(|record| record.revision_field)
                .filter(|_| contract.kind == OperationKind::Get);
            kinds.push((
                contract.operation,
                RouteContract {
                    kind: contract.kind,
                    reads,
                    revision,
                },
            ));
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use wamn_catalog::{OperationKind, ServingRelation};

    use super::package_route;

    fn app(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps")
            .join(name)
    }

    fn relations(names: &[(&str, &str)]) -> BTreeSet<ServingRelation> {
        names
            .iter()
            .map(|(schema, relation)| ServingRelation {
                schema: (*schema).to_owned(),
                relation: (*relation).to_owned(),
            })
            .collect()
    }

    /// A route carries the relations its read reads and the revision field of
    /// its `get`, from the generated contract that publish reads.
    #[test]
    fn a_read_route_carries_its_relations_and_a_get_its_revision() {
        let wms = app("wamn_wms");
        let get = package_route(&wms, "wamn_wms", "wms", "wamn-wms:pallet/get@1.0.0")
            .expect("the pallet get has a contract");
        assert_eq!(get.kind, OperationKind::Get);
        assert_eq!(get.revision.as_deref(), Some("row_version"));
        assert_eq!(get.reads, relations(&[("wms", "pallet")]));

        let query = package_route(&wms, "wamn_wms", "wms", "wamn-wms:pallet/query@1.0.0")
            .expect("the pallet query has a contract");
        assert_eq!((query.kind, query.revision), (OperationKind::Query, None));
        assert_eq!(query.reads, relations(&[("wms", "pallet")]));

        let aggregate = package_route(
            &wms,
            "wamn_wms",
            "wms",
            "wamn-wms:inventory/aggregate@1.0.0",
        )
        .expect("the authored projection has a contract");
        assert_eq!(
            aggregate.reads,
            relations(&[("wms", "pallet"), ("wms", "pallet_quantity")])
        );

        let history = package_route(
            &app("wamn_receiving"),
            "wamn_receiving",
            "receiving",
            "wamn-receiving:receiving/load-purchase-order-history@1.0.0",
        )
        .expect("the history projection has a contract");
        assert_eq!(
            history.reads,
            relations(&[("receiving", "purchase_order")]),
            "a history table stands for its model relation"
        );

        let write = package_route(&wms, "wamn_wms", "wms", "wamn-wms:inventory/move@1.0.0")
            .expect("the move command has a contract");
        assert_eq!((write.reads, write.revision), (BTreeSet::new(), None));
    }
}

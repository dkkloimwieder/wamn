//! Package manifest bytes and generated metadata.

use std::path::Path;

use wamn_record_history::{HISTORY_TABLE_SUFFIX, is_history_table_name};

use super::{
    BTreeMap, BTreeSet, OperationType, PathBuf, PublishManifestError, PublishManifestErrorType,
    RouteCanonicalization, RouteContract, RouteContracts, ServingRelation, ServingRoute, sha256,
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
) -> Result<PackageManifestSources, PublishManifestError> {
    let mut manifests = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    let mut contracts = BTreeMap::new();
    for path in paths {
        let bytes = std::fs::read(path).map_err(|error| {
            PublishManifestError::with_source(
                PublishManifestErrorType::PackageManifest,
                format!("read package manifest {}", path.display()),
                error,
            )
        })?;
        let manifest =
            wamn_schema_generator::PackageManifest::from_slice(&bytes).map_err(|error| {
                PublishManifestError::with_source(
                    PublishManifestErrorType::PackageManifest,
                    format!("parse package manifest {}", path.display()),
                    error,
                )
            })?;
        wamn_schema_generator::validate_operation_vocabulary(&manifest).map_err(|error| {
            PublishManifestError::with_source(
                PublishManifestErrorType::PackageManifest,
                format!("validate package manifest {}", path.display()),
                error,
            )
        })?;
        let root = wamn_schema_generator::manifest_package_root(path).ok_or_else(|| {
            PublishManifestError::new(
                PublishManifestErrorType::GeneratedPackageMetadata,
                format!(
                    "package manifest {} has no package directory; pass package-owned wamn.json",
                    path.display()
                ),
            )
        })?;
        let metadata_path = root.join("generated/package-identity.json");
        let metadata_bytes = std::fs::read(&metadata_path).map_err(|error| {
            PublishManifestError::with_source(
                PublishManifestErrorType::GeneratedPackageMetadata,
                format!(
                    "package {}@{} requires generated/package-identity.json at {}; regenerate the package evidence",
                    manifest.package.id,
                    manifest.package.version,
                    metadata_path.display()
                ),
                error,
            )
        })?;
        let metadata = wamn_schema_generator::GeneratedPackageMetadata::from_slice(&metadata_bytes).map_err(|error| {
            PublishManifestError::with_source(
                PublishManifestErrorType::GeneratedPackageMetadata,
                format!(
                    "package {}@{} carries an invalid generated/package-identity.json; regenerate the package evidence",
                    manifest.package.id, manifest.package.version
                ),
                error,
            )
        })?;
        validate_package_metadata(&manifest, &metadata)?;
        let package_id = manifest.package.id.clone();
        contracts.insert(package_id.clone(), read_operation_contracts(root)?);
        if manifests.insert(package_id.clone(), manifest).is_some() {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::PackageManifest,
                format!("more than one package manifest names {package_id:?}"),
            ));
        }
        hashes.insert(package_id, sha256(&bytes));
    }
    let mut kinds = RouteContracts::new();
    for (package_id, package) in &contracts {
        for contract in package {
            let route = resolve_claim(&manifests[package_id], contract, |base| {
                contracts.get(base).map(Vec::as_slice).ok_or_else(|| {
                    PublishManifestError::new(
                        PublishManifestErrorType::PackageManifest,
                        format!(
                            "package {package_id:?} inherits a claim from package {base:?}, which the release does not carry"
                        ),
                    )
                })
            })?;
            kinds.insert((package_id.clone(), contract.operation.clone()), route);
        }
    }
    Ok((manifests, hashes, kinds))
}

/// The manifest route of one route attachment of the package at `root`, as
/// publish writes it from the generated contract of its operation.
///
/// A local application assembly calls it, so a test release carries the same
/// route facts as a published one. `roots` holds the directory of every
/// assembled package, so an inherited claim reads its base contract.
///
/// # Errors
///
/// Returns [`PublishManifestError`] when no generated contract declares the
/// operation, or a contract cannot be read.
pub fn package_route(
    roots: &BTreeMap<String, &Path>,
    package_id: &str,
    component: &str,
    operation: &str,
) -> Result<ServingRoute, PublishManifestError> {
    let unassembled = |package: &str| {
        PublishManifestError::new(
            PublishManifestErrorType::PackageManifest,
            format!("package {package:?} is not assembled"),
        )
    };
    let root = roots
        .get(package_id)
        .ok_or_else(|| unassembled(package_id))?;
    let contract = read_operation_contracts(root)?
        .into_iter()
        .find(|contract| contract.operation == operation)
        .ok_or_else(|| {
            PublishManifestError::new(
                PublishManifestErrorType::GeneratedPackageMetadata,
                format!(
                    "no generated contract of package {package_id:?} declares operation {operation:?}; regenerate the package evidence"
                ),
            )
        })?;
    let contract = if contract.inherited.is_some() {
        let path = wamn_schema_generator::package_manifest_path(root);
        let bytes = std::fs::read(&path).map_err(|error| {
            PublishManifestError::with_source(
                PublishManifestErrorType::PackageManifest,
                format!("read package manifest {}", path.display()),
                error,
            )
        })?;
        let manifest =
            wamn_schema_generator::PackageManifest::from_slice(&bytes).map_err(|error| {
                PublishManifestError::with_source(
                    PublishManifestErrorType::PackageManifest,
                    format!("parse package manifest {}", path.display()),
                    error,
                )
            })?;
        resolve_claim(&manifest, &contract, |base| {
            read_operation_contracts(roots.get(base).ok_or_else(|| unassembled(base))?)
        })?
    } else {
        contract.route
    };
    Ok(ServingRoute {
        package_id: package_id.to_owned(),
        component: component.to_owned(),
        operation: operation.to_owned(),
        type_: contract.kind,
        reads: contract.reads,
        revision: contract.revision,
        idempotency: contract.idempotency,
        canonicalization: contract.canonicalization,
        claim_operation: contract.claim_operation,
    })
}

/// One generated contract of a package, before an inherited claim reads its
/// base.
struct PackageContract {
    operation: String,
    /// The manifest name of the operation, `<model>.<name>`.
    declared: String,
    /// The base operation whose claim the operation inherits.
    inherited: Option<InheritedClaim>,
    route: RouteContract,
}

#[derive(serde::Deserialize)]
struct InheritedClaim {
    base: String,
    operation: String,
}

/// The route facts of `contract`. An inherited claim is the base's: the
/// route claims under the base operation, with the base key field and the
/// base canonical form, so one key through either route is one request.
fn resolve_claim<B: std::borrow::Borrow<[PackageContract]>>(
    manifest: &wamn_schema_generator::PackageManifest,
    contract: &PackageContract,
    base_contracts: impl FnOnce(&str) -> Result<B, PublishManifestError>,
) -> Result<RouteContract, PublishManifestError> {
    let Some(inherited) = &contract.inherited else {
        return Ok(contract.route.clone());
    };
    let refused = |detail: String| {
        PublishManifestError::new(PublishManifestErrorType::GeneratedPackageMetadata, detail)
    };
    let dependency = manifest
        .base_dependencies
        .get(&inherited.base)
        .ok_or_else(|| {
            refused(format!(
                "operation {:?} inherits a claim from undeclared base dependency {:?}",
                contract.operation, inherited.base
            ))
        })?;
    let base_contracts = base_contracts(&dependency.package)?;
    let base = base_contracts
        .borrow()
        .iter()
        .find(|base| base.declared == inherited.operation)
        .ok_or_else(|| {
            refused(format!(
                "operation {:?} inherits the claim of {:?}, which package {:?} does not generate",
                contract.operation, inherited.operation, dependency.package
            ))
        })?;
    Ok(RouteContract {
        idempotency: base.route.idempotency.clone(),
        canonicalization: base.route.canonicalization.clone(),
        claim_operation: Some(base.operation.clone()),
        ..contract.route.clone()
    })
}

/// The route facts of every generated contract `operation.json` under the package.
///
/// The generated contract is the only source of an operation kind, of the
/// relations a read reads, of the revision field of a `get`, and of the item
/// field that carries an idempotency key. A package that generates no contract
/// has no operation a route can call.
fn read_operation_contracts(root: &Path) -> Result<Vec<PackageContract>, PublishManifestError> {
    #[derive(serde::Deserialize)]
    struct Contract {
        operation: String,
        #[serde(rename = "type")]
        type_: OperationType,
        #[serde(default)]
        relations: Vec<Relation>,
        #[serde(default)]
        record: Option<Record>,
        #[serde(default)]
        idempotent_by: Option<IdempotentBy>,
    }
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum IdempotentBy {
        Inherited { inherited: InheritedClaim },
        Other(serde::de::IgnoredAny),
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
        PublishManifestError::with_source(
            PublishManifestErrorType::GeneratedPackageMetadata,
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
                PublishManifestError::with_source(
                    PublishManifestErrorType::GeneratedPackageMetadata,
                    format!(
                        "generated contract {} names no operation kind; regenerate the package evidence",
                        path.display()
                    ),
                    error,
                )
            })?;
            let reads = if contract.type_.is_read() {
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
                .filter(|_| contract.type_ == OperationType::Get);
            let (idempotency, canonicalization) = if contract.type_.is_read() {
                (None, None)
            } else {
                read_input_identity(&path)?
            };
            let inherited = match contract.idempotent_by {
                Some(IdempotentBy::Inherited { inherited }) => Some(inherited),
                _ => None,
            };
            let stem = path.file_name().unwrap_or_default().to_string_lossy();
            let declared = format!(
                "{}.{}",
                model.file_name().unwrap_or_default().to_string_lossy(),
                &stem[..stem.len() - ".operation.json".len()]
            );
            kinds.push(PackageContract {
                operation: contract.operation,
                declared,
                inherited,
                route: RouteContract {
                    kind: contract.type_,
                    reads,
                    revision,
                    idempotency,
                    canonicalization,
                    claim_operation: None,
                },
            });
        }
    }
    Ok(kinds)
}

/// The idempotency key field and the canonical item form of the input
/// contract beside `operation_path`.
///
/// An operation with no input contract declares neither.
fn read_input_identity(
    operation_path: &Path,
) -> Result<(Option<String>, Option<RouteCanonicalization>), PublishManifestError> {
    let name = operation_path.to_string_lossy();
    let input_path = std::path::PathBuf::from(format!(
        "{}.input.json",
        &name[..name.len() - ".operation.json".len()]
    ));
    let bytes = match std::fs::read(&input_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((None, None)),
        Err(error) => {
            return Err(PublishManifestError::with_source(
                PublishManifestErrorType::GeneratedPackageMetadata,
                format!("read generated contract {}", input_path.display()),
                error,
            ));
        }
    };
    let contract = serde_json::from_slice(&bytes).map_err(|error| {
        PublishManifestError::with_source(
            PublishManifestErrorType::GeneratedPackageMetadata,
            format!(
                "generated contract {} is not JSON; regenerate the package evidence",
                input_path.display()
            ),
            error,
        )
    })?;
    Ok((
        wamn_schema_generator::client_plan::idempotency_field(&contract),
        wamn_schema_generator::client_plan::route_canonicalization(&contract),
    ))
}

pub(super) fn validate_package_metadata(
    manifest: &wamn_schema_generator::PackageManifest,
    metadata: &wamn_schema_generator::GeneratedPackageMetadata,
) -> Result<(), PublishManifestError> {
    let coordinate = format!("{}@{}", manifest.package.id, manifest.package.version);
    if metadata.required_platform_policy_contract() != &manifest.required_platform_policy_contract {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::GeneratedPackageMetadata,
            format!(
                "package {coordinate} manifest and generated package contract disagree on the required platform policy contract; regenerate the package evidence"
            ),
        ));
    }
    if !metadata.promotion_eligible() {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::PolicyContractUnsatisfied,
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

    use wamn_catalog::{OperationType, ServingRelation};
    use wamn_test_infrastructure::operations::sealed;

    use super::BTreeMap;

    fn app(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps")
            .join(name)
    }

    /// The route of one package, assembled alone.
    fn package_route(
        root: &Path,
        package_id: &str,
        component: &str,
        operation: &str,
    ) -> Result<wamn_catalog::ServingRoute, super::PublishManifestError> {
        let roots = BTreeMap::from([(package_id.to_owned(), root)]);
        super::package_route(&roots, package_id, component, operation)
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
        let get = package_route(&wms, "wamn_wms", "wms", &sealed("wamn-wms:packaging/get"))
            .expect("the packaging get has a contract");
        assert_eq!(get.type_, OperationType::Get);
        assert_eq!(get.revision.as_deref(), Some("row_version"));
        assert_eq!(get.reads, relations(&[("wms", "packaging")]));

        let query = package_route(&wms, "wamn_wms", "wms", &sealed("wamn-wms:packaging/query"))
            .expect("the packaging query has a contract");
        assert_eq!((query.type_, query.revision), (OperationType::Query, None));
        assert_eq!(query.reads, relations(&[("wms", "packaging")]));

        let aggregate = package_route(
            &wms,
            "wamn_wms",
            "wms",
            &sealed("wamn-wms:inventory/aggregate"),
        )
        .expect("the authored projection has a contract");
        assert_eq!(
            aggregate.reads,
            relations(&[("wms", "packaging"), ("wms", "packaging_quantity")])
        );

        let history = package_route(
            &app("wamn_receiving"),
            "wamn_receiving",
            "receiving",
            &sealed("wamn-receiving:receiving/load-purchase-order-history"),
        )
        .expect("the history projection has a contract");
        assert_eq!(
            history.reads,
            relations(&[("receiving", "purchase_order")]),
            "a history table stands for its model relation"
        );

        let write = package_route(&wms, "wamn_wms", "wms", &sealed("wamn-wms:inventory/move"))
            .expect("the move command has a contract");
        assert_eq!((write.reads, write.revision), (BTreeSet::new(), None));
    }

    /// A write route names the input field of its idempotency key, from the
    /// generated input contract. A read and a keyless write name none.
    #[test]
    fn a_write_route_names_its_idempotency_field() {
        let wms = app("wamn_wms");
        let route = |operation: &str| {
            package_route(&wms, "wamn_wms", "wms", operation)
                .expect("the operation has a contract")
                .idempotency
        };
        assert_eq!(
            route(&sealed("wamn-wms:inventory/move")).as_deref(),
            Some("value.idempotency_key")
        );
        assert_eq!(
            route(&sealed("wamn-wms:packaging/create")).as_deref(),
            Some("idempotency_key")
        );
        assert_eq!(route(&sealed("wamn-wms:packaging/get")), None);
        let archive = package_route(
            &app("platform_fixture"),
            "platform_fixture",
            "widget",
            "platform-fixture:widget/archive@2.1.0",
        );
        assert_eq!(
            archive.expect("the archive has a contract").idempotency,
            None
        );
    }

    /// An inherited claim is the base's: the overlay route claims under the
    /// base operation, with the base key field and the base canonical form.
    #[test]
    fn an_inherited_claim_carries_the_base_claim() {
        let receiving = app("wamn_receiving");
        let acme = app("client_acme_receiving");
        let roots = BTreeMap::from([
            ("wamn_receiving".to_owned(), receiving.as_path()),
            ("client_acme_receiving".to_owned(), acme.as_path()),
        ]);
        let base = super::package_route(
            &roots,
            "wamn_receiving",
            "receiving",
            "wamn-receiving:receiving/record-receipt@2.1.0",
        )
        .expect("the base command has a contract");
        let overlay = super::package_route(
            &roots,
            "client_acme_receiving",
            "client_acme_receiving",
            "client-acme-receiving:receiving/record-receipt@4.1.0",
        )
        .expect("the overlay command has a contract");
        assert_eq!(base.claim_operation, None);
        assert_eq!(
            overlay.claim_operation.as_deref(),
            Some(base.operation.as_str())
        );
        assert_eq!(overlay.idempotency, base.idempotency);
        assert!(base.canonicalization.is_some());
        assert_eq!(overlay.canonicalization, base.canonicalization);
    }
}

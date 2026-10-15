//! Package manifest bytes and generated operation contracts.

use std::path::Path;

use wamn_record_history::{HISTORY_TABLE_SUFFIX, is_history_table_name};

use super::{
    BTreeMap, BTreeSet, OperationType, PathBuf, PublishManifestError, PublishManifestErrorType,
    RouteCanonicalization, RouteContract, RouteContracts, ServingRelation, ServingRoute,
};

/// Every package's parsed manifest and the contract kind of every operation
/// the packages generate.
pub(super) type PackageManifestSources = (
    BTreeMap<String, wamn_schema_generator::PackageManifest>,
    RouteContracts,
);

pub(super) fn read_package_manifests(
    paths: &[PathBuf],
) -> Result<PackageManifestSources, PublishManifestError> {
    let mut manifests = BTreeMap::new();
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
                PublishManifestErrorType::PackageManifest,
                format!(
                    "package manifest {} has no package directory; pass package-owned wamn.json",
                    path.display()
                ),
            )
        })?;
        let package_id = manifest.package.id.clone();
        contracts.insert(package_id.clone(), read_operation_contracts(root)?);
        if manifests.insert(package_id.clone(), manifest).is_some() {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::PackageManifest,
                format!("more than one package manifest names {package_id:?}"),
            ));
        }
    }
    let mut kinds = RouteContracts::new();
    for (package_id, package) in contracts {
        for contract in package {
            kinds.insert((package_id.clone(), contract.operation), contract.route);
        }
    }
    Ok((manifests, kinds))
}

/// The manifest route of one route attachment of the package at `root`, as
/// publish writes it from the generated contract of its operation.
///
/// A local application assembly calls it, so a test release carries the same
/// route facts as a published one. `roots` holds the directory of every
/// assembled package.
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
                PublishManifestErrorType::PackageManifest,
                format!(
                    "no generated contract of package {package_id:?} declares operation {operation:?}; regenerate the package evidence"
                ),
            )
        })?;
    let contract = contract.route;
    Ok(ServingRoute {
        package_id: package_id.to_owned(),
        component: component.to_owned(),
        operation: operation.to_owned(),
        type_: contract.kind,
        reads: contract.reads,
        revision: contract.revision,
        idempotency: contract.idempotency,
        canonicalization: contract.canonicalization,
    })
}

/// One generated contract of a package.
struct PackageContract {
    operation: String,
    route: RouteContract,
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
            PublishManifestErrorType::PackageManifest,
            format!("read generated contracts {}", path.display()),
            error,
        )
    };
    let contracts = wamn_schema_generator::output_root(root).join("contracts");
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
                    PublishManifestErrorType::PackageManifest,
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
            kinds.push(PackageContract {
                operation: contract.operation,
                route: RouteContract {
                    kind: contract.type_,
                    reads,
                    revision,
                    idempotency,
                    canonicalization,
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
                PublishManifestErrorType::PackageManifest,
                format!("read generated contract {}", input_path.display()),
                error,
            ));
        }
    };
    let contract = serde_json::from_slice(&bytes).map_err(|error| {
        PublishManifestError::with_source(
            PublishManifestErrorType::PackageManifest,
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
}

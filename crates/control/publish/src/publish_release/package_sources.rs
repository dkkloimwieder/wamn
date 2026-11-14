//! Package manifest bytes and generated operation contracts.

pub(super) mod plan;

use std::path::Path;

use crate::package_artifact::{OpenedPackage, PackageRegistry, open_package};
use anyhow::{Context as _, ensure};
use wamn_catalog::{ApplicationDefinition, PackageCoordinate};

use wamn_record_history::{HISTORY_TABLE_SUFFIX, is_history_table_name};

use super::{
    BTreeMap, BTreeSet, OperationType, PathBuf, PublishManifestError, PublishManifestErrorType,
    RouteContract, RouteContracts, ServingRelation, ServingRoute,
};

/// Every package's parsed manifest and the contract type of every operation
/// the packages generate.
pub(super) type PackageManifestSources = (
    BTreeMap<String, wamn_compiler::manifest::PackageManifest>,
    RouteContracts,
);

/// Open only the selected application's exact package dependency closure.
pub(super) async fn open_application(
    registry: &PackageRegistry,
    application: &ApplicationDefinition,
    available: &[PackageCoordinate],
) -> anyhow::Result<(Vec<PackageCoordinate>, Vec<OpenedPackage>)> {
    let mut pending = Vec::new();
    for id in &application.packages {
        let mut matches = available
            .iter()
            .filter(|package| package.package_id() == id);
        let package = matches
            .next()
            .with_context(|| format!("application package {id:?} needs an exact coordinate"))?;
        ensure!(
            matches.next().is_none(),
            "application package {id:?} has multiple coordinates"
        );
        pending.push(package.clone());
    }
    ensure!(!pending.is_empty(), "application has no packages");
    let mut selected = BTreeMap::new();
    let mut coordinates = Vec::new();
    let mut opened = Vec::new();
    while let Some(package) = pending.pop() {
        if let Some(version) = selected.get(package.package_id()) {
            ensure!(
                version == package.package_version(),
                "application requires conflicting versions of {}",
                package.package_id()
            );
            continue;
        }
        let artifact = open_package(registry, package.package_id(), package.package_version())
            .await
            .with_context(|| {
                format!(
                    "open {}@{}",
                    package.package_id(),
                    package.package_version()
                )
            })?;
        let manifest = wamn_compiler::manifest::PackageManifest::from_slice(&artifact.manifest()?)?;
        ensure!(
            manifest.package.id == package.package_id()
                && manifest.package.version == package.package_version(),
            "package artifact identity differs from its coordinate"
        );
        for dependency in manifest.base_dependencies.values() {
            pending.push(PackageCoordinate::new(
                &dependency.package,
                &dependency.version,
            )?);
        }
        selected.insert(
            package.package_id().to_owned(),
            package.package_version().to_owned(),
        );
        coordinates.push(package);
        opened.push(artifact);
    }
    Ok((coordinates, opened))
}

pub(super) fn read_package_manifests(
    roots: &[PathBuf],
) -> Result<PackageManifestSources, PublishManifestError> {
    let mut manifests = BTreeMap::new();
    let mut contracts = BTreeMap::new();
    for root in roots {
        let path = wamn_compiler::package_manifest_path(root);
        let directory = crate::package_source::read_package_directory(root).map_err(|error| {
            PublishManifestError {
                source: Some(error.into()),
                ..PublishManifestError::new(
                    PublishManifestErrorType::PackageManifest,
                    format!("read package directory {}", root.display()),
                )
            }
        })?;
        let manifest =
            wamn_compiler::manifest::PackageManifest::from_slice(&directory.manifest_bytes)
                .map_err(|error| {
                    PublishManifestError::with_source(
                        PublishManifestErrorType::PackageManifest,
                        format!("parse package manifest {}", path.display()),
                        error,
                    )
                })?;
        wamn_compiler::manifest::validate_operation_vocabulary(&manifest).map_err(|error| {
            PublishManifestError::with_source(
                PublishManifestErrorType::PackageManifest,
                format!("validate package manifest {}", path.display()),
                error,
            )
        })?;
        let package_id = manifest.package.id.clone();
        contracts.insert(package_id.clone(), operation_contracts(&manifest)?);
        if manifests.insert(package_id.clone(), manifest).is_some() {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::PackageManifest,
                format!("more than one package manifest names {package_id:?}"),
            ));
        }
    }
    let mut types = RouteContracts::new();
    for (package_id, package) in contracts {
        for contract in package {
            types.insert((package_id.clone(), contract.operation), contract.route);
        }
    }
    Ok((manifests, types))
}

/// One generated contract of a package.
struct PackageContract {
    operation: String,
    route: RouteContract,
}

/// Borrow the operation descriptions compiled into a package document.
fn operation_contracts(
    manifest: &wamn_compiler::manifest::PackageManifest,
) -> Result<Vec<PackageContract>, PublishManifestError> {
    #[derive(serde::Deserialize)]
    struct Relation<'a> {
        schema: &'a str,
        table: &'a str,
    }
    let read = || -> anyhow::Result<Vec<PackageContract>> {
        manifest
            .compiled_operations
            .values()
            .map(|description| {
                let contract = &description.operation;
                let reads = if contract.type_.is_read() {
                    contract
                        .relations
                        .iter()
                        .map(|relation| {
                            let relation: Relation<'_> = serde::Deserialize::deserialize(relation)?;
                            let table = if is_history_table_name(relation.table) {
                                &relation.table[..relation.table.len() - HISTORY_TABLE_SUFFIX.len()]
                            } else {
                                relation.table
                            };
                            Ok(ServingRelation {
                                schema: relation.schema.to_owned(),
                                relation: table.to_owned(),
                            })
                        })
                        .collect::<anyhow::Result<BTreeSet<_>>>()?
                } else {
                    BTreeSet::new()
                };
                let revision = contract
                    .record
                    .as_ref()
                    .and_then(|record| record["revision_field"].as_str())
                    .filter(|_| contract.type_ == OperationType::Get)
                    .map(str::to_owned);
                let idempotency = (!contract.type_.is_read())
                    .then(|| wamn_compiler::client_plan::idempotency_field(&description.input))
                    .flatten();
                Ok(PackageContract {
                    operation: contract.operation.clone(),
                    route: RouteContract {
                        type_: contract.type_,
                        reads,
                        revision,
                        idempotency,
                    },
                })
            })
            .collect()
    };
    read().map_err(|source| PublishManifestError {
        source: Some(source.into()),
        ..PublishManifestError::new(
            PublishManifestErrorType::PackageManifest,
            format!("read compiled operations of {}", manifest.package.id),
        )
    })
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
    let path = wamn_compiler::package_manifest_path(root);
    let bytes = std::fs::read(&path).map_err(|source| {
        PublishManifestError::with_source(
            PublishManifestErrorType::PackageManifest,
            format!("read {}", path.display()),
            source,
        )
    })?;
    let manifest =
        wamn_compiler::manifest::PackageManifest::from_slice(&bytes).map_err(|source| {
            PublishManifestError::with_source(
                PublishManifestErrorType::PackageManifest,
                format!("parse {}", path.display()),
                source,
            )
        })?;
    let contracts = operation_contracts(&manifest)?;
    plan::package_route(contracts, package_id, component, operation)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use wamn_catalog::{OperationType, ServingRelation};
    use wamn_test_infrastructure::operations::sealed;

    use super::BTreeMap;

    #[test]
    fn publication_reads_built_packages_with_and_without_source_migrations() {
        for with_migrations in [true, false] {
            let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
            let root = scratch.path().join("source_fixture");
            let output = wamn_compiler::output_root(&root);
            std::fs::create_dir_all(&output).unwrap();
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("wamn.k"),
                "package = {id = \"source_fixture\", version = \"1.0.0\"}\n",
            )
            .unwrap();
            let mut manifest: serde_json::Value = serde_json::from_slice(include_bytes!(
                "../../tests/fixtures/component_package/generated/wamn.json"
            ))
            .unwrap();
            if with_migrations {
                std::fs::create_dir(output.join("migrations")).unwrap();
                std::fs::write(
                    output.join("migrations/0001_initial.sql"),
                    "CREATE TABLE inventory.item (id uuid);",
                )
                .unwrap();
            } else {
                manifest["models"]["item"]["owner"] = "base_fixture".into();
                manifest["models"]["item"]
                    .as_object_mut()
                    .unwrap()
                    .remove("audit_log");
                manifest["base_dependencies"] = serde_json::json!({
                    "base": {
                        "package": "base_fixture",
                        "version": "1.0.0",
                        "digest": crate::test_support::DIGEST,
                        "operations": ["item.get"],
                    }
                });
            }
            std::fs::write(
                output.join("wamn.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            wamn_test_infrastructure::fixture_digest::write_build_record(&root);

            super::read_package_manifests(&[root]).unwrap();
        }
    }

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

        assert_eq!(route(&sealed("wamn-wms:packaging/get")), None);
        let archive = package_route(
            &app("platform_fixture"),
            "platform_fixture",
            "widget",
            "platform-fixture:widget/archive@2.1.1",
        );
        assert_eq!(
            archive.expect("the archive has a contract").idempotency,
            None
        );
    }
}

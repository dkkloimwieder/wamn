//! Package manifest bytes and generated operation contracts.

pub(super) mod plan;

use wamn_record_history::{HISTORY_TABLE_SUFFIX, is_history_table_name};

use super::{
    BTreeMap, BTreeSet, OperationType, PublishManifestError, PublishManifestErrorType,
    RouteContract, RouteContracts, ServingRelation, ServingRoute,
};

/// Collect the compiled operation contracts for the packages in one deployment.
pub fn route_contracts(
    manifests: &BTreeMap<wamn_package::PackageName, wamn_component_policy::PackageManifest>,
) -> Result<RouteContracts, PublishManifestError> {
    let mut types = RouteContracts::new();
    for (package_id, manifest) in manifests {
        for contract in operation_contracts(manifest)? {
            types.insert(
                (package_id.clone(), contract.operation.into()),
                contract.route,
            );
        }
    }
    Ok(types)
}

/// One generated contract of a package.
struct PackageContract {
    operation: wamn_package::registered_operation::RegisteredOperation,
    route: RouteContract,
}

/// Borrow the operation descriptions compiled into a package document.
fn operation_contracts(
    manifest: &wamn_component_policy::PackageManifest,
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
                    .then(|| wamn_package::client_plan::idempotency_field(&description.input))
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

/// Project one route from its compiled package manifest.
pub fn package_route(
    manifest: &wamn_component_policy::PackageManifest,
    component: &wamn_package::canonical_name::CanonicalName,
    operation: &wamn_package::registered_operation::RegisteredOperation,
) -> Result<ServingRoute, PublishManifestError> {
    let contracts = operation_contracts(manifest)?;
    plan::package_route(contracts, &manifest.package.id, component, operation)
}

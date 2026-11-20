use super::{PackageContract, PublishManifestError, PublishManifestErrorType, ServingRoute};

pub(super) fn package_route(
    contracts: Vec<PackageContract>,
    package_id: &wamn_package::PackageName,
    component: &wamn_package::canonical_name::CanonicalName,
    operation: &wamn_package::registered_operation::RegisteredOperation,
) -> Result<ServingRoute, PublishManifestError> {
    let contract = contracts
        .into_iter()
        .find(|contract| &contract.operation == operation)
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
        operation: operation.clone().into(),
        type_: contract.type_,
        reads: contract.reads,
        revision: contract.revision,
        idempotency: contract.idempotency,
    })
}

use super::{PackageContract, PublishManifestError, PublishManifestErrorType, ServingRoute};

pub(super) fn package_route(
    contracts: Vec<PackageContract>,
    package_id: &str,
    component: &str,
    operation: &str,
) -> Result<ServingRoute, PublishManifestError> {
    let contract = contracts
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

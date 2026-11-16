//! Operations exported by the components of one release.

use crate::canonical_name::CanonicalName;

use crate::ServingComponent;
use serde::Serialize;

/// The operations of a release's components.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseProfile {
    pub operations: Vec<OperationProfile>,
}

/// One export of one release component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperationProfile {
    pub package: crate::PackageName,
    pub component: CanonicalName,
    pub operation: CanonicalName,
}

/// List the exported operations of the released components.
pub fn release_profile<'a>(
    components: impl IntoIterator<Item = &'a ServingComponent>,
) -> ReleaseProfile {
    let operations = components
        .into_iter()
        .flat_map(|component| {
            component
                .descriptor
                .operations
                .keys()
                .map(|operation| OperationProfile {
                    package: component.package_id.clone(),
                    component: component.descriptor.component.clone(),
                    operation: operation.clone(),
                })
        })
        .collect();
    ReleaseProfile { operations }
}

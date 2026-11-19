//! Platform palette descriptors shared by publication and development.
use crate::ComponentPackageCoordinate;

/// The platform owns these components independently of application packages.
pub const PALETTE_PACKAGE_ID: &str = "wamn_palette";
pub const PALETTE_PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PALETTE_FILE: &str = "palette.json";
pub const PALETTE_DESCRIPTOR_ANNOTATION: &str = "wamn.palette.descriptor";

/// Supply only the deployment coordinate when reading a sealed descriptor.
pub fn palette_target(
    project_env_key: &wamn_registry::ProjectEnvKey,
) -> ComponentPackageCoordinate {
    ComponentPackageCoordinate {
        project_env_key: project_env_key.to_owned(),
        package_id: PALETTE_PACKAGE_ID
            .parse()
            .expect("platform package name is valid"),
        package_version: PALETTE_PACKAGE_VERSION
            .parse()
            .expect("fixed palette package version"),
    }
}

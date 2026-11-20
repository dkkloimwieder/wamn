//! Platform palette descriptors shared by publication and development.
use crate::PackageCoordinate;

/// The platform owns these components independently of application packages.
pub const PALETTE_PACKAGE_ID: &str = "wamn_palette";
pub const PALETTE_PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PALETTE_FILE: &str = "palette.json";
pub const PALETTE_DESCRIPTOR_ANNOTATION: &str = "wamn.palette.descriptor";

/// The package identity used to admit platform palette components.
pub fn palette_package() -> PackageCoordinate {
    PackageCoordinate::new(
        PALETTE_PACKAGE_ID
            .parse()
            .expect("valid palette package name"),
        PALETTE_PACKAGE_VERSION
            .parse()
            .expect("valid palette package version"),
    )
    .expect("valid palette package coordinate")
}

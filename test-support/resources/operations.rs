//! The sealed operation ids and package versions of the repository packages.
//!
//! A test names an operation by its reference,
//! `<package>:<interface>/<operation>`, as the authored publication files do.
//! The version is authored once, in the package `wamn.json`, and these
//! functions read it there.

use std::path::PathBuf;

use wamn_catalog::manifest::{OperationOwners, resolve_operation_reference};
use wamn_codegen::package_manifest_path;

/// The manifest of one repository package, by its package id.
fn owners(package_id: &wamn_catalog::PackageName) -> OperationOwners {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps")
        .join(package_id.as_str());
    let path = package_manifest_path(&root);
    let bytes =
        std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    OperationOwners::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// The sealed id of one operation reference of a repository package.
///
/// # Panics
///
/// When the reference names no repository package, or does not resolve.
pub fn sealed(reference: &str) -> wamn_catalog::registered_operation::RegisteredOperation {
    let (prefix, _) = reference
        .split_once(':')
        .unwrap_or_else(|| panic!("{reference:?} is not an operation reference"));
    resolve_operation_reference(
        &owners(
            &prefix
                .replace('-', "_")
                .parse()
                .expect("valid operation package name"),
        ),
        reference,
    )
    .unwrap_or_else(|error| panic!("resolve {reference:?}: {error}"))
}

/// The version of one repository package, from its `wamn.json`.
///
/// # Panics
///
/// When no repository package has the id.
pub fn package_version(
    package_id: &wamn_catalog::PackageName,
) -> wamn_catalog::package_version::PackageVersion {
    owners(package_id).package.version
}

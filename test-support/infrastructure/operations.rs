//! The sealed operation ids and package versions of the repository packages.
//!
//! A test names an operation by its reference,
//! `<package>:<interface>/<operation>`, as the authored publication files do.
//! The version is authored once, in the package `wamn.json`, and these
//! functions read it there (docs/plan/operation-ids.md).

use std::path::PathBuf;

use wamn_schema_generator::{OperationOwners, resolve_operation_reference};

/// The `wamn.json` of one repository package, by its package id.
fn owners(package_id: &str) -> OperationOwners {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps")
        .join(package_id)
        .join("wamn.json");
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
pub fn sealed(reference: &str) -> String {
    let (prefix, _) = reference
        .split_once(':')
        .unwrap_or_else(|| panic!("{reference:?} is not an operation reference"));
    resolve_operation_reference(&owners(&prefix.replace('-', "_")), reference)
        .unwrap_or_else(|error| panic!("resolve {reference:?}: {error}"))
}

/// The version of one repository package, from its `wamn.json`.
///
/// # Panics
///
/// When no repository package has the id.
pub fn package_version(package_id: &str) -> String {
    owners(package_id).package.version
}

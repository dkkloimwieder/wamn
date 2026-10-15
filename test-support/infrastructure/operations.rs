//! The sealed operation ids and package versions of the repository packages.
//!
//! A test names an operation by its reference,
//! `<package>:<interface>/<operation>`, as the authored publication files do.
//! The version is authored once, in the package `wamn.json`, and these
//! functions read it there (docs/history/operation-ids.md). A cluster case reads
//! it from the `wamn.json` of the candidate's package artifact instead
//! ([`sealed_in`]): the runner has no checkout.

use std::path::PathBuf;

use wamn_schema_generator::{OperationOwners, package_manifest_path, resolve_operation_reference};

/// The manifest of one repository package, by its package id.
fn owners(package_id: &str) -> OperationOwners {
    let path = package_manifest_path(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps")
            .join(package_id),
    );
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

/// The sealed id of one operation reference of the package whose `wamn.json`
/// is `manifest`, such as layer 0 of a candidate's package artifact.
///
/// # Errors
///
/// When the manifest does not parse, or the reference does not resolve.
pub fn sealed_in(manifest: &[u8], reference: &str) -> anyhow::Result<String> {
    Ok(resolve_operation_reference(
        &OperationOwners::from_slice(manifest)?,
        reference,
    )?)
}

/// The version of one repository package, from its `wamn.json`.
///
/// # Panics
///
/// When no repository package has the id.
pub fn package_version(package_id: &str) -> String {
    owners(package_id).package.version
}

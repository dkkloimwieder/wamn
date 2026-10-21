//! Gate, author, and publish the wirings of a release inside one Job pod.

use std::path::Path;

use anyhow::Context as _;
use wamn_authoring_model::{
    AuthoringCommand, AuthoringDocument, AuthoringRequest, AuthoringRequestEnvelope,
    AuthoringScope, Gate, SCHEMA_VERSION,
};
use wamn_catalog::PackageCoordinate;

/// Build the authoring request for a package's resolved wiring document.
///
/// # Errors
///
/// Returns an error for an unreadable document or a missing wiring identity.
pub fn gate_request(
    package: &PackageCoordinate,
    project: &str,
    environment: &str,
    path: &Path,
) -> anyhow::Result<AuthoringDocument> {
    let document = wamn_schema_generator::operation_reference::read_authored_document(
        path,
        wamn_schema_generator::operation_reference::AuthoredDocument::Wiring,
    )?;
    let wiring_id = document["wiring-id"]
        .as_str()
        .context("the wiring document names no wiring-id")?;
    Ok(AuthoringDocument::Request(Box::new(
        AuthoringRequestEnvelope::Command(AuthoringRequest {
            schema_version: SCHEMA_VERSION.to_owned(),
            command_id: format!(
                "gate-{}-{}-{wiring_id}",
                package.package_id(),
                package.package_version()
            ),
            command: AuthoringCommand::Gate(Gate {
                scope: AuthoringScope {
                    project_id: project.into(),
                    environment: environment.into(),
                },
                package_id: package.package_id().into(),
                package_version: package.package_version().into(),
                document,
            }),
        }),
    )))
}

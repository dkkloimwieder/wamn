//! Validate a storage binding before building a client.

use crate::plugins::wamn_postgres::ConnectionEffectSnapshot;
use wamn_catalog::{BlobstoreDefinition, BlobstoreProvider, ConnectionTypeDescriptor};

/// A storage refusal before a client or container is exposed.
#[derive(Debug)]
pub struct BindingError {
    type_: BindingErrorType,
    detail: &'static str,
    source: Option<wamn_catalog::ConnectionDefinitionError>,
}
#[derive(Debug)]
enum BindingErrorType {
    Authority,
    Definition,
    Credential,
}
impl BindingError {
    pub(super) fn unauthorized() -> Self {
        Self {
            type_: BindingErrorType::Authority,
            detail: "connection is not authorized for this component",
            source: None,
        }
    }
    fn definition(source: wamn_catalog::ConnectionDefinitionError) -> Self {
        Self {
            type_: BindingErrorType::Definition,
            detail: "storage definition is invalid",
            source: Some(source),
        }
    }
    fn refused(type_: BindingErrorType, detail: &'static str) -> Self {
        Self {
            type_,
            detail,
            source: None,
        }
    }
}
impl core::fmt::Display for BindingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let category = match self.type_ {
            BindingErrorType::Authority => "authority",
            BindingErrorType::Definition => "definition",
            BindingErrorType::Credential => "credential",
        };
        write!(f, "storage {category}: {}", self.detail)?;
        if let Some(source) = &self.source {
            write!(f, ": {source}")?;
        }
        Ok(())
    }
}
impl std::error::Error for BindingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

pub fn resolve(
    snapshot: &ConnectionEffectSnapshot,
    provider: BlobstoreProvider,
) -> Result<BlobstoreDefinition, BindingError> {
    crate::plugins::connection_http::authorize_snapshot(
        snapshot,
        &ConnectionTypeDescriptor::blobstore_v1(),
    )
    .map_err(|_| BindingError::unauthorized())?;
    resolve_coordinates(
        snapshot.definition.as_ref().ok_or(BindingError::refused(
            BindingErrorType::Definition,
            "connection generation carries no definition",
        ))?,
        snapshot.credential_handle.as_deref(),
        provider,
    )
}

pub fn resolve_coordinates(
    definition: &serde_json::Value,
    credential_handle: Option<&str>,
    provider: BlobstoreProvider,
) -> Result<BlobstoreDefinition, BindingError> {
    let bound =
        BlobstoreDefinition::parse_for(definition, provider).map_err(BindingError::definition)?;
    match (
        bound.provider,
        credential_handle.filter(|handle| !handle.is_empty()),
    ) {
        (BlobstoreProvider::S3, None) => {
            return Err(BindingError::refused(
                BindingErrorType::Credential,
                "connection names no credential",
            ));
        }
        (BlobstoreProvider::Gcs, Some(_)) => {
            return Err(BindingError::refused(
                BindingErrorType::Credential,
                "gcs uses the pod identity and takes no credential handle",
            ));
        }
        _ => {}
    }
    Ok(bound)
}

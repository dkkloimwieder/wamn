//! Validate a storage binding before building a client.

use wamn_connection_type::{BlobstoreDefinition, BlobstoreProvider};

/// A storage refusal before a client or container is exposed.
#[derive(Debug)]
pub struct BindingError {
    type_: BindingErrorType,
    detail: &'static str,
    source: Option<wamn_connection_type::ConnectionDefinitionError>,
}
#[derive(Debug)]
enum BindingErrorType {
    Definition,
    Credential,
}
impl BindingError {
    pub(crate) fn unauthorized() -> Self {
        Self::refused(BindingErrorType::Credential, "blobstore binding refused")
    }

    fn definition(source: wamn_connection_type::ConnectionDefinitionError) -> Self {
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

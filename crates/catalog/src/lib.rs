//! Portable connection definitions and component artifact references.

mod component_artifact_reference;
mod connection;

pub use component_artifact_reference::{
    COMPOSED_ENTRY_REPOSITORY, ComponentArtifactBase, ComponentArtifactReference,
    ComponentArtifactReferenceError, ComponentArtifactReferenceErrorType,
    component_artifact_reference, composed_entry_start_reference, composed_entry_starts_as,
    parse_component_artifact_base, validate_component_start_reference,
    validate_composed_entry_path, validate_serving_entry_reference,
};
pub use connection::{
    BlobstoreDefinition, BlobstoreProvider, CONNECTION_DESCRIPTOR_VERSION,
    ComponentConnectionRequirement, ConnectionAuthorityModel, ConnectionDefinitionError,
    ConnectionField, ConnectionFieldOwner, ConnectionFieldOwnership, ConnectionTypeDescriptor,
    CredentialInjection, HttpConnectionDefinition, HttpTlsVerification, RequirementType,
    RuntimeType, org_credential_handle, parse_org_credential_handle,
};

//! Compiled release facts, immutable catalog identities, and definition hashing.

pub mod client_package_name;
mod component_artifact_reference;
mod component_facts;
mod connection;
pub mod edge_bundle;
mod environment_application;
pub mod feed;
mod host_route;
mod http_route_data;
pub mod http_routes;
pub mod local_operation;
pub mod manifest;
mod package;
pub mod package_version;
pub mod profile;
pub mod read_query;
pub mod registered_operation;
mod route_identity;
pub mod schema;
mod serving_manifest;
mod signed_file_read;
mod wiring;
mod wiring_compatibility;
mod wiring_jsonata;

mod workflow_condition;

/// Test databases with the project catalog minimum.
#[cfg(feature = "test-util")]
pub mod test_database;

pub use component_artifact_reference::{
    COMPOSED_ENTRY_REPOSITORY, ComponentArtifactBase, ComponentArtifactReference,
    ComponentArtifactReferenceError, ComponentArtifactReferenceErrorType,
    component_artifact_reference, composed_entry_start_reference, composed_entry_starts_as,
    parse_component_artifact_base, validate_component_start_reference,
    validate_composed_entry_path,
};
pub use component_facts::{
    AdmittedComponent, AdmittedComponentEffect, AdmittedComponentFacts, AdmittedComponentOperation,
    AdmittedComponentParameter, AdmittedComponentPort, ComponentConnection,
    ComponentConnectionAlias, ComponentConnectionType, ComponentDeclaration, ComponentDescriptor,
    ComponentEffectProvenance, ComponentFactError, ComponentFactErrorType,
    ComponentOperationDeclaration, ComponentPackageCoordinate, ComponentParameterDeclaration,
    ComponentPortDeclaration, ComponentSchema, ConnectionMode, normalize_component_fact,
    verify_stored_effect_projection,
};
pub use connection::{
    BlobstoreDefinition, BlobstoreProvider, CONNECTION_DESCRIPTOR_VERSION,
    ComponentConnectionRequirement, ConnectionAuthorityModel, ConnectionDefinitionError,
    ConnectionField, ConnectionFieldOwner, ConnectionFieldOwnership, ConnectionTypeDescriptor,
    CredentialInjection, HttpConnectionDefinition, HttpTlsVerification, RequirementType,
    RuntimeType, org_credential_handle, parse_org_credential_handle,
};
pub use environment_application::{
    ApplicationDefinition, ApplicationName, EnvironmentApplications, ParseApplicationNameError,
    application_for_package,
};
pub use host_route::{
    HostAttachment, HostHandler, HostRoute, HostRouteAuthority, HostRouteContractError,
    HostRouteSet, host_route_package, host_route_path_prefix, host_route_version,
    is_granted_host_route_operation, is_host_route_operation,
};
pub use package::{PackageCoordinate, PackageName, ParsePackageNameError};
pub use route_identity::{
    OperationContract, OperationDescription, StatementDescription, StatementValue,
    operation_contract, operation_token, route_attachment_id, route_path,
    sealed_operation_reference, split_sealed_operation,
};
pub use serving_manifest::{
    AttachmentAuthPolicy, AttachmentRef, AttachmentTarget, INVALID_ATTACHMENT_AUTH_POLICY_REFUSAL,
    MAX_SERVING_MANIFEST_BYTES, NO_AUTHENTICATION_MODE, OperationType, PAT_AUTHENTICATION_MODE,
    RELEASE_MANIFEST_CONFIGMAP_PREFIX, RELEASE_MANIFEST_FILE_NAME, RELEASE_MANIFEST_MOUNT_PATH,
    RouteAttachment, SERVING_MANIFEST_FORMAT_VERSION, SESSION_AUTHENTICATION_MODE,
    ServedAttachment, ServingAttachment, ServingComponent, ServingComposedEntry,
    ServingEntryMember, ServingManifest, ServingRelation, ServingRelease, ServingRoute,
    UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL, parse_attachment_auth_policy,
    parse_workflow_schedule, release_manifest_configmap_name,
};
pub use signed_file_read::{
    MAX_SIGNED_FILE_SECONDS, SIGNED_FILE_READS, SignedFileField, SignedFileRead,
    SignedFileReadError, SignedFileReads, SignedFileResultClass, signed_file_reads,
};
#[cfg(any(test, feature = "test-util"))]
pub use wiring::fixture_graph;
pub use wiring::{
    WIRING_DOCUMENT_FORMAT_VERSION, WiringDocument, WiringEdge, WiringEventOperation, WiringNode,
    WiringOperationDependency, WiringResponse, WiringTerminal, partial_response_schema,
};
pub use wiring_compatibility::{
    WiringCompatibilityError, WiringCompatibilityErrorType, resolve_input_port,
    validate_resolved_wiring_compatibility, validate_wiring_compatibility,
};
pub use wiring_jsonata::specialize_wiring_operation;
pub use workflow_condition::{
    WorkflowConditionError, condition_workflow_document, resolve_workflow_components,
};

use std::fmt;

use serde::{Deserialize, Serialize};

/// Complete catalog bootstrap for a fresh project database, including its transaction.
pub const CATALOG_SCHEMA_SQL: &str = concat!(
    include_str!("../../../deploy/sql/catalog-schema-prefix.sql"),
    include_str!("../../../deploy/sql/reject-immutable-row-change.sql"),
    include_str!("../../../deploy/sql/record-history.sql"),
    include_str!("../../../deploy/sql/record-history-app-grants.sql"),
    include_str!("../../../deploy/sql/catalog-schema.sql"),
);

/// A catalog identity construction error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogIdentityError {
    DuplicateMember {
        field: &'static str,
        id: String,
    },
    EmptyIdentity {
        field: &'static str,
    },
    NonCanonicalIdentity {
        field: &'static str,
    },
    ZeroVersion {
        field: &'static str,
    },
    InvalidDefinition {
        message: String,
    },
    NonCanonicalJson,
    UnsupportedServingManifestVersion {
        requested: String,
    },
    InvalidAttachmentAuthPolicy {
        attachment_id: String,
    },
    UnauthenticatedRegisteredOperation {
        attachment_id: String,
    },
    UnresolvableManifestWiring {
        package_id: crate::PackageName,
        wiring_id: String,
        wiring_version: u32,
    },
    UnresolvableManifestRoute {
        package_id: crate::PackageName,
        component: String,
        operation: String,
    },
    ManifestTooLarge {
        bytes: usize,
        limit: usize,
    },
    UnresolvedWiringNode {
        node_id: String,
    },
    UnresolvedWiringEntry {
        node_id: String,
    },
}

impl fmt::Display for CatalogIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateMember { field, id } => {
                write!(formatter, "{field} member {id:?} is duplicated")
            }
            Self::EmptyIdentity { field } => write!(formatter, "{field} is empty"),
            Self::NonCanonicalIdentity { field } => {
                write!(formatter, "{field} is not in canonical form")
            }
            Self::ZeroVersion { field } => write!(formatter, "{field} must be greater than zero"),
            Self::InvalidDefinition { message } => write!(formatter, "{message}"),
            Self::NonCanonicalJson => write!(formatter, "JSON input is not RFC 8785 canonical"),
            Self::UnsupportedServingManifestVersion { requested } => {
                write!(
                    formatter,
                    "{UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL}: requested {requested}; \
                     supported version is {SERVING_MANIFEST_FORMAT_VERSION}"
                )
            }
            Self::InvalidAttachmentAuthPolicy { attachment_id } => {
                write!(
                    formatter,
                    "{INVALID_ATTACHMENT_AUTH_POLICY_REFUSAL}: attachment {attachment_id:?} must declare one supported canonical modes list"
                )
            }
            Self::UnauthenticatedRegisteredOperation { attachment_id } => {
                write!(
                    formatter,
                    "attachment {attachment_id:?} cannot combine auth-policy.modes=[\"none\"] with registered-operation; set auth-policy.modes=[\"pat\"]"
                )
            }
            Self::UnresolvableManifestWiring {
                package_id,
                wiring_id,
                wiring_version,
            } => {
                write!(
                    formatter,
                    "wiring {package_id:?}/{wiring_id:?} version {wiring_version} is absent from the serving manifest"
                )
            }
            Self::UnresolvableManifestRoute {
                package_id,
                component,
                operation,
            } => {
                write!(
                    formatter,
                    "route {package_id:?}/{component:?} operation {operation:?} is absent from the serving manifest"
                )
            }
            Self::ManifestTooLarge { bytes, limit } => {
                write!(
                    formatter,
                    "serving manifest is {bytes} bytes, over the {limit}-byte delivery limit"
                )
            }
            Self::UnresolvedWiringNode { node_id } => {
                write!(
                    formatter,
                    "wiring edge names node {node_id:?}, which the document does not declare"
                )
            }
            Self::UnresolvedWiringEntry { node_id } => {
                write!(
                    formatter,
                    "wiring entry names node {node_id:?}, which the document does not declare"
                )
            }
        }
    }
}

impl std::error::Error for CatalogIdentityError {}

fn validate_text(value: &str, field: &'static str) -> Result<(), CatalogIdentityError> {
    if value.is_empty() {
        return Err(CatalogIdentityError::EmptyIdentity { field });
    }
    if value.trim() != value || value.as_bytes().contains(&0) {
        return Err(CatalogIdentityError::NonCanonicalIdentity { field });
    }
    Ok(())
}

/// An attachment exposure type.
///
/// `Deserialize` is derived so the frozen [`ServingAttachment`] wire shape can
/// name this type instead of re-declaring one; the enum is fieldless, so the
/// derive adds no unvalidated construction path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttachmentType {
    Http,
    Internal,
    Studio,
}

/// Default node output port.
pub const MAIN_PORT: &str = "main";

/// Node output port for failures.
pub const ERROR_PORT: &str = "error";

pub mod exposure;
pub mod package_schema;

mod palette;
pub use palette::{
    PALETTE_DESCRIPTOR_ANNOTATION, PALETTE_FILE, PALETTE_PACKAGE_ID, PALETTE_PACKAGE_VERSION,
    Palette, PalettePin, palette_target, read_palette,
};

/// Resource names derived from catalog identities.
pub mod resource_names;

/// Applied environment configuration.
pub mod configuration;
/// Environment and application rollout locks.
pub mod lifecycle_lock;

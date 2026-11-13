//! Compiled release facts, immutable catalog identities, and definition hashing.

mod component_artifact_reference;
mod component_facts;
mod connection;
pub mod edge_bundle;
mod environment_application;
pub mod feed;
mod host_route;
pub mod manifest;
mod package;
pub mod profile;
mod route_identity;
pub mod schema;
mod serving_manifest;
mod signed_file_read;
#[doc(hidden)]
pub mod sql_lex;
mod wiring;
mod wiring_compatibility;
mod workflow_condition;

/// Test databases with the project catalog minimum.
#[cfg(feature = "test-util")]
pub mod test_database;

pub use component_artifact_reference::{
    COMPOSED_ENTRY_REPOSITORY, ComponentArtifactBase, ComponentArtifactReference,
    ComponentArtifactReferenceError, ComponentArtifactReferenceErrorType,
    component_artifact_reference, component_digest_tag, composed_entry_start_reference,
    composed_entry_starts_as, parse_component_artifact_base, validate_component_start_reference,
    validate_composed_entry_path,
};
pub use component_facts::{
    AdmittedComponent, AdmittedComponentEffect, AdmittedComponentFacts, AdmittedComponentOperation,
    AdmittedComponentParameter, AdmittedComponentPort, ComponentConnection,
    ComponentConnectionType, ComponentDeclaration, ComponentDescriptor, ComponentEffectProvenance,
    ComponentFactError, ComponentFactErrorType, ComponentOperationDeclaration,
    ComponentPackageCoordinate, ComponentParameterDeclaration, ComponentPortDeclaration,
    ComponentSchema, ComponentSqlField, ComponentSqlStatement, bind_component_statement_facts,
    normalize_component_fact, verify_stored_effect_projection,
};
pub use connection::{
    CONNECTION_DESCRIPTOR_VERSION, ComponentConnectionRequirement, ConnectionAuthorityModel,
    ConnectionField, ConnectionFieldOwner, ConnectionFieldOwnership, ConnectionTypeDescriptor,
    CredentialInjection, RequirementType, RuntimeType, org_credential_handle,
    parse_org_credential_handle,
};
pub use environment_application::{
    ApplicationDefinition, EnvironmentApplications, application_for_package,
};
pub use host_route::{
    HostAttachment, HostHandler, HostRoute, HostRouteAuthority, HostRouteContractError,
    HostRouteSet, host_route_package, host_route_path_prefix, host_route_version,
    is_granted_host_route_operation, is_host_route_operation,
};
pub use package::PackageCoordinate;
pub use route_identity::{
    operation_token, route_attachment_id, route_path, sealed_operation_reference,
    split_sealed_operation,
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
    WiringCompatibilityError, WiringCompatibilityErrorType, validate_resolved_wiring_compatibility,
    validate_wiring_compatibility,
};
pub use workflow_condition::{
    WorkflowConditionError, condition_workflow_document, resolve_workflow_components,
};

use std::fmt;

use serde::{Deserialize, Serialize};

/// Complete catalog bootstrap for a fresh project database, including its transaction.
pub const CATALOG_SCHEMA_SQL: &str = concat!(
    include_str!("../../../../deploy/sql/catalog-schema-prefix.sql"),
    include_str!("../../../../deploy/sql/reject-immutable-row-change.sql"),
    include_str!("../../../../deploy/sql/record-history.sql"),
    include_str!("../../../../deploy/sql/record-history-app-grants.sql"),
    include_str!("../../../../deploy/sql/catalog-schema.sql"),
);

const HASH_PREFIX: &str = "sha256:";
const HASH_HEX_LEN: usize = 64;

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
    InvalidDigest {
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
        package_id: String,
        wiring_id: String,
        wiring_version: u32,
    },
    UnresolvableManifestRoute {
        package_id: String,
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
            Self::InvalidDigest { field } => {
                write!(
                    formatter,
                    "{field} must be sha256:<64 lowercase hex digits>"
                )
            }
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

/// A canonical SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct DefinitionHash(String);

impl DefinitionHash {
    /// Parse a canonical SHA-256 definition hash.
    pub fn parse(value: impl Into<String>) -> Result<Self, CatalogIdentityError> {
        let value = value.into();
        validate_digest(&value, "definition-hash")?;
        Ok(Self(value))
    }

    /// The `sha256:<hex>` representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for DefinitionHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for DefinitionHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A canonical artifact-content digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ArtifactHash(String);

impl ArtifactHash {
    /// Parse a canonical SHA-256 artifact-content digest.
    pub fn parse(value: impl Into<String>) -> Result<Self, CatalogIdentityError> {
        let value = value.into();
        validate_digest(&value, "artifact-hash")?;
        Ok(Self(value))
    }

    /// The `sha256:<hex>` representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ArtifactHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for ArtifactHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The digest of a serving manifest's RFC 8785 canonical bytes.
///
/// This is the one *derived* identity in the family: it is never asserted by a
/// carrier and never read from an object name, only computed from verified
/// content by [`ServingManifest::from_canonical_bytes`]. From there it travels
/// further than any other hash in the system — through the lease-time run
/// recording, effect-authority equality, and the deployment attestation, across
/// two host processes and four manifest readers. Its distinct type keeps it
/// apart from the manifest's [`ArtifactHash`] and [`DefinitionHash`] members.
///
/// Unlike the manifest-carried [`DefinitionHash`] and [`ArtifactHash`], it has
/// no `Deserialize`: a carrier must not assert this derived identity. The only
/// admitting boundary computes it from verified canonical bytes through
/// [`ServingManifest::from_canonical_bytes`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ManifestDigest(String);

impl ManifestDigest {
    /// Parse a canonical SHA-256 serving-manifest digest.
    pub fn parse(value: impl Into<String>) -> Result<Self, CatalogIdentityError> {
        let value = value.into();
        validate_digest(&value, "manifest-digest")?;
        Ok(Self(value))
    }

    /// The `sha256:<hex>` representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ManifestDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn validate_digest(value: &str, field: &'static str) -> Result<(), CatalogIdentityError> {
    let valid = value.strip_prefix(HASH_PREFIX).is_some_and(|hex| {
        hex.len() == HASH_HEX_LEN
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if valid {
        Ok(())
    } else {
        Err(CatalogIdentityError::InvalidDigest { field })
    }
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

#[cfg(test)]
mod tests {
    use super::ManifestDigest;

    /// The invariant that retired `valid_manifest_digest` in the lease path: the
    /// newtype admits exactly the `sha256:<64 lowercase hex>` shape the run
    /// plane's `runs_release_record_check` admits, so a parsed value can never
    /// die on that CHECK inside a lease grant.
    #[test]
    fn a_manifest_digest_admits_exactly_the_run_plane_shape() {
        assert!(ManifestDigest::parse(format!("sha256:{}", "0".repeat(64))).is_ok());
        assert!(ManifestDigest::parse(format!("sha256:{}b", "af9".repeat(21))).is_ok());
        for rejected in [
            String::new(),
            "sha256:".to_string(),
            "deadbeef".to_string(),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(65)),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:{}", "g".repeat(64)),
            format!("SHA256:{}", "a".repeat(64)),
        ] {
            assert!(
                ManifestDigest::parse(rejected.clone()).is_err(),
                "accepted {rejected:?}"
            );
        }
    }
}

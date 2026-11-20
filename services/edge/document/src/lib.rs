//! Edge release documents and their canonical byte representation.

use std::fmt;

pub mod edge_bundle;
pub mod http_routes;
mod serving_manifest;

pub use serving_manifest::{
    AttachmentRef, MAX_SERVING_MANIFEST_BYTES, RELEASE_MANIFEST_CONFIGMAP_PREFIX,
    RELEASE_MANIFEST_FILE_NAME, RouteAttachment, SERVING_MANIFEST_FORMAT_VERSION, ServedAttachment,
    ServingComponent, ServingComposedEntry, ServingEntryMember, ServingManifest, ServingRelation,
    ServingRelease, ServingRoute, UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL,
    release_manifest_configmap_name, route_attachments,
};
use wamn_package::{INVALID_ATTACHMENT_AUTH_POLICY_REFUSAL, PackageIdentityError, canonical_name};

/// A catalog identity construction error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogIdentityError {
    DuplicateMember {
        field: &'static str,
        id: String,
    },
    InvalidDefinition {
        message: String,
    },
    NonCanonicalJson,
    UnsupportedServingManifestVersion {
        requested: String,
    },
    InvalidAttachmentAuthPolicy {
        attachment_id: canonical_name::CanonicalName,
    },
    UnauthenticatedRegisteredOperation {
        attachment_id: canonical_name::CanonicalName,
    },
    UnresolvableManifestRoute {
        package_id: wamn_package::PackageName,
        component: String,
        operation: String,
    },
    ManifestTooLarge {
        bytes: usize,
        limit: usize,
    },
}

impl fmt::Display for CatalogIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateMember { field, id } => {
                write!(formatter, "{field} member {id:?} is duplicated")
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
        }
    }
}

impl std::error::Error for CatalogIdentityError {}

impl From<PackageIdentityError> for CatalogIdentityError {
    fn from(error: PackageIdentityError) -> Self {
        Self::InvalidDefinition {
            message: error.to_string(),
        }
    }
}

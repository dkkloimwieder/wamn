//! Package attachment documents and deployment route preparation.

pub(super) mod plan;
pub use plan::{merge_package_attachment_documents, resolve_route_methods};

use super::{
    AuthoredHttpRoute, BTreeMap, BTreeSet, PathBuf, PublishManifestError, PublishManifestErrorType,
    RouteContracts, ServingAttachment, canonical_http_route_template, normalize_http_route,
};

use wamn_package::AttachmentTarget;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoredRoute {
    path: String,
}

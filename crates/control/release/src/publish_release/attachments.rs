//! Package attachment documents and deployment route preparation.

pub(super) mod plan;
pub(super) use plan::{
    merge_package_attachment_documents, resolve_route_methods,
    validate_attachment_definition_hashes,
};

use super::{
    AuthoredHttpRoute, BTreeMap, BTreeSet, PathBuf, PublishManifestError, PublishManifestErrorType,
    RouteContracts, ServingAttachment, canonical_http_route_template, normalize_http_route,
};

use wamn_catalog::AttachmentTarget;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoredRoute {
    path: String,
}

/// Read final compiler output. Publication already carries inline schemas and hashes.
pub(super) fn read_package_attachments(
    paths: &[PathBuf],
) -> Result<BTreeMap<String, ServingAttachment>, PublishManifestError> {
    let mut documents = Vec::new();
    for path in paths {
        let root = path
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap_or_else(|| std::path::Path::new(""));
        let attachments =
            wamn_compiler::publication::read_package_attachments(root).map_err(|error| {
                PublishManifestError {
                    source: Some(error.into()),
                    ..PublishManifestError::new(
                        PublishManifestErrorType::Document,
                        format!("read final attachments for {}", path.display()),
                    )
                }
            })?;
        documents.push((
            wamn_compiler::output_root(root).join("publication/attachments.json"),
            attachments,
        ));
    }
    merge_package_attachment_documents(documents)
}

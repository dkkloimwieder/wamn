//! Package attachment documents and deployment route preparation.

pub(super) mod plan;
pub(super) use plan::{merge_package_attachment_documents, resolve_route_methods};

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

/// Read the build's compiled routes for local edge assembly.
pub(super) fn read_package_attachments(
    paths: &[PathBuf],
) -> Result<
    BTreeMap<wamn_catalog::canonical_name::CanonicalName, ServingAttachment>,
    PublishManifestError,
> {
    let documents = paths
        .iter()
        .map(|root| {
            let path = wamn_codegen::package_manifest_path(root);
            let read = || -> anyhow::Result<_> {
                let manifest =
                    wamn_catalog::manifest::PackageManifest::from_slice(&std::fs::read(&path)?)?;
                Ok(manifest
                    .deploy
                    .ok_or_else(|| {
                        anyhow::anyhow!("build the package to compile its deploy section")
                    })?
                    .routes)
            };
            let routes = read().map_err(|error| PublishManifestError {
                source: Some(error.into()),
                ..PublishManifestError::new(
                    PublishManifestErrorType::Document,
                    format!("read routes from {}", path.display()),
                )
            })?;
            Ok((path, routes))
        })
        .collect::<Result<Vec<_>, PublishManifestError>>()?;
    merge_package_attachment_documents(documents)
}

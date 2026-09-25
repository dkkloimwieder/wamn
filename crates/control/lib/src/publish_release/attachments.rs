//! Package attachment documents and deployment route preparation.

use super::{
    AuthoredHttpRoute, BTreeMap, BTreeSet, MintManifestError, MintManifestErrorKind, PathBuf,
    RouteKinds, ServingAttachment, canonical_http_route_template, normalize_http_route,
};
use wamn_catalog::AttachmentTarget;

/// The route an author writes: the path alone. The deployment adds the host,
/// and publish adds the method from the operation kind.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoredRoute {
    path: String,
}

/// Read the package attachment documents, with every generated input schema
/// they name resolved against the package that owns the attachment.
///
/// A document can be a copy outside its package, so a reference resolves
/// against the root of the `wamn.json` the release presents for the
/// attachment's package, never against the document's own directory.
pub(super) fn read_package_attachments(
    paths: &[PathBuf],
    package_manifests: &[PathBuf],
) -> Result<BTreeMap<String, ServingAttachment>, MintManifestError> {
    let mut attachments = read_authored_attachments(paths)?;
    resolve_generated_input_schemas(&mut attachments, package_manifests)?;
    Ok(attachments)
}

fn resolve_generated_input_schemas(
    attachments: &mut BTreeMap<String, ServingAttachment>,
    package_manifests: &[PathBuf],
) -> Result<(), MintManifestError> {
    let named = |attachment: &ServingAttachment| {
        wamn_schema_generator::route_schema::names_generated_schema(attachment)
    };
    if !attachments.values().any(named) {
        return Ok(());
    }
    let mut roots = BTreeMap::new();
    for path in package_manifests {
        let bytes = std::fs::read(path).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::PackageManifest,
                format!("read package manifest {}", path.display()),
                error,
            )
        })?;
        let manifest =
            wamn_schema_generator::PackageManifest::from_slice(&bytes).map_err(|error| {
                MintManifestError::with_source(
                    MintManifestErrorKind::PackageManifest,
                    format!("parse package manifest {}", path.display()),
                    error,
                )
            })?;
        let root = path.parent().unwrap_or_else(|| std::path::Path::new(""));
        roots.insert(manifest.package.id, root.to_owned());
    }
    for (attachment_id, attachment) in attachments {
        if !named(attachment) {
            continue;
        }
        let root = roots.get(&attachment.package_id).ok_or_else(|| {
            MintManifestError::new(
                MintManifestErrorKind::Document,
                format!(
                    "attachment {attachment_id:?} names a generated input schema of package {:?}, and the release presents no wamn.json for it",
                    attachment.package_id
                ),
            )
        })?;
        wamn_schema_generator::route_schema::resolve_attachment(
            attachment_id,
            attachment,
            &mut |reference| {
                wamn_schema_generator::route_schema::read_from_package(root, reference)
            },
        )
        .map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Document,
                format!("attachment {attachment_id:?} input schema"),
                error,
            )
        })?;
    }
    Ok(())
}

fn read_authored_attachments(
    paths: &[PathBuf],
) -> Result<BTreeMap<String, ServingAttachment>, MintManifestError> {
    if paths.is_empty() {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Document,
            "publish-release requires at least one package-owned --attachments document",
        ));
    }
    let documents = paths
        .iter()
        .map(|path| {
            let bytes = std::fs::read(path).map_err(|error| {
                MintManifestError::with_source(
                    MintManifestErrorKind::Document,
                    format!("read package attachments {}", path.display()),
                    error,
                )
            })?;
            let attachments = serde_json::from_slice(&bytes).map_err(|error| {
                MintManifestError::with_source(
                    MintManifestErrorKind::Document,
                    format!("parse package attachments {}", path.display()),
                    error,
                )
            })?;
            Ok((path.clone(), attachments))
        })
        .collect::<Result<Vec<_>, MintManifestError>>()?;
    merge_package_attachment_documents(documents)
}

pub(super) fn merge_package_attachment_documents(
    documents: Vec<(PathBuf, BTreeMap<String, ServingAttachment>)>,
) -> Result<BTreeMap<String, ServingAttachment>, MintManifestError> {
    let mut merged = BTreeMap::new();
    let mut sources = BTreeMap::<String, PathBuf>::new();
    for (path, attachments) in documents {
        for (attachment_id, attachment) in attachments {
            if let Some(first_path) = sources.get(&attachment_id) {
                return Err(MintManifestError::new(
                    MintManifestErrorKind::DuplicateAttachmentId,
                    format!(
                        "attachment {attachment_id:?} occurs in package documents {} and {}; keep exactly one package owner",
                        first_path.display(),
                        path.display(),
                    ),
                ));
            }
            sources.insert(attachment_id.clone(), path.clone());
            merged.insert(attachment_id, attachment);
        }
    }
    Ok(merged)
}

/// Require every attachment hash to identify its exact canonical definition.
///
/// The release mint is the production boundary that accepts the authored
/// attachment map. Comparing here prevents a caller from pairing an unchanged
/// identity claim with different route or contract bytes before either can
/// enter the immutable serving snapshot.
pub(super) fn validate_attachment_definition_hashes(
    attachments: &BTreeMap<String, ServingAttachment>,
) -> Result<(), MintManifestError> {
    for (attachment_id, attachment) in attachments {
        let derived = wamn_execution_contract::canonical_json_sha256(&attachment.definition);
        if attachment.definition_hash.as_str() != derived {
            return Err(MintManifestError::new(
                MintManifestErrorKind::Document,
                format!(
                    "attachment {attachment_id:?} definition-hash {} differs from canonical definition hash {derived}",
                    attachment.definition_hash.as_str(),
                ),
            ));
        }
    }
    Ok(())
}

/// Resolve deployment-owned route identity without letting package content
/// become a second hostname emitter, and write each route's method from the
/// kind of the operation it calls.
pub(super) fn resolve_route_host_overlay(
    authored: &BTreeMap<String, ServingAttachment>,
    route_host: Option<&str>,
    route_kinds: &RouteKinds,
) -> Result<BTreeMap<String, ServingAttachment>, MintManifestError> {
    validate_authored_attachment_routes(authored)?;
    validate_attachment_definition_hashes(authored)?;
    let first_routed = authored.iter().find(|(_, attachment)| {
        matches!(
            attachment.kind,
            wamn_catalog::AttachmentKind::Http | wamn_catalog::AttachmentKind::Studio
        )
    });
    let Some((first_attachment_id, _)) = first_routed else {
        return Ok(authored.clone());
    };
    let route_host = route_host.filter(|host| !host.is_empty()).ok_or_else(|| {
        MintManifestError::new(
            MintManifestErrorKind::RouteHostUnbound,
            format!(
                "attachment {first_attachment_id:?} requires deployment route host; pass --route-host"
            ),
        )
    })?;
    if route_host != "*"
        && (route_host.contains('/') || route_host.chars().any(char::is_whitespace))
    {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Document,
            format!("deployment route host {route_host:?} is invalid"),
        ));
    }
    let route_host = route_host.to_ascii_lowercase();
    let mut resolved = authored.clone();
    let mut route_keys = BTreeSet::new();
    for (attachment_id, attachment) in &mut resolved {
        if !matches!(
            attachment.kind,
            wamn_catalog::AttachmentKind::Http | wamn_catalog::AttachmentKind::Studio
        ) {
            continue;
        }
        let method = route_method(attachment_id, attachment, route_kinds)?;
        let route = attachment
            .definition
            .as_object_mut()
            .and_then(|definition| definition.get_mut("route"))
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| {
                MintManifestError::new(
                    MintManifestErrorKind::Document,
                    format!("attachment {attachment_id:?} carries no route object"),
                )
            })?;
        let AuthoredRoute { path } = serde_json::from_value(serde_json::Value::Object(
            route.clone(),
        ))
        .map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Document,
                format!(
                    "attachment {attachment_id:?} route must contain exactly a string path field"
                ),
                error,
            )
        })?;
        let normalized = normalize_http_route(
            &AuthoredHttpRoute {
                path,
                method: method.to_owned(),
            },
            attachment_id,
        )
        .map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Document,
                format!("attachment {attachment_id:?} carries an invalid route"),
                error,
            )
        })?;
        if !route_keys.insert((
            canonical_http_route_template(&normalized.path),
            normalized.method,
        )) {
            return Err(MintManifestError::new(
                MintManifestErrorKind::Document,
                format!(
                    "attachment {attachment_id:?} duplicates another attachment's canonical path and method"
                ),
            ));
        }
        route.insert(
            "method".to_owned(),
            serde_json::Value::String(method.to_owned()),
        );
        route.insert(
            "host".to_owned(),
            serde_json::Value::String(route_host.clone()),
        );
        attachment.definition_hash = wamn_catalog::DefinitionHash::parse(
            wamn_execution_contract::canonical_json_sha256(&attachment.definition),
        )
        .expect("the shared canonicalizer emits a valid definition hash");
    }
    Ok(resolved)
}

/// The method of one routed attachment: GET for a route to a read operation,
/// POST for every other route and for every wiring.
fn route_method(
    attachment_id: &str,
    attachment: &ServingAttachment,
    route_kinds: &RouteKinds,
) -> Result<&'static str, MintManifestError> {
    let AttachmentTarget::Route { operation, .. } = &attachment.target else {
        return Ok("POST");
    };
    route_kinds
        .get(&(attachment.package_id.clone(), operation.clone()))
        .map(|kind| kind.http_method())
        .ok_or_else(|| {
            MintManifestError::new(
                MintManifestErrorKind::GeneratedPackageMetadata,
                format!(
                    "route attachment {attachment_id:?} calls operation {operation:?}, which has no generated contract kind; regenerate the package evidence"
                ),
            )
        })
}

/// Admit only package-owned route coordinates. The deployment hostname and
/// the method are deliberately absent from this schema: the hostname joins at
/// publication, and the method follows from the operation kind.
fn validate_authored_attachment_routes(
    authored: &BTreeMap<String, ServingAttachment>,
) -> Result<(), MintManifestError> {
    for (attachment_id, attachment) in authored {
        if attachment.definition.pointer("/route/host").is_some() {
            return Err(MintManifestError::new(
                MintManifestErrorKind::Document,
                format!(
                    "attachment {attachment_id:?} authors route.host; remove it and pass --route-host"
                ),
            ));
        }
        if matches!(
            attachment.kind,
            wamn_catalog::AttachmentKind::Http | wamn_catalog::AttachmentKind::Studio
        ) && attachment.definition.pointer("/route/method").is_some()
        {
            return Err(MintManifestError::new(
                MintManifestErrorKind::Document,
                format!(
                    "attachment {attachment_id:?} authors route.method; remove it, because the method follows from the operation kind"
                ),
            ));
        }
    }
    Ok(())
}

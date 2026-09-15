//! Package attachment documents and deployment route preparation.

use super::{
    AuthoredHttpRoute, BTreeMap, BTreeSet, MintManifestError, MintManifestErrorKind, PathBuf,
    ServingAttachment, canonical_http_route_template, normalize_http_route,
};

pub(super) fn read_package_attachments(
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
/// become a second hostname emitter.
pub(super) fn resolve_route_host_overlay(
    authored: &BTreeMap<String, ServingAttachment>,
    route_host: Option<&str>,
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
    for (attachment_id, attachment) in &mut resolved {
        if !matches!(
            attachment.kind,
            wamn_catalog::AttachmentKind::Http | wamn_catalog::AttachmentKind::Studio
        ) {
            continue;
        }
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

/// Admit only package-owned route coordinates. The deployment hostname is
/// deliberately absent from this schema and joins at publication.
fn validate_authored_attachment_routes(
    authored: &BTreeMap<String, ServingAttachment>,
) -> Result<(), MintManifestError> {
    let mut route_keys = BTreeSet::new();
    for (attachment_id, attachment) in authored {
        if attachment.definition.pointer("/route/host").is_some() {
            return Err(MintManifestError::new(
                MintManifestErrorKind::Document,
                format!(
                    "attachment {attachment_id:?} authors route.host; remove it and pass --route-host"
                ),
            ));
        }
        if !matches!(
            attachment.kind,
            wamn_catalog::AttachmentKind::Http | wamn_catalog::AttachmentKind::Studio
        ) {
            continue;
        }
        let route = attachment
            .definition
            .get("route")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let route = serde_json::from_value::<AuthoredHttpRoute>(route).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Document,
                format!(
                    "attachment {attachment_id:?} route must contain exactly string path and method fields"
                ),
                error,
            )
        })?;
        let route = normalize_http_route(&route, attachment_id).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Document,
                format!("attachment {attachment_id:?} carries an invalid route"),
                error,
            )
        })?;
        let key = (canonical_http_route_template(&route.path), route.method);
        if !route_keys.insert(key) {
            return Err(MintManifestError::new(
                MintManifestErrorKind::Document,
                format!(
                    "attachment {attachment_id:?} duplicates another attachment's canonical path and method"
                ),
            ));
        }
    }
    Ok(())
}

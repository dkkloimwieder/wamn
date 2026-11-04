use super::{
    AttachmentTarget, AuthoredHttpRoute, AuthoredRoute, BTreeMap, BTreeSet, PathBuf,
    PublishManifestError, PublishManifestErrorType, RouteContracts, ServingAttachment,
    canonical_http_route_template, normalize_http_route,
};

pub(in super::super) fn merge_package_attachment_documents(
    documents: Vec<(PathBuf, BTreeMap<String, ServingAttachment>)>,
) -> Result<BTreeMap<String, ServingAttachment>, PublishManifestError> {
    let mut merged = BTreeMap::new();
    let mut sources = BTreeMap::<String, PathBuf>::new();
    for (path, attachments) in documents {
        for (attachment_id, attachment) in attachments {
            if let Some(first_path) = sources.get(&attachment_id) {
                return Err(PublishManifestError::new(
                    PublishManifestErrorType::DuplicateAttachmentId,
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
/// The release publish is the production boundary that accepts the authored
/// attachment map. Comparing here prevents a caller from pairing an unchanged
/// identity claim with different route or contract bytes before either can
/// enter the immutable serving snapshot.
pub(in super::super) fn validate_attachment_definition_hashes(
    attachments: &BTreeMap<String, ServingAttachment>,
) -> Result<(), PublishManifestError> {
    for (attachment_id, attachment) in attachments {
        let derived = wamn_execution_contract::canonical_json_sha256(&attachment.definition);
        if attachment.definition_hash.as_str() != derived {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::Document,
                format!(
                    "attachment {attachment_id:?} definition-hash {} differs from canonical definition hash {derived}",
                    attachment.definition_hash.as_str(),
                ),
            ));
        }
    }
    Ok(())
}

/// Write each route's method from the kind of the operation it calls, and
/// refuse ambiguous routes.
///
/// The release names no route host (R1): a route is its path and method, and
/// the host configuration and the http workload's `config.host` name the host.
pub(in super::super) fn resolve_route_methods(
    authored: &BTreeMap<String, ServingAttachment>,
    route_contracts: &RouteContracts,
) -> Result<BTreeMap<String, ServingAttachment>, PublishManifestError> {
    validate_authored_attachment_routes(authored)?;
    validate_attachment_definition_hashes(authored)?;
    let mut resolved = authored.clone();
    let mut route_keys = BTreeSet::new();
    for (attachment_id, attachment) in &mut resolved {
        if !matches!(
            attachment.type_,
            wamn_catalog::AttachmentType::Http | wamn_catalog::AttachmentType::Studio
        ) {
            continue;
        }
        let method = route_method(attachment_id, attachment, route_contracts)?;
        let route = attachment
            .definition
            .as_object_mut()
            .and_then(|definition| definition.get_mut("route"))
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| {
                PublishManifestError::new(
                    PublishManifestErrorType::Document,
                    format!("attachment {attachment_id:?} carries no route object"),
                )
            })?;
        let AuthoredRoute { path } = serde_json::from_value(serde_json::Value::Object(
            route.clone(),
        ))
        .map_err(|error| {
            PublishManifestError::with_source(
                PublishManifestErrorType::Document,
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
            PublishManifestError::with_source(
                PublishManifestErrorType::Document,
                format!("attachment {attachment_id:?} carries an invalid route"),
                error,
            )
        })?;
        if normalized
            .path
            .starts_with(wamn_catalog::host_route_path_prefix())
        {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::Document,
                format!(
                    "attachment {attachment_id:?} uses the path prefix {:?} of the platform's host routes",
                    wamn_catalog::host_route_path_prefix()
                ),
            ));
        }
        if !route_keys.insert((
            canonical_http_route_template(&normalized.path),
            normalized.method,
        )) {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::Document,
                format!(
                    "attachment {attachment_id:?} duplicates another attachment's canonical path and method"
                ),
            ));
        }
        route.insert(
            "method".to_owned(),
            serde_json::Value::String(method.to_owned()),
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
pub(super) fn route_method(
    attachment_id: &str,
    attachment: &ServingAttachment,
    route_contracts: &RouteContracts,
) -> Result<&'static str, PublishManifestError> {
    let AttachmentTarget::Route { operation, .. } = &attachment.target;
    route_contracts
        .get(&(attachment.package_id.clone(), operation.clone()))
        .map(|contract| contract.kind.http_method())
        .ok_or_else(|| {
            PublishManifestError::new(
                PublishManifestErrorType::PackageManifest,
                format!(
                    "route attachment {attachment_id:?} calls operation {operation:?}, which has no generated contract kind; regenerate the package evidence"
                ),
            )
        })
}

/// Admit only package-owned route coordinates. The hostname and the method
/// are deliberately absent from this schema: no release names a hostname
/// (R1), and the method follows from the operation kind.
pub(super) fn validate_authored_attachment_routes(
    authored: &BTreeMap<String, ServingAttachment>,
) -> Result<(), PublishManifestError> {
    for (attachment_id, attachment) in authored {
        if attachment.definition.pointer("/route/host").is_some() {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::Document,
                format!(
                    "attachment {attachment_id:?} authors route.host; remove it, because the host configuration names the route host"
                ),
            ));
        }
        if matches!(
            attachment.type_,
            wamn_catalog::AttachmentType::Http | wamn_catalog::AttachmentType::Studio
        ) && attachment.definition.pointer("/route/method").is_some()
        {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::Document,
                format!(
                    "attachment {attachment_id:?} authors route.method; remove it, because the method follows from the operation kind"
                ),
            ));
        }
    }
    Ok(())
}

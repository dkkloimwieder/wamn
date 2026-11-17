use super::{
    AttachmentTarget, AuthoredHttpRoute, AuthoredRoute, BTreeMap, BTreeSet, PathBuf,
    PublishManifestError, PublishManifestErrorType, RouteContracts, ServingAttachment,
    canonical_http_route_template, normalize_http_route,
};

pub(in super::super) fn merge_package_attachment_documents(
    documents: Vec<(
        PathBuf,
        BTreeMap<wamn_catalog::canonical_name::CanonicalName, ServingAttachment>,
    )>,
) -> Result<
    BTreeMap<wamn_catalog::canonical_name::CanonicalName, ServingAttachment>,
    PublishManifestError,
> {
    let mut merged = BTreeMap::new();
    let mut sources = BTreeMap::<wamn_catalog::canonical_name::CanonicalName, PathBuf>::new();
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

/// Write each route's method from the type of the operation it calls, and
/// refuse ambiguous routes.
///
/// The release names no route host (R1): a route is its path and method, and
/// the host configuration and the http workload's `config.host` name the host.
pub(in super::super) fn resolve_route_methods(
    authored: &BTreeMap<wamn_catalog::canonical_name::CanonicalName, ServingAttachment>,
    route_contracts: &RouteContracts,
) -> Result<
    BTreeMap<wamn_catalog::canonical_name::CanonicalName, ServingAttachment>,
    PublishManifestError,
> {
    validate_authored_attachment_routes(authored)?;
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
            attachment_id.as_str(),
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
        // Refuse both the forwarded stream URI and its browser /api form.
        let file_prefix = |path: &str| {
            path.strip_prefix('/')
                .and_then(|path| path.split_once('/'))
                .is_some_and(|(_, path)| path == "files" || path.starts_with("files/"))
        };
        if file_prefix(&normalized.path)
            || normalized.path.strip_prefix("/api").is_some_and(file_prefix)
        {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::Document,
                format!("attachment {attachment_id:?} uses the platform's file stream prefix"),
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
        attachment.definition_hash = wamn_digest::canonical_json_sha256(&attachment.definition);
    }
    Ok(resolved)
}

/// The method of one routed attachment: GET for a route to a read operation,
/// POST for every other route and for every wiring.
pub(super) fn route_method(
    attachment_id: &wamn_catalog::canonical_name::CanonicalName,
    attachment: &ServingAttachment,
    route_contracts: &RouteContracts,
) -> Result<&'static str, PublishManifestError> {
    let AttachmentTarget::Route { operation, .. } = &attachment.target;
    route_contracts
        .get(&(attachment.package_id.clone(), operation.clone()))
        .map(|contract| contract.type_.http_method())
        .ok_or_else(|| {
            PublishManifestError::new(
                PublishManifestErrorType::PackageManifest,
                format!(
                    "route attachment {attachment_id:?} calls operation {operation:?}, which has no generated contract type; regenerate the package evidence"
                ),
            )
        })
}

/// Admit only package-owned route coordinates. The hostname and the method
/// are deliberately absent from this schema: no release names a hostname
/// (R1), and the method follows from the operation type.
pub(super) fn validate_authored_attachment_routes(
    authored: &BTreeMap<wamn_catalog::canonical_name::CanonicalName, ServingAttachment>,
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
                    "attachment {attachment_id:?} authors route.method; remove it, because the method follows from the operation type"
                ),
            ));
        }
    }
    Ok(())
}

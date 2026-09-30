//! Package attachment documents and deployment route preparation.

use super::{
    AuthoredHttpRoute, BTreeMap, BTreeSet, MintManifestError, MintManifestErrorType, PathBuf,
    RouteContracts, ServingAttachment, canonical_http_route_template, normalize_http_route,
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
    let mut attachments = read_authored_attachments(paths, &package_owners(package_manifests)?)?;
    resolve_generated_input_schemas(&mut attachments, package_manifests)?;
    Ok(attachments)
}

/// The operation owners of each package the release presents, by package id.
fn package_owners(
    package_manifests: &[PathBuf],
) -> Result<BTreeMap<String, wamn_schema_generator::OperationOwners>, MintManifestError> {
    let mut owners = BTreeMap::new();
    for path in package_manifests {
        let bytes = std::fs::read(path).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorType::PackageManifest,
                format!("read package manifest {}", path.display()),
                error,
            )
        })?;
        let package =
            wamn_schema_generator::OperationOwners::from_slice(&bytes).map_err(|error| {
                MintManifestError::with_source(
                    MintManifestErrorType::PackageManifest,
                    format!("parse package manifest {}", path.display()),
                    error,
                )
            })?;
        owners.insert(package.package.id.clone(), package);
    }
    Ok(owners)
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
                MintManifestErrorType::PackageManifest,
                format!("read package manifest {}", path.display()),
                error,
            )
        })?;
        let manifest =
            wamn_schema_generator::PackageManifest::from_slice(&bytes).map_err(|error| {
                MintManifestError::with_source(
                    MintManifestErrorType::PackageManifest,
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
                MintManifestErrorType::Document,
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
                MintManifestErrorType::Document,
                format!("attachment {attachment_id:?} input schema"),
                error,
            )
        })?;
    }
    Ok(())
}

fn read_authored_attachments(
    paths: &[PathBuf],
    owners: &BTreeMap<String, wamn_schema_generator::OperationOwners>,
) -> Result<BTreeMap<String, ServingAttachment>, MintManifestError> {
    if paths.is_empty() {
        return Err(MintManifestError::new(
            MintManifestErrorType::Document,
            "publish-release requires at least one package-owned --attachments document",
        ));
    }
    let mut documents = Vec::new();
    for path in paths {
        let bytes = std::fs::read(path).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorType::Document,
                format!("read package attachments {}", path.display()),
                error,
            )
        })?;
        let parse = |error| {
            MintManifestError::with_source(
                MintManifestErrorType::Document,
                format!("parse package attachments {}", path.display()),
                error,
            )
        };
        let mut document: BTreeMap<String, serde_json::Value> =
            serde_json::from_slice(&bytes).map_err(parse)?;
        // An authored entry names its operation by reference. The version
        // comes from the wamn.json the release presents for its package.
        for (attachment_id, entry) in &mut document {
            let package_id = entry
                .get("package-id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let package = owners.get(package_id).ok_or_else(|| {
                MintManifestError::new(
                    MintManifestErrorType::Document,
                    format!(
                        "attachment {attachment_id:?} names package {package_id:?}, and the release presents no wamn.json for it"
                    ),
                )
            })?;
            wamn_schema_generator::operation_reference::resolve_attachment_entry(
                attachment_id,
                entry,
                package,
            )
            .map_err(|error| {
                MintManifestError::with_source(
                    MintManifestErrorType::Document,
                    format!("package attachments {}", path.display()),
                    error,
                )
            })?;
        }
        let attachments = document
            .into_iter()
            .map(|(attachment_id, entry)| {
                serde_json::from_value(entry).map(|attachment| (attachment_id, attachment))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(parse)?;
        documents.push((path.clone(), attachments));
        // Generation writes the route entries of the package's generated
        // operations beside the authored document.
        let root = wamn_schema_generator::route_schema::package_root_of(path);
        let generated = wamn_schema_generator::route_schema::read_generated_publication(
            root,
            wamn_schema_generator::route_schema::GENERATED_ATTACHMENTS,
        )
        .and_then(wamn_schema_generator::route_schema::generated_attachments)
        .map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorType::Document,
                format!("read the generated attachments beside {}", path.display()),
                error,
            )
        })?;
        if !generated.is_empty() {
            documents.push((
                root.join(wamn_schema_generator::route_schema::GENERATED_ATTACHMENTS),
                generated,
            ));
        }
    }
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
                    MintManifestErrorType::DuplicateAttachmentId,
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
                MintManifestErrorType::Document,
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
    route_contracts: &RouteContracts,
) -> Result<BTreeMap<String, ServingAttachment>, MintManifestError> {
    validate_authored_attachment_routes(authored)?;
    validate_attachment_definition_hashes(authored)?;
    let first_routed = authored.iter().find(|(_, attachment)| {
        matches!(
            attachment.type_,
            wamn_catalog::AttachmentType::Http | wamn_catalog::AttachmentType::Studio
        )
    });
    let Some((first_attachment_id, _)) = first_routed else {
        return Ok(authored.clone());
    };
    let route_host = route_host.filter(|host| !host.is_empty()).ok_or_else(|| {
        MintManifestError::new(
            MintManifestErrorType::RouteHostUnbound,
            format!(
                "attachment {first_attachment_id:?} requires deployment route host; pass --route-host"
            ),
        )
    })?;
    if route_host != "*"
        && (route_host.contains('/') || route_host.chars().any(char::is_whitespace))
    {
        return Err(MintManifestError::new(
            MintManifestErrorType::Document,
            format!("deployment route host {route_host:?} is invalid"),
        ));
    }
    let route_host = route_host.to_ascii_lowercase();
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
                MintManifestError::new(
                    MintManifestErrorType::Document,
                    format!("attachment {attachment_id:?} carries no route object"),
                )
            })?;
        let AuthoredRoute { path } = serde_json::from_value(serde_json::Value::Object(
            route.clone(),
        ))
        .map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorType::Document,
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
                MintManifestErrorType::Document,
                format!("attachment {attachment_id:?} carries an invalid route"),
                error,
            )
        })?;
        if !route_keys.insert((
            canonical_http_route_template(&normalized.path),
            normalized.method,
        )) {
            return Err(MintManifestError::new(
                MintManifestErrorType::Document,
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
    route_contracts: &RouteContracts,
) -> Result<&'static str, MintManifestError> {
    let AttachmentTarget::Route { operation, .. } = &attachment.target else {
        return Ok("POST");
    };
    route_contracts
        .get(&(attachment.package_id.clone(), operation.clone()))
        .map(|contract| contract.kind.http_method())
        .ok_or_else(|| {
            MintManifestError::new(
                MintManifestErrorType::GeneratedPackageMetadata,
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
                MintManifestErrorType::Document,
                format!(
                    "attachment {attachment_id:?} authors route.host; remove it and pass --route-host"
                ),
            ));
        }
        if matches!(
            attachment.type_,
            wamn_catalog::AttachmentType::Http | wamn_catalog::AttachmentType::Studio
        ) && attachment.definition.pointer("/route/method").is_some()
        {
            return Err(MintManifestError::new(
                MintManifestErrorType::Document,
                format!(
                    "attachment {attachment_id:?} authors route.method; remove it, because the method follows from the operation kind"
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The route hashes and the whole attachment set of the fixture, as publish
    /// read them before generation wrote the generated route entries.
    const FIXTURE_ROUTES: &[(&str, &str)] = &[
        (
            "widget-archive-http",
            "sha256:258f16829d08346de513cc9d0fb123ec4b8e9eb6ae9696c68da5ccfa252a2b69",
        ),
        (
            "widget-create-http",
            "sha256:9503afb0a101e407ff840430d229fd1628c83605963921b9877b6057926e9c9d",
        ),
        (
            "widget-delete-http",
            "sha256:96d3221727142dc6745fc078b06d64c67b4e3afb95921bcbaf7b5337d296c121",
        ),
        (
            "widget-get-http",
            "sha256:be12a2d0f49cc998c581271aa6bdfc131bd552c4ea73a6da7e7ecd6f9dfc1dc2",
        ),
        (
            "widget-list-http",
            "sha256:6108c568f9b01d428f2f100c0b3e4f2c97e6c91291676d5d17f127c8714ea3e6",
        ),
        (
            "widget-maker-get-http",
            "sha256:178499cb0949cf49768312369ccbdeede8dce515c6bce049769a1e946f9ad8b2",
        ),
        (
            "widget-maker-list-http",
            "sha256:b76b3cba4edb6be3dfd8767dc2324265133efa093af564fa4149d5292baccf1e",
        ),
        (
            "widget-maker-query-http",
            "sha256:6956775292590cfff211573833979b9d13ce915e6543a97a574551bb69ac8f9a",
        ),
        (
            "widget-query-http",
            "sha256:288363232c077fe60133f6191c06d222bce9982b5cedd6f7805f5ed0382f1125",
        ),
        (
            "widget-record-batch-http",
            "sha256:8fcea4457fb67d951ddaa86ec9bcda160399a85ac98523a1006327f74fe74b09",
        ),
        (
            "widget-tag-update-http",
            "sha256:1543ab164fe1711f6a22ce07d33ae304ccb1f7ec5fedc38149387f74201659bd",
        ),
        (
            "widget-update-http",
            "sha256:1518132fc051dfe9e421730f88a952e1cde0bbd75ffa8e319c228a67058327d4",
        ),
    ];
    const FIXTURE_ATTACHMENTS_DIGEST: &str =
        "sha256:39be5e1be0c9196ea30b12dfac907705fbc82e968887ffa8d527b2dbe5c27458";
    /// The rendered fixture declaration, before generation wrote its entries.
    const FIXTURE_DECLARATION_DIGEST: &str =
        "sha256:e6770014d60668c5e8ed4f12a078576c2c884c398cff8ca1782b09e601e63fef";

    /// A publish of the fixture reads the same routes and the same component
    /// declaration from the generated entries as from the authored ones they
    /// replaced (wamn-iowb.3).
    #[test]
    fn the_fixture_publishes_what_it_published_before_generated_route_entries() {
        let root = wamn_fixture_package::package_root();
        let attachments = read_package_attachments(
            &[root.join("publication/attachments.json")],
            &[root.join("wamn.json")],
        )
        .expect("read the fixture attachments");
        let routes = attachments
            .iter()
            .map(|(id, attachment)| (id.as_str(), attachment.definition_hash.as_str()))
            .collect::<Vec<_>>();
        let digest = wamn_execution_contract::canonical_json_sha256(
            &serde_json::to_value(&attachments).expect("attachments serialize"),
        );
        let declaration = crate::component_declaration::render_declaration_document(
            &root.join("publication/components/fixture.json.in"),
            "tenant-a",
            &crate::component_declaration::authored_base_digests(&root)
                .expect("read the fixture base digests"),
        )
        .expect("render the fixture declaration");
        let declaration = wamn_execution_contract::canonical_json_sha256(&declaration);
        assert_eq!(routes, FIXTURE_ROUTES);
        assert_eq!(digest, FIXTURE_ATTACHMENTS_DIGEST);
        assert_eq!(declaration, FIXTURE_DECLARATION_DIGEST);
    }
}

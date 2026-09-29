//! Package attachment documents and deployment route preparation.

use super::{
    AuthoredHttpRoute, BTreeMap, BTreeSet, MintManifestError, MintManifestErrorKind, PathBuf,
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
                MintManifestErrorKind::PackageManifest,
                format!("read package manifest {}", path.display()),
                error,
            )
        })?;
        let package =
            wamn_schema_generator::OperationOwners::from_slice(&bytes).map_err(|error| {
                MintManifestError::with_source(
                    MintManifestErrorKind::PackageManifest,
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
    owners: &BTreeMap<String, wamn_schema_generator::OperationOwners>,
) -> Result<BTreeMap<String, ServingAttachment>, MintManifestError> {
    if paths.is_empty() {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Document,
            "publish-release requires at least one package-owned --attachments document",
        ));
    }
    let mut documents = Vec::new();
    for path in paths {
        let bytes = std::fs::read(path).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Document,
                format!("read package attachments {}", path.display()),
                error,
            )
        })?;
        let parse = |error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Document,
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
                    MintManifestErrorKind::Document,
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
                    MintManifestErrorKind::Document,
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
                MintManifestErrorKind::Document,
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
    route_contracts: &RouteContracts,
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
        let method = route_method(attachment_id, attachment, route_contracts)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The route hashes and the whole attachment set of the fixture, as publish
    /// read them before generation wrote the generated route entries.
    const FIXTURE_ROUTES: &[(&str, &str)] = &[
        (
            "widget-archive-http",
            "sha256:318e0ed97696b8fa54040bf7fdf8d44251ff7aa18acc475b6a7464d1cd5e3535",
        ),
        (
            "widget-create-http",
            "sha256:4189444939f50079641e7492fc76f00227d87c31617c9533d2bbbf0f92ebbc11",
        ),
        (
            "widget-delete-http",
            "sha256:f03abc2aeb927967fe464cc8d95103aea930cb7587923dd4cc4a5e2f81570937",
        ),
        (
            "widget-get-http",
            "sha256:8c02eb64b189b763080d8b33c505ac8ac728bdd5556a772318e306511f78a788",
        ),
        (
            "widget-list-http",
            "sha256:e5905f348a94051788ffd9eece1c61e6716ea6864e56cf25dc3d2ac93041c6ed",
        ),
        (
            "widget-maker-get-http",
            "sha256:039d448ec7fbdf2bed46c6e0809f297fc4ed031fb67687177c363cea7bbafd25",
        ),
        (
            "widget-maker-list-http",
            "sha256:45ff1b1784d3f7799fea1e777a960099d0c4589c94eec78220ad17579bcc094c",
        ),
        (
            "widget-maker-query-http",
            "sha256:3be20be39c575f8deb827f95bc441840e8cd4fc9c9efb92ffe4dca22f9a9668e",
        ),
        (
            "widget-query-http",
            "sha256:6c98260e18b130caf89668deacb9d14a1d34d973ab2068d952c38751920a17e8",
        ),
        (
            "widget-record-batch-http",
            "sha256:c9617868c09c438f4e03634c9f21ccc5f21e6d6f870af592b691f0bf579d5d2b",
        ),
        (
            "widget-tag-update-http",
            "sha256:4f99f1c2a78e9cb92500b04b3454e827e2e672a2721beda0654cd3db0d430b0c",
        ),
        (
            "widget-update-http",
            "sha256:11aa126e11433c107a43b1a1e6a36f47471a603bf768196fa0c22c5eac5f800e",
        ),
    ];
    const FIXTURE_ATTACHMENTS_DIGEST: &str =
        "sha256:0a63a518eeb9ac7bcccf03a1938589fd4cf5e0f65b641cd851842802a873d802";
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

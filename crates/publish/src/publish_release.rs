pub(super) mod plan;
pub use plan::{
    application_dependency_permissions, composes_integration, deploy_runtime_schema, parse_package,
};
use plan::{
    pin_runtime_schemas, project_components, project_routes,
    validate_request,
};

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context as _, ensure};

use wamn_component_policy::AdmittedComponent;
use wamn_edge_document::{
    SERVING_MANIFEST_FORMAT_VERSION, ServingComponent, ServingManifest, ServingRelation,
    ServingRelease, ServingRoute,
};
use wamn_package::{
    AttachmentTarget, ComponentConnection, OperationType, PackageCoordinate, ServingAttachment,
};
use wamn_routes::exposure::{
    HttpRoute as AuthoredHttpRoute, canonical_http_route_template, normalize_http_route,
};

mod attachments;
mod components;
mod package_sources;

use attachments::{read_package_attachments, resolve_route_methods};
use components::{resolve_component_dependency_closure, resolve_route_component};
use package_sources::read_package_manifests;
pub use package_sources::{package_route, route_contracts};

#[cfg(test)]
use attachments::merge_package_attachment_documents;

/// Inputs owned by the release publisher.
#[derive(Debug)]
pub struct PublishReleaseManifest<'a> {
    pub org: &'a wamn_registry::OrgId,
    pub project: &'a wamn_registry::ProjectId,
    pub environment: &'a wamn_registry::EnvName,
    pub packages: &'a BTreeSet<PackageCoordinate>,
    pub attachments: &'a BTreeMap<wamn_package::canonical_name::CanonicalName, ServingAttachment>,
    /// Local development can use rebuilt dependencies after target validation.
    /// Published construction always requires the declared dependency digests.
    pub environment_is_disposable: bool,
}

impl PublishReleaseManifest<'_> {
    /// The composed environment key derived from the environment coordinate.
    pub fn project_env_key(&self) -> wamn_registry::ProjectEnvKey {
        wamn_registry::project_env_key(self.org, self.project, self.environment)
    }
}

/// How a release matches a declared dependency to an admitted component fact.
///
/// `Declared` is the durable rule: a publish names the exact bytes it depends
/// on, and the digest pinned in the authored manifest IS that declaration.
///
/// `Built` is the development rule, ruled 2026-09-08 on wamn-10yt.48. A
/// disposable target rebuilt the base from the same tree in the same run, so
/// the base it built is the honest dependency and the declared digest names
/// bytes that no longer exist. Matching by coordinate and operation there costs
/// nothing, because the run built both halves. A durable publish keeps
/// demanding the exact bytes, as it must.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DependencyDigestRule {
    Declared,
    Built,
}

impl DependencyDigestRule {
    /// The rule a target's projected environment marker selects.
    pub const fn for_environment(disposable: bool) -> Self {
        if disposable {
            Self::Built
        } else {
            Self::Declared
        }
    }

    /// Whether a fact must carry the digest the declaration named.
    pub const fn matches_declared_digest(self) -> bool {
        matches!(self, Self::Declared)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PublishedRelease {
    pub manifest: ServingManifest,
    pub digest: wamn_digest::Digest,
    pub canonical_bytes: Vec<u8>,
}

pub const RELEASE_MANIFEST_MINT_REFUSAL: &str = "release-manifest-mint-refused";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishManifestErrorType {
    Release,
    PackageManifest,
    Component,
    OperationDependency,
    UnauthenticatedRegisteredOperation,
    UnauthenticatedWrite,
    ClosureConflict,
    Document,
    DuplicateAttachmentId,
}

impl PublishManifestErrorType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::PackageManifest => "package-manifest",
            Self::Component => "component",
            Self::OperationDependency => "operation-dependency",
            Self::UnauthenticatedRegisteredOperation => "unauthenticated-registered-operation",
            Self::UnauthenticatedWrite => "unauthenticated-write",
            Self::ClosureConflict => "closure-conflict",
            Self::Document => "document",
            Self::DuplicateAttachmentId => "duplicate-attachment-id",
        }
    }
}

#[derive(Debug)]
pub struct PublishManifestError {
    type_: PublishManifestErrorType,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl PublishManifestError {
    pub fn new(type_: PublishManifestErrorType, detail: impl Into<String>) -> Self {
        Self {
            type_,
            detail: detail.into(),
            source: None,
        }
    }

    pub fn with_source(
        type_: PublishManifestErrorType,
        detail: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            type_,
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }

    pub const fn error_type(&self) -> PublishManifestErrorType {
        self.type_
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl std::fmt::Display for PublishManifestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{RELEASE_MANIFEST_MINT_REFUSAL} ({}): {}",
            self.type_.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for PublishManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// The facts a route takes from the generated contract of the operation it
/// calls, keyed by package id and exact operation.
pub type RouteContracts = BTreeMap<
    (
        wamn_package::PackageName,
        wamn_package::canonical_name::CanonicalName,
    ),
    RouteContract,
>;

/// What a route takes from the generated contract `operation.json` of its
/// operation: the type, the relations a read reads, and the revision field of
/// a `get`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteContract {
    pub type_: OperationType,
    reads: BTreeSet<ServingRelation>,
    revision: Option<String>,
    pub idempotency: Option<String>,
}

/// Inputs of one application release publication.
#[derive(Debug)]
pub struct PublishReleaseRequest {
    /// Organization in the declared environment coordinate.
    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    /// Environment the release is published for.
    pub environment: wamn_registry::EnvName,
    /// Available exact coordinates for the declared application packages.
    pub packages: Vec<PackageCoordinate>,
}

impl PublishReleaseRequest {
    /// The composed environment key derived from the environment coordinate.
    pub fn project_env_key(&self) -> wamn_registry::ProjectEnvKey {
        wamn_registry::project_env_key(&self.org, &self.project, &self.environment)
    }
}

/// Build ingress routes from the deployed package closure and its admitted exports.
pub fn deployment_routes(
    packages: &BTreeMap<wamn_package::PackageName, wamn_component_policy::PackageManifest>,
    contracts: &RouteContracts,
    application: &wamn_registry::ApplicationName,
    digest: &wamn_digest::Digest,
) -> anyhow::Result<Vec<wamn_routes::http_route_data::RouteDefinition>> {
    let documents = packages
        .values()
        .map(|package| {
            let deploy = package.deploy.as_ref().with_context(|| {
                format!(
                    "package {} has no compiled deploy section",
                    package.package.id
                )
            })?;
            Ok((
                PathBuf::from(package.package.id.as_str()),
                deploy.routes.clone(),
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let attachments = attachments::merge_package_attachment_documents(documents)?;
    let attachments = attachments::plan::resolve_owned_route_methods(attachments, contracts)?;
    let workflow_prefix = wamn_routes::workflow_http_prefix(application);
    let mut routes = Vec::with_capacity(attachments.len());
    for (id, attachment) in attachments {
        if !matches!(
            attachment.type_,
            wamn_package::AttachmentType::Http | wamn_package::AttachmentType::Studio
        ) {
            continue;
        }
        ensure!(
            !attachment.definition["route"]["path"]
                .as_str()
                .is_some_and(|path| {
                    path.strip_prefix(&workflow_prefix)
                        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
                }),
            "attachment {id} uses the application's workflow path {workflow_prefix}"
        );
        let AttachmentTarget::Route {
            component,
            operation,
        } = &attachment.target;
        let export = packages
            .get(&attachment.package_id)
            .and_then(|package| package.deploy.as_ref())
            .and_then(|deploy| deploy.components.get(component))
            .and_then(|component| component.descriptor.operations.get(operation))
            .with_context(|| {
                format!("attachment {id} names an operation that no admitted component exports")
            })?;
        let policy = wamn_package::parse_attachment_auth_policy(&attachment.auth_policy)
            .with_context(|| format!("attachment {id} has an invalid auth policy"))?;
        if policy.has_no_caller() {
            components::refuse_anonymous_reach(&id, export, operation.as_str())?;
        }
        let contract = contracts
            .get(&(attachment.package_id.clone(), operation.clone()))
            .with_context(|| format!("attachment {id} names no compiled operation contract"))?;
        let signs_files = export.output_ports.iter().any(|port| {
            port.schema
                .schema
                .get(wamn_package::SIGNED_FILE_READS)
                .is_some()
        });
        ensure!(
            contract.type_ != OperationType::EventHandler,
            "attachment {id} names an event handler"
        );
        let route = ServingRoute {
            package_id: attachment.package_id,
            component: component.clone(),
            operation: operation.clone(),
            type_: contract.type_,
            reads: contract.reads.clone(),
            revision: contract.revision.clone(),
            idempotency: contract.idempotency.clone(),
        };
        routes.push(
            wamn_edge_document::http_routes::route_definition(
                &id,
                &attachment.definition,
                &attachment.auth_policy,
                &route,
                signs_files,
                digest,
            )
            .with_context(|| format!("attachment {id} has an invalid route definition"))?,
        );
    }
    Ok(routes)
}

/// An unpublished release assembled for an edge bundle.
#[derive(Debug)]
pub struct AssembledRelease {
    pub published: PublishedRelease,
    pub components: Vec<AdmittedComponent>,
}

/// Assemble an edge release from package directories and admitted components.
/// This operation reads no database and publishes no artifacts.
pub fn assemble_release(
    args: &PublishReleaseRequest,
    package_roots: &[PathBuf],
    admissions: &[crate::component_admission::ComponentAdmission],
    palette: &[crate::palette::PaletteComponent],
) -> anyhow::Result<AssembledRelease> {
    let authored = read_package_attachments(package_roots)?;
    let (package_manifests, route_contracts) = read_package_manifests(package_roots)?;
    let attachments = resolve_route_methods(&authored, &route_contracts)?;
    let packages = args.packages.iter().cloned().collect::<BTreeSet<_>>();
    ensure!(
        packages.len() == args.packages.len(),
        "local candidate repeats a package"
    );
    let request = PublishReleaseManifest {
        org: &args.org,
        project: &args.project,
        environment: &args.environment,
        packages: &packages,
        attachments: &attachments,
        environment_is_disposable: true,
    };
    let mut release_components = ReleaseComponents::default();
    for admission in admissions {
        let fact = admission.facts();
        ensure!(
            fact.target.project_env_key == args.project_env_key()
                && packages
                    .iter()
                    .any(|package| package.package_id() == &fact.target.package_id
                        && package.package_version() == &fact.target.package_version),
            "local admission is outside package membership"
        );
        release_components.insert(fact.clone(), admission.descriptor().connections)?;
    }
    let mut published = publish_release_with_package_manifests(
        &request,
        &release_components,
        &package_manifests,
        &route_contracts,
    )?;
    for component in palette {
        published
            .manifest
            .components
            .insert(ServingComponent::project(
                &component.facts.component,
                components::palette_connections(&component.facts.connections, &package_manifests),
            ));
    }
    if !palette.is_empty() {
        published = seal_release(&published.manifest)?;
    }
    let mut components: Vec<AdmittedComponent> = admissions
        .iter()
        .map(crate::component_admission::ComponentAdmission::facts)
        .filter(|fact| {
            published
                .manifest
                .components
                .iter()
                .any(|component| component.descriptor.component_digest == fact.component_digest)
        })
        .cloned()
        .collect();
    components.extend(
        palette
            .iter()
            .map(|component| component.facts.component.clone()),
    );
    Ok(AssembledRelease {
        published,
        components,
    })
}

pub fn publish_release_with_package_manifests(
    request: &PublishReleaseManifest<'_>,
    component_facts: &ReleaseComponents,
    package_manifests: &BTreeMap<wamn_package::PackageName, wamn_component_policy::PackageManifest>,
    route_contracts: &RouteContracts,
) -> Result<PublishedRelease, PublishManifestError> {
    validate_request(request)?;
    validate_release_package_manifests(request, package_manifests)?;

    let mut components = project_components(component_facts)?;
    let routes = project_routes(
        request,
        route_contracts,
        component_facts,
        package_manifests,
        &mut components,
    )?;
    let route_attachments = wamn_edge_document::route_attachments(request.attachments.clone());

    let projected = ServingManifest {
        composed_entry: None,
        format_version: SERVING_MANIFEST_FORMAT_VERSION,
        release: ServingRelease {
            packages: request.packages.clone(),
        },
        components: pin_runtime_schemas(components, package_manifests)?,
        routes,
        attachments: route_attachments,
        // Every application release serves the fixed application administration
        // routes (docs/architecture/execution.md#host-run-routes).
        host_routes: BTreeSet::from([wamn_routes::HostRouteSet::Application]),
    };
    seal_release(&projected)
}

fn seal_release(projected: &ServingManifest) -> Result<PublishedRelease, PublishManifestError> {
    let canonical_bytes = projected.canonical_bytes();
    let (manifest, digest) =
        ServingManifest::from_canonical_bytes(&canonical_bytes).map_err(|error| {
            PublishManifestError::with_source(
                PublishManifestErrorType::Document,
                "the release does not project a deliverable format-11 manifest",
                error,
            )
        })?;
    Ok(PublishedRelease {
        manifest,
        digest,
        canonical_bytes,
    })
}

/// The admitted components of the release packages: the facts of each package
/// by `(package_id, package_version)`, and the declared connections of each
/// component by `(package_id, component)`. Together they are the descriptors
/// the release carries.
#[derive(Debug, Default)]
pub struct ReleaseComponents {
    facts: BTreeMap<
        (
            wamn_package::PackageName,
            wamn_package::package_version::PackageVersion,
        ),
        Vec<AdmittedComponent>,
    >,
    connections: BTreeMap<
        (
            wamn_package::PackageName,
            wamn_package::canonical_name::CanonicalName,
        ),
        Vec<ComponentConnection>,
    >,
}

impl ReleaseComponents {
    /// Add one admitted component and its connections. A package names each
    /// component once.
    pub fn insert(
        &mut self,
        fact: AdmittedComponent,
        connections: Vec<ComponentConnection>,
    ) -> anyhow::Result<()> {
        ensure!(
            self.connections
                .insert(
                    (fact.target.package_id.clone(), fact.component.clone()),
                    connections,
                )
                .is_none(),
            "package {}@{} names component {} twice",
            fact.target.package_id,
            fact.target.package_version,
            fact.component
        );
        self.facts
            .entry((
                fact.target.package_id.clone(),
                fact.target.package_version.clone(),
            ))
            .or_default()
            .push(fact);
        Ok(())
    }

    /// The declared connections of `fact`.
    fn connections(
        &self,
        fact: &AdmittedComponent,
    ) -> Result<Vec<ComponentConnection>, PublishManifestError> {
        self.connections
            .get(&(fact.target.package_id.clone(), fact.component.clone()))
            .cloned()
            .ok_or_else(|| {
                PublishManifestError::new(
                    PublishManifestErrorType::Component,
                    format!(
                        "component {:?} of package {:?} has no descriptor",
                        fact.component, fact.target.package_id
                    ),
                )
            })
    }
}

/// Bind release behavior and ownership to its verified package manifests.
fn validate_release_package_manifests(
    request: &PublishReleaseManifest<'_>,
    package_manifests: &BTreeMap<wamn_package::PackageName, wamn_component_policy::PackageManifest>,
) -> Result<(), PublishManifestError> {
    let expected_package_ids = request
        .packages
        .iter()
        .map(PackageCoordinate::package_id)
        .collect::<BTreeSet<_>>();
    let presented_package_ids = package_manifests.keys().collect::<BTreeSet<_>>();
    if presented_package_ids != expected_package_ids {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::PackageManifest,
            format!(
                "release package manifests must exactly match membership; expected={expected_package_ids:?}, presented={presented_package_ids:?}; supply one exact wamn.json per release package"
            ),
        ));
    }
    for package in request.packages {
        let coordinate = format!("{}@{}", package.package_id(), package.package_version());
        let manifest = package_manifests
            .get(package.package_id())
            .ok_or_else(|| {
                PublishManifestError::new(
                    PublishManifestErrorType::PackageManifest,
                    format!(
                        "package {coordinate} has no presented manifest; supply the exact wamn.json from the verified artifact"
                    ),
                )
            })?;
        if &manifest.package.id != package.package_id()
            || &manifest.package.version != package.package_version()
        {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::PackageManifest,
                format!(
                    "package {coordinate} is paired with manifest {}@{}; supply the exact wamn.json from the verified artifact",
                    manifest.package.id, manifest.package.version
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::plan::application_runtime_schema;
    use super::*;
    use crate::test_support::closure_component;
    use wamn_test_infrastructure::publish::source_manifest;

    #[test]
    fn one_application_release_has_one_digest_across_environments() {
        let manifest = source_manifest();
        let packages = BTreeSet::from([PackageCoordinate::new(
            manifest.package.id.clone(),
            manifest.package.version.clone(),
        )
        .unwrap()]);
        let manifests: BTreeMap<wamn_package::PackageName, wamn_component_policy::PackageManifest> =
            BTreeMap::from([(manifest.package.id.clone(), manifest)]);
        let publish = |environment| {
            let request = PublishReleaseManifest {
                org: &"acme".parse().unwrap(),
                project: &"apps".parse().unwrap(),
                environment,
                packages: &packages,
                attachments: &BTreeMap::new(),
                environment_is_disposable: false,
            };
            let mut component = closure_component("source_fixture", None);
            component.target.package_id = "source_fixture".parse().unwrap();
            component.target.project_env_key = request.project_env_key();
            let mut components = ReleaseComponents::default();
            components.insert(component, Vec::new()).unwrap();
            publish_release_with_package_manifests(
                &request,
                &components,
                &manifests,
                &BTreeMap::new(),
            )
            .unwrap()
        };
        let staging_env = "staging".parse().unwrap();
        let production_env = "production".parse().unwrap();
        let staging = publish(&staging_env);
        let production = publish(&production_env);
        assert_eq!(staging.digest, production.digest);
    }

    fn refresh_definition_hash(attachment: &mut ServingAttachment) {
        attachment.definition_hash = wamn_digest::canonical_json_sha256(&attachment.definition);
    }

    /// The contract of a route that reads nothing and names no revision or key.
    fn route_contract(type_: OperationType) -> RouteContract {
        RouteContract {
            type_,
            reads: BTreeSet::new(),
            revision: None,
            idempotency: None,
        }
    }

    fn fixture_route_contracts(
        attachments: &BTreeMap<wamn_package::canonical_name::CanonicalName, ServingAttachment>,
    ) -> RouteContracts {
        attachments
            .values()
            .map(|attachment| {
                let AttachmentTarget::Route { operation, .. } = &attachment.target;
                (
                    (attachment.package_id.clone(), operation.clone()),
                    route_contract(OperationType::Command),
                )
            })
            .collect()
    }

    fn package_http_attachment(
        attachment_id: &str,
        package_id: &wamn_package::PackageName,
        component: &str,
        operation: &str,
        path: &str,
    ) -> ServingAttachment {
        let definition = serde_json::json!({
            "id": attachment_id,
            "type": "http",
            "route": {"path": path},
        });
        let definition_hash = wamn_digest::canonical_json_sha256(&definition);
        ServingAttachment {
            type_: wamn_package::AttachmentType::Http,
            package_id: package_id.to_owned(),
            target: wamn_package::AttachmentTarget::Route {
                component: component.parse().unwrap(),
                operation: operation.parse().unwrap(),
            },
            definition_hash,
            definition,
            auth_policy: serde_json::json!({"modes": ["pat"]}),
            registered_operation: Some(operation.parse().unwrap()),
        }
    }

    #[test]
    fn package_attachment_documents_merge_into_one_release() {
        let base = BTreeMap::from([
            (
                "first".parse().unwrap(),
                package_http_attachment(
                    "first",
                    &"source_fixture".parse().unwrap(),
                    "read",
                    "source-fixture:item/get@1.0.0",
                    "/items/get",
                ),
            ),
            (
                "second".parse().unwrap(),
                package_http_attachment(
                    "second",
                    &"source_fixture".parse().unwrap(),
                    "list",
                    "source-fixture:item/list@1.0.0",
                    "/items/list",
                ),
            ),
        ]);
        let overlay = BTreeMap::from([(
            "third".parse().unwrap(),
            package_http_attachment(
                "third",
                &"observer_fixture".parse().unwrap(),
                "read",
                "observer-fixture:item/get@3.0.0",
                "/observed/get",
            ),
        )]);
        let authored = merge_package_attachment_documents(vec![
            (PathBuf::from("source/attachments.json"), base),
            (PathBuf::from("observer/attachments.json"), overlay),
        ])
        .expect("distinct package attachment identities merge");
        assert_eq!(authored.len(), 3);
        assert_eq!(
            authored
                .values()
                .filter(|attachment| attachment.package_id == "source_fixture".parse().unwrap())
                .count(),
            2
        );
        assert_eq!(
            authored
                .values()
                .filter(|attachment| attachment.package_id == "observer_fixture".parse().unwrap())
                .count(),
            1
        );
        let resolved = resolve_route_methods(&authored, &fixture_route_contracts(&authored))
            .expect("merged routes resolve");
        assert!(
            resolved
                .values()
                .all(|attachment| attachment.definition["route"].get("host").is_none()),
            "a release names no route host"
        );
    }

    #[test]
    fn package_attachment_documents_refuse_duplicate_identity() {
        let base = package_http_attachment(
            "fixture-http",
            &"source_fixture".parse().unwrap(),
            "fixture_widget_update",
            "source-fixture:widget/update@1.0.0",
            "/widget/update",
        );
        let overlay = package_http_attachment(
            "fixture-http",
            &"observer_fixture".parse().unwrap(),
            "fixture_widget_update",
            "observer-fixture:widget/update@3.0.0",
            "/overlay/widget/update",
        );
        let error = merge_package_attachment_documents(vec![
            (
                PathBuf::from("apps/source_fixture/publication/attachments.json"),
                BTreeMap::from([("fixture-http".parse().unwrap(), base)]),
            ),
            (
                PathBuf::from("apps/observer_fixture/publication/attachments.json"),
                BTreeMap::from([("fixture-http".parse().unwrap(), overlay)]),
            ),
        ])
        .expect_err("one attachment identity cannot have two package owners");

        assert_eq!(
            error.error_type(),
            PublishManifestErrorType::DuplicateAttachmentId
        );
    }

    #[test]
    fn package_attachment_route_collisions_use_the_existing_governor() {
        let first = package_http_attachment(
            "first-http",
            &"source_fixture".parse().unwrap(),
            "widget_get",
            "source-fixture:widget/get@1.0.0",
            "/widget/{id}",
        );
        let second = package_http_attachment(
            "second-http",
            &"observer_fixture".parse().unwrap(),
            "quality_load_widget_detail",
            "observer-fixture:quality/load-widget-detail@3.0.0",
            "/widget/{widget_id}",
        );
        let authored = merge_package_attachment_documents(vec![
            (
                PathBuf::from("apps/source_fixture/publication/attachments.json"),
                BTreeMap::from([("first-http".parse().unwrap(), first)]),
            ),
            (
                PathBuf::from("apps/observer_fixture/publication/attachments.json"),
                BTreeMap::from([("second-http".parse().unwrap(), second)]),
            ),
        ])
        .expect("distinct attachment identities merge before route validation");
        let error = resolve_route_methods(&authored, &fixture_route_contracts(&authored))
            .expect_err("canonical route collisions remain refused after package merging");

        assert_eq!(error.error_type(), PublishManifestErrorType::Document);
    }

    #[test]
    fn release_publish_writes_no_route_host_and_refuses_an_authored_one() {
        let definition = serde_json::json!({
            "id": "fixture-http",
            "type": "http",
            "route": {"path": "/widget/get"},
        });
        let authored_hash = wamn_digest::canonical_json_sha256(&definition);
        let attachment = ServingAttachment {
            type_: wamn_package::AttachmentType::Http,
            package_id: "source_fixture".parse().unwrap(),
            target: wamn_package::AttachmentTarget::Route {
                component: "widget".parse().unwrap(),
                operation: "source-fixture:widget/get@1.0.0".parse().unwrap(),
            },
            definition_hash: authored_hash,
            definition,
            auth_policy: serde_json::json!({"modes": ["pat"]}),
            registered_operation: Some("source-fixture:widget/get@1.0.0".parse().unwrap()),
        };
        let authored = BTreeMap::from([("fixture-http".parse().unwrap(), attachment)]);

        let resolved = resolve_route_methods(&authored, &fixture_route_contracts(&authored))
            .expect("a routed release needs no route host");
        assert!(
            resolved["fixture-http"].definition["route"]
                .get("host")
                .is_none(),
            "a release names no route host"
        );
        assert_ne!(resolved["fixture-http"].definition_hash, authored_hash);
        assert_eq!(
            resolved["fixture-http"].definition_hash,
            wamn_digest::canonical_json_sha256(&resolved["fixture-http"].definition)
        );

        let mut package_authored = authored.clone();
        package_authored
            .get_mut("fixture-http")
            .expect("the attachment exists")
            .definition["route"]["host"] = serde_json::json!("package.example");
        refresh_definition_hash(
            package_authored
                .get_mut("fixture-http")
                .expect("the attachment exists"),
        );
        let package_host =
            resolve_route_methods(&package_authored, &fixture_route_contracts(&authored))
                .expect_err("package content cannot author a hostname");
        assert_eq!(
            package_host.error_type(),
            PublishManifestErrorType::Document
        );

        let mut non_routed = package_authored;
        non_routed
            .get_mut("fixture-http")
            .expect("the attachment exists")
            .type_ = wamn_package::AttachmentType::Internal;
        let package_host = resolve_route_methods(&non_routed, &fixture_route_contracts(&authored))
            .expect_err("every attachment type refuses an authored route hostname");
        assert_eq!(
            package_host.error_type(),
            PublishManifestErrorType::Document
        );

        let mut extra_route_field = authored.clone();
        extra_route_field
            .get_mut("fixture-http")
            .expect("the attachment exists")
            .definition["route"]["port"] = serde_json::json!(443);
        refresh_definition_hash(
            extra_route_field
                .get_mut("fixture-http")
                .expect("the attachment exists"),
        );
        let extra = resolve_route_methods(&extra_route_field, &fixture_route_contracts(&authored))
            .expect_err("package route schema admits only a path");
        assert_eq!(extra.error_type(), PublishManifestErrorType::Document);

        let mut colliding = authored;
        let first = colliding
            .get_mut("fixture-http")
            .expect("the attachment exists");
        first.definition["route"]["path"] = serde_json::json!("/widget/{id}");
        refresh_definition_hash(first);
        let mut second = first.clone();
        second.definition["id"] = serde_json::json!("fixture-http-alias");
        second.definition["route"]["path"] = serde_json::json!("/widget/{widget_id}");
        refresh_definition_hash(&mut second);
        colliding.insert("fixture-http-alias".parse().unwrap(), second);
        let collision = resolve_route_methods(&colliding, &fixture_route_contracts(&colliding))
            .expect_err("one release cannot carry ambiguous route templates");
        assert_eq!(collision.error_type(), PublishManifestErrorType::Document);
    }

    #[test]
    fn release_publish_writes_the_method_from_the_operation_type() {
        let route = |id: &str, operation: &str, path: &str| {
            let mut attachment = package_http_attachment(
                id,
                &"source_fixture".parse().unwrap(),
                id,
                operation,
                path,
            );
            attachment.target = wamn_package::AttachmentTarget::Route {
                component: "widget".parse().unwrap(),
                operation: operation.parse().unwrap(),
            };
            (id.parse().unwrap(), attachment)
        };
        let authored = BTreeMap::from([
            route("get-http", "source-fixture:widget/get@1.0.0", "/widget/get"),
            route(
                "query-http",
                "source-fixture:widget/query@1.0.0",
                "/widget/query",
            ),
            route(
                "load-http",
                "source-fixture:widget/load@1.0.0",
                "/widget/load",
            ),
            route(
                "update-http",
                "source-fixture:widget/update@1.0.0",
                "/widget/update",
            ),
            route(
                "record-http",
                "source-fixture:widget/record@1.0.0",
                "/widget/record",
            ),
            (
                "graph-http".parse().unwrap(),
                package_http_attachment(
                    "graph-http",
                    &"source_fixture".parse().unwrap(),
                    "widget_flow",
                    "source-fixture:widget/flow@1.0.0",
                    "/widget/flow",
                ),
            ),
        ]);
        let types: RouteContracts = [
            ("widget/get", OperationType::Get),
            ("widget/query", OperationType::Query),
            ("widget/load", OperationType::Projection),
            ("widget/update", OperationType::Update),
            ("widget/record", OperationType::Command),
            ("widget/flow", OperationType::Command),
        ]
        .into_iter()
        .map(|(operation, type_)| {
            (
                (
                    "source_fixture".parse().unwrap(),
                    format!("source-fixture:{operation}@1.0.0").parse().unwrap(),
                ),
                route_contract(type_),
            )
        })
        .collect();
        let resolved =
            resolve_route_methods(&authored, &types).expect("every route resolves a method");
        for (id, method) in [
            ("get-http", hyper::Method::GET),
            ("query-http", hyper::Method::GET),
            ("load-http", hyper::Method::GET),
            ("update-http", hyper::Method::POST),
            ("record-http", hyper::Method::POST),
            ("graph-http", hyper::Method::POST),
        ] {
            assert_eq!(
                resolved[id].definition["route"]["method"]
                    .as_str()
                    .unwrap()
                    .parse::<hyper::Method>()
                    .unwrap(),
                method,
                "{id}"
            );
        }

        let unknown = resolve_route_methods(&authored, &BTreeMap::new())
            .expect_err("a route to an operation with no contract type refuses");
        assert_eq!(
            unknown.error_type(),
            PublishManifestErrorType::PackageManifest
        );

        let mut authored_method = authored;
        let get = authored_method
            .get_mut("get-http")
            .expect("the attachment exists");
        get.definition["route"]["method"] = serde_json::json!("GET");
        refresh_definition_hash(get);
        let refusal = resolve_route_methods(&authored_method, &types)
            .expect_err("an authored method refuses");
        assert_eq!(refusal.error_type(), PublishManifestErrorType::Document);
    }


    /// A package directory whose `descriptors/` holds the canonical descriptor
    /// of `fact` with one connection, as an unpacked artifact holds it.
    fn dependency_manifest(digest: &wamn_digest::Digest) -> wamn_component_policy::PackageManifest {
        let mut document: serde_json::Value =
            serde_json::from_str(wamn_test_infrastructure::publish::SOURCE_MANIFEST)
                .expect("the repository package manifest parses as JSON");
        document["base_dependencies"] = serde_json::json!({
            "base": {
                "package": "base",
                "version": "1.0.0",
                "digest": digest,
                "operations": ["stock.record_widget"]
            }
        });
        serde_json::from_value(document).expect("the dependency fixture manifest parses")
    }

    #[test]
    fn runtime_schema_follows_relations_instead_of_the_application_name() {
        let mut manifest = source_manifest();
        manifest.custom_operations.clear();
        manifest.internal_relations.clear();
        for model in manifest.models.values_mut() {
            model.schema = "own_schema".parse().unwrap();
        }
        application_runtime_schema(&manifest, &BTreeMap::new(), &BTreeSet::new()).unwrap();
        for model in manifest.models.values_mut() {
            model.schema = "base_schema".parse().unwrap();
            model.owner = "base_package".parse().unwrap();
        }
        application_runtime_schema(&manifest, &BTreeMap::new(), &BTreeSet::new()).unwrap();
        let mut extra = manifest.models.values().next().unwrap().clone();
        extra.schema = "other_schema".parse().unwrap();
        manifest.models.insert("other".parse().unwrap(), extra);
        assert!(application_runtime_schema(&manifest, &BTreeMap::new(), &BTreeSet::new()).is_err());
    }

    /// The rule is selected by the projected environment marker and by nothing
    /// else. wamn-10yt.38 writes that marker; this pins the mapping so an
    /// operator switch cannot be added without failing here.
    #[test]
    fn the_projected_environment_marker_selects_the_dependency_rule() {
        assert_eq!(
            DependencyDigestRule::for_environment(false),
            DependencyDigestRule::Declared
        );
        assert_eq!(
            DependencyDigestRule::for_environment(true),
            DependencyDigestRule::Built
        );
        assert!(DependencyDigestRule::Declared.matches_declared_digest());
        assert!(!DependencyDigestRule::Built.matches_declared_digest());
    }

    /// The manifest of package `id`@`version` with one base pinned at
    /// `digest`.
    fn pinning_manifest(
        id: &wamn_package::PackageName,
        version: &str,
        base: (&wamn_package::PackageName, &str),
        digest: &wamn_digest::Digest,
    ) -> wamn_component_policy::PackageManifest {
        let mut document = serde_json::to_value(dependency_manifest(digest))
            .expect("the dependency fixture manifest serializes");
        document["package"] = serde_json::json!({"id": id, "version": version});
        document["base_dependencies"]["base"]["package"] = serde_json::json!(base.0);
        document["base_dependencies"]["base"]["version"] = serde_json::json!(base.1);
        serde_json::from_value(document).expect("the pinning manifest parses")
    }

    #[test]
    fn base_dependencies_expand_the_exact_release_closure_and_refuse_cycles() {
        let base_operation: wamn_package::registered_operation::RegisteredOperation =
            "base:stock/record-widget@1.0.0".parse().unwrap();
        let overlay_operation: wamn_package::registered_operation::RegisteredOperation =
            "overlay:stock/record-widget@3.0.0".parse().unwrap();
        let mut base = closure_component("base-component", Some(&base_operation));
        base.target.package_id = "base".parse().unwrap();
        let mut overlay = closure_component("overlay-component", Some(&overlay_operation));
        overlay.target.package_id = "overlay".parse().unwrap();
        overlay.target.package_version = "3.0.0".parse().unwrap();
        overlay.component_digest = (format!("sha256:{}", "b".repeat(64)))
            .parse::<wamn_digest::Digest>()
            .unwrap();
        let roots = BTreeMap::from([("entry".parse().unwrap(), overlay.clone())]);
        let facts: BTreeMap<
            (
                wamn_package::PackageName,
                wamn_package::package_version::PackageVersion,
            ),
            Vec<AdmittedComponent>,
        > = BTreeMap::from([
            (
                ("base".parse().unwrap(), "1.0.0".parse().unwrap()),
                vec![base.clone()],
            ),
            (
                ("overlay".parse().unwrap(), "3.0.0".parse().unwrap()),
                vec![overlay.clone()],
            ),
        ]);
        let manifests: BTreeMap<wamn_package::PackageName, wamn_component_policy::PackageManifest> =
            BTreeMap::from([(
                "overlay".parse().unwrap(),
                pinning_manifest(
                    &"overlay".parse().unwrap(),
                    "3.0.0",
                    (&"base".parse().unwrap(), "1.0.0"),
                    &base.component_digest,
                ),
            )]);

        let closure = resolve_component_dependency_closure(
            &roots,
            &facts,
            &manifests,
            DependencyDigestRule::Declared,
        )
        .expect("the pinned base expands the release component closure");
        assert_eq!(closure.len(), 2);
        assert!(closure.contains(&base));
        assert!(closure.contains(&overlay));
        let projected = ServingComponent::project(&overlay, Vec::new());
        assert_eq!(
            projected.descriptor.operations[overlay_operation.as_str()]
                .registered_operation
                .as_ref(),
            Some(&overlay_operation),
            "the overlay entry requires its own permission alone"
        );

        let mut cyclic = manifests;
        cyclic.insert(
            "base".parse().unwrap(),
            pinning_manifest(
                &"base".parse().unwrap(),
                "1.0.0",
                (&"overlay".parse().unwrap(), "3.0.0"),
                &overlay.component_digest,
            ),
        );
        let error = resolve_component_dependency_closure(
            &roots,
            &facts,
            &cyclic,
            DependencyDigestRule::Declared,
        )
        .expect_err("a base dependency cycle was accepted");
        assert_eq!(
            error.error_type(),
            PublishManifestErrorType::OperationDependency
        );
    }

    /// wamn-10yt.48 at the closure: an author edits a base package, the built
    /// digest moves, and the overlay's authored pin still names the old bytes.
    /// A durable release refuses that; a disposable target built both halves
    /// from this same tree, so it resolves the base by coordinate.
    #[test]
    fn a_disposable_target_expands_a_closure_whose_pinned_base_digest_moved() {
        let base_operation: wamn_package::registered_operation::RegisteredOperation =
            "base:stock/record-widget@1.0.0".parse().unwrap();
        let overlay_operation: wamn_package::registered_operation::RegisteredOperation =
            "overlay:stock/record-widget@3.0.0".parse().unwrap();
        let mut base = closure_component("base-component", Some(&base_operation));
        base.target.package_id = "base".parse().unwrap();
        let mut overlay = closure_component("overlay-component", Some(&overlay_operation));
        overlay.target.package_id = "overlay".parse().unwrap();
        overlay.target.package_version = "3.0.0".parse().unwrap();
        overlay.component_digest = (format!("sha256:{}", "b".repeat(64)))
            .parse::<wamn_digest::Digest>()
            .unwrap();
        let stale_pin = (format!("sha256:{}", "d".repeat(64)))
            .parse::<wamn_digest::Digest>()
            .unwrap();
        assert_ne!(stale_pin, base.component_digest);
        let roots = BTreeMap::from([("entry".parse().unwrap(), overlay.clone())]);
        let facts: BTreeMap<
            (
                wamn_package::PackageName,
                wamn_package::package_version::PackageVersion,
            ),
            Vec<AdmittedComponent>,
        > = BTreeMap::from([
            (
                ("base".parse().unwrap(), "1.0.0".parse().unwrap()),
                vec![base.clone()],
            ),
            (
                ("overlay".parse().unwrap(), "3.0.0".parse().unwrap()),
                vec![overlay.clone()],
            ),
        ]);
        let manifests: BTreeMap<wamn_package::PackageName, wamn_component_policy::PackageManifest> =
            BTreeMap::from([(
                "overlay".parse().unwrap(),
                pinning_manifest(
                    &"overlay".parse().unwrap(),
                    "3.0.0",
                    (&"base".parse().unwrap(), "1.0.0"),
                    &stale_pin,
                ),
            )]);

        let refusal = resolve_component_dependency_closure(
            &roots,
            &facts,
            &manifests,
            DependencyDigestRule::Declared,
        )
        .expect_err("a durable release still demands the pinned bytes");
        assert_eq!(
            refusal.error_type(),
            PublishManifestErrorType::OperationDependency
        );

        let closure = resolve_component_dependency_closure(
            &roots,
            &facts,
            &manifests,
            DependencyDigestRule::Built,
        )
        .expect("a disposable target expands the closure it just built");
        assert_eq!(closure.len(), 2);
        assert!(closure.contains(&base));
        assert!(closure.contains(&overlay));
    }
}

#[cfg(test)]
mod effective_release_live;

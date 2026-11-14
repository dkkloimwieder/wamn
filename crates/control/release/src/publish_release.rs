//! Publish one immutable release.
//!
//! A release is its manifest digest
//! (docs/architecture/overview.md#release-identity). The publisher composes
//! package artifacts (§8.1): it pulls the artifact under the tag of each exact
//! package pair from the registry only, and refuses a package with no artifact
//! before it writes anything (R11 (2)). It resolves every component from the
//! admitted descriptors of those artifacts, and each release component carries
//! its descriptor. It pushes the canonical manifest bytes to OCI and writes no
//! Postgres row: `apply` step 2 writes `catalog.releases` where it installs (R1
//! (3), R2, §10.2).

use sqlx::PgConnection as Client;
use sqlx::Row as _;
pub(super) mod plan;
pub use plan::{
    application_dependency_permissions, composes_integration, parse_package, record_composed_entry,
};
use plan::{
    application_digests, covers_routes, pin_runtime_schemas, project_components, project_routes,
    validate_request,
};

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context as _, ensure};

use wamn_catalog::exposure::{
    HttpRoute as AuthoredHttpRoute, canonical_http_route_template, normalize_http_route,
};
use wamn_catalog::{
    AdmittedComponent, AttachmentTarget, ComponentConnection, ComponentPackageCoordinate,
    ManifestDigest, OperationType, PackageCoordinate, SERVING_MANIFEST_FORMAT_VERSION,
    ServingAttachment, ServingComponent, ServingManifest, ServingRelation, ServingRelease,
    ServingRoute,
};

use wamn_package_control::package_artifact::{OpenedPackage, PackageRegistry};

mod attachments;
mod components;
mod entry_publication;
mod package_sources;

use attachments::{read_package_attachments, resolve_route_methods};
use components::{
    project_serving_component, resolve_component_dependency_closure, resolve_route_component,
};
pub use entry_publication::EntryPublicationInputs;
pub use package_sources::package_route;
use package_sources::read_package_manifests;

pub use manifest_push::push_manifest_bytes;

#[cfg(test)]
use attachments::merge_package_attachment_documents;

const ASSIGN_PROJECT_ENV_KEY_SQL: &str = "SELECT set_config('app.project_env_key', $1, false)";
const READ_RELEASE_SQL: &str = "\
SELECT canonical_bytes FROM catalog.releases \
 WHERE project_env_key = $1 AND manifest_digest = $2";

/// Inputs owned by the release publisher.
#[derive(Debug)]
pub struct PublishReleaseManifest<'a> {
    pub org: &'a str,
    pub project: &'a str,
    pub environment: &'a str,
    pub packages: &'a BTreeSet<PackageCoordinate>,
    pub attachments: &'a BTreeMap<String, ServingAttachment>,
    /// Local development can use rebuilt dependencies after target validation.
    /// Published construction always requires the declared dependency digests.
    pub environment_is_disposable: bool,
}

impl PublishReleaseManifest<'_> {
    /// The composed environment key derived from the environment coordinate.
    pub fn project_env_key(&self) -> String {
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
    pub digest: ManifestDigest,
    pub canonical_bytes: Vec<u8>,
}

pub const RELEASE_MANIFEST_MINT_REFUSAL: &str = "release-manifest-mint-refused";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishManifestErrorType {
    Storage,
    Release,
    PackageManifest,
    Component,
    OperationDependency,
    UnauthenticatedRegisteredOperation,
    UnauthenticatedWrite,
    ClosureConflict,
    Document,
    DuplicateAttachmentId,
    ApplicationSelection,
    ApplicationCoverage,
    AmbiguousApplicationMember,
}

impl PublishManifestErrorType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApplicationSelection => "application-selection",
            Self::ApplicationCoverage => "application-coverage",
            Self::AmbiguousApplicationMember => "ambiguous-application-member",
            Self::Storage => "storage",
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
pub type RouteContracts = BTreeMap<(String, String), RouteContract>;

/// What a route takes from the generated contract `operation.json` of its
/// operation: the type, the relations a read reads, and the revision field of
/// a `get`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteContract {
    type_: OperationType,
    reads: BTreeSet<ServingRelation>,
    revision: Option<String>,
    idempotency: Option<String>,
}

#[cfg(test)]
impl From<OperationType> for RouteContract {
    fn from(type_: OperationType) -> Self {
        Self {
            type_,
            reads: BTreeSet::new(),
            revision: None,
            idempotency: None,
        }
    }
}

/// Inputs of one application release publication.
#[derive(Debug)]
pub struct PublishReleaseRequest {
    /// Application membership from environment.k.
    pub applications: wamn_catalog::EnvironmentApplications,
    /// Selects one declared application; omitted for a one-application release.
    pub application: Option<String>,
    /// Organization in the declared environment coordinate.
    pub org: String,
    pub project: String,
    /// Environment the release is published for.
    pub environment: String,
    /// Available exact coordinates for the declared application packages.
    pub packages: Vec<PackageCoordinate>,
}

impl PublishReleaseRequest {
    /// The composed environment key derived from the environment coordinate.
    pub fn project_env_key(&self) -> String {
        wamn_registry::project_env_key(&self.org, &self.project, &self.environment)
    }
}

/// Provisioned target required only by local development.
#[derive(Debug)]
pub struct LocalReleaseTarget<'a> {
    pub database_url: &'a str,
    pub control_database_url: &'a str,
    pub run_schema: &'a str,
}

/// Publish one effective release from the package artifacts in
/// `registry`: push its manifest to `release_artifact_base` with the
/// credential and trust roots of `registry` (§8.1), and return its manifest
/// digest. A release with an HTTP entry needs `entry` (§8.1). It writes OCI
/// only.
pub async fn publish_release(
    request: PublishReleaseRequest,
    registry: &PackageRegistry,
    release_artifact_base: &str,
    entry: Option<&EntryPublicationInputs<'_>>,
) -> anyhow::Result<ManifestDigest> {
    Ok(
        publish_candidate(&request, registry, release_artifact_base, entry)
            .await?
            .digest,
    )
}

/// Assemble a candidate only in a provisioned disposable target, without publication.
///
/// The packages are the directories `package_roots`, and the components their
/// in-process admissions: the dev loop pushes nothing and reads no registry.
pub async fn publish_local(
    args: PublishReleaseRequest,
    target: LocalReleaseTarget<'_>,
    package_roots: &[PathBuf],
    admissions: &[wamn_package_control::component_admission::ComponentAdmission],
    palette: &[crate::palette::PaletteComponent],
) -> anyhow::Result<(
    PublishedRelease,
    wamn_runtime::local_application::LocalApplicationFacts,
)> {
    wamn_runtime::local_application::require_local_target(
        target.database_url,
        &args.project_env_key(),
        &args.environment,
    )
    .await?;
    ensure!(
        read_environment_disposable(target.control_database_url, &args.org, &args.environment)
            .await?,
        "local candidates require a provisioned disposable environment"
    );
    let mut client = wamn_postgres::connect(target.database_url).await?;

    sqlx::query(ASSIGN_PROJECT_ENV_KEY_SQL)
        .bind(args.project_env_key())
        .fetch_one(&mut client)
        .await?;
    let assembled = assemble_local_release(&args, package_roots, admissions, palette)?;
    drop(client);
    Ok((assembled.published, assembled.facts))
}

/// Built application bytes with the platform route layer, not yet selected for serving.
#[derive(Debug)]
pub struct ComposedApplication {
    bytes: Box<[u8]>,
    digest: String,
    route_digest: String,
    application: wamn_catalog::ServingEntryMember,
    application_digests: BTreeSet<String>,
}

impl ComposedApplication {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn application(&self) -> &wamn_catalog::ServingEntryMember {
        &self.application
    }
}

/// Build an HTTP application from an admitted member and pinned platform route bytes.
///
/// The member can already contain its admitted bases. The pin must identify
/// the supplied route layer exactly.
pub fn compose_application(
    application: &wamn_package_control::component_admission::ComponentAdmission,
    route_bytes: &[u8],
    route_digest: &str,
) -> anyhow::Result<ComposedApplication> {
    compose_verified_application(
        application.facts(),
        application.bytes(),
        route_bytes,
        route_digest,
    )
}

fn compose_verified_application(
    application: &AdmittedComponent,
    application_bytes: &[u8],
    route_bytes: &[u8],
    route_digest: &str,
) -> anyhow::Result<ComposedApplication> {
    ensure!(
        wamn_digest::bytes_sha256(route_bytes) == route_digest,
        "route layer bytes differ from pinned digest {route_digest}"
    );
    wamn_component_composer::validate_application_member(application_bytes)?;
    let bytes = route_bytes.to_vec();
    let digest = wamn_digest::bytes_sha256(&bytes);
    let application_digests = application_digests(application, application_bytes)?;
    Ok(ComposedApplication {
        bytes: bytes.into_boxed_slice(),
        digest,
        route_digest: route_digest.to_owned(),
        application: wamn_catalog::ServingEntryMember {
            package_id: application.target.package_id.as_str().to_owned(),
            component: application.component.as_str().to_owned(),
            digest: application.component_digest.as_str().to_owned(),
        },
        application_digests,
    })
}

/// Compose the routed application before its artifact reference is recorded.
pub fn compose_release_entry(
    published: &PublishedRelease,
    admissions: &[wamn_package_control::component_admission::ComponentAdmission],
    route_bytes: &[u8],
    applications: &wamn_catalog::EnvironmentApplications,
    application: Option<&str>,
) -> anyhow::Result<ComposedApplication> {
    compose_covering_entry(
        published,
        &admissions
            .iter()
            .map(|admission| (admission.facts(), admission.bytes()))
            .collect::<Vec<_>>(),
        route_bytes,
        applications,
        application,
    )
}

/// Compose the unique member of the declared application that covers its routes.
pub fn compose_covering_entry(
    published: &PublishedRelease,
    candidates: &[(&AdmittedComponent, &[u8])],
    route_bytes: &[u8],
    applications: &wamn_catalog::EnvironmentApplications,
    application: Option<&str>,
) -> anyhow::Result<ComposedApplication> {
    let (member, bytes) = covering_application(published, candidates, applications, application)?;
    compose_pinned_application(member, bytes, route_bytes)
}

/// Keep the composed bytes while the selected member and route digests match.
pub fn refresh_composed_entry(
    entry: &mut Option<ComposedApplication>,
    published: &PublishedRelease,
    candidates: &[(&AdmittedComponent, &[u8])],
    route_bytes: &[u8],
    applications: &wamn_catalog::EnvironmentApplications,
) -> anyhow::Result<bool> {
    let (member, bytes) = covering_application(published, candidates, applications, None)?;
    let route_digest = wamn_digest::bytes_sha256(route_bytes);
    if entry.as_ref().is_some_and(|entry| {
        entry.application.package_id == member.target.package_id
            && entry.application.component == member.component
            && entry.application.digest == member.component_digest
            && entry.route_digest == route_digest
    }) {
        return Ok(false);
    }
    *entry = Some(compose_pinned_application(member, bytes, route_bytes)?);
    Ok(true)
}

pub fn compose_pinned_application(
    member: &AdmittedComponent,
    bytes: &[u8],
    route_bytes: &[u8],
) -> anyhow::Result<ComposedApplication> {
    let pin = format!(
        "sha256:{}",
        include_str!("../../../../apps/platform/ingress/http-route/http_route.wasm.sha256")
            .split_whitespace()
            .next()
            .expect("route pin exists")
    );
    compose_verified_application(member, bytes, route_bytes, &pin)
}

fn select_application<'a>(
    applications: &'a wamn_catalog::EnvironmentApplications,
    application: Option<&str>,
) -> Result<(&'a str, &'a wamn_catalog::ApplicationDefinition), PublishManifestError> {
    let selected = match application {
        Some(name) => applications.get_key_value(name),
        None if applications.len() == 1 => applications.first_key_value(),
        None => None,
    };
    selected
        .map(|(name, definition)| (name.as_str(), definition))
        .ok_or_else(|| {
            PublishManifestError::new(
                PublishManifestErrorType::ApplicationSelection,
                format!(
                    "select one application from environment.k: {:?}; requested {application:?}",
                    applications.keys().collect::<Vec<_>>()
                ),
            )
        })
}

/// Select the declared application's unique member covering its routes.
fn covering_application<'a>(
    published: &PublishedRelease,
    candidates: &[(&'a AdmittedComponent, &'a [u8])],
    applications: &wamn_catalog::EnvironmentApplications,
    application: Option<&str>,
) -> anyhow::Result<(&'a AdmittedComponent, &'a [u8])> {
    let (name, definition) = select_application(applications, application)?;
    let mut covering = Vec::new();
    for (member, bytes) in candidates {
        if definition
            .packages
            .contains(member.target.package_id.as_str())
            && published.manifest.components.iter().any(|released| {
                released.package_id == member.target.package_id
                    && released.descriptor.component == member.component
                    && released.descriptor.component_digest == member.component_digest
            })
            && covers_routes(published, member, bytes)?
        {
            covering.push((*member, *bytes));
        }
    }
    match covering.as_slice() {
        [member] => Ok(*member),
        [] => Err(PublishManifestError::new(
            PublishManifestErrorType::ApplicationCoverage,
            format!("application {name:?} has no released member covering every route"),
        )
        .into()),
        members => Err(PublishManifestError::new(
            PublishManifestErrorType::AmbiguousApplicationMember,
            format!(
                "application {name:?} has multiple members covering every route: {}",
                members
                    .iter()
                    .map(|(member, _)| format!(
                        "{}::{}",
                        member.target.package_id, member.component
                    ))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
        .into()),
    }
}

/// A local release assembled from its package files and admitted components.
#[derive(Debug)]
pub struct AssembledLocalRelease {
    pub published: PublishedRelease,
    pub facts: wamn_runtime::local_application::LocalApplicationFacts,
}

/// Assemble the manifest and local facts of a local release from the package
/// directories `package_roots` and admitted components, as [`publish_local`]
/// does. Each component's descriptor comes from
/// its admission. It reads no database.
pub fn assemble_local_release(
    args: &PublishReleaseRequest,
    package_roots: &[PathBuf],
    admissions: &[wamn_package_control::component_admission::ComponentAdmission],
    palette: &[crate::palette::PaletteComponent],
) -> anyhow::Result<AssembledLocalRelease> {
    use wamn_runtime::local_application::LocalApplicationFacts;
    let (attachment_paths, _) = package_files(package_roots);
    let authored = read_package_attachments(&attachment_paths)?;
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
    validate_request(&request)?;
    ensure!(
        package_manifests.len() == packages.len()
            && packages.iter().all(|package| package_manifests
                .get(package.package_id())
                .is_some_and(|manifest| manifest.package.version == package.package_version())),
        "local candidate requires every exact package manifest"
    );
    let mut release_components = ReleaseComponents::default();
    for admission in admissions {
        let fact = admission.facts();
        ensure!(
            fact.target.project_env_key == args.project_env_key()
                && packages
                    .iter()
                    .any(|package| package.package_id() == fact.target.package_id
                        && package.package_version() == fact.target.package_version),
            "local admission is outside package membership"
        );
        release_components.insert(
            fact.clone(),
            admission.descriptor().connections,
            admission.statement_instances().clone(),
        )?;
    }
    let mut components = project_components(&release_components)?;
    let routes = project_routes(
        &request,
        &route_contracts,
        &release_components,
        &package_manifests,
        &mut components,
    )?;
    for component in palette {
        components.insert(ServingComponent::project(
            &component.facts.component,
            components::palette_connections(&component.facts.connections, &package_manifests),
        )?);
    }
    let route_attachments = ServingAttachment::routes(attachments.clone());
    let manifest = ServingManifest {
        composed_entry: None,
        format_version: SERVING_MANIFEST_FORMAT_VERSION,
        release: ServingRelease {
            packages: packages.clone(),
        },
        components: pin_runtime_schemas(components, &package_manifests)?,
        routes,
        attachments: route_attachments,
        // Every application release serves the fixed application administration
        // routes (docs/architecture/execution.md#host-run-routes).
        host_routes: BTreeSet::from([wamn_catalog::HostRouteSet::Application]),
    };
    let canonical_bytes = manifest.canonical_bytes();
    let (manifest, digest) = ServingManifest::from_canonical_bytes(&canonical_bytes)?;
    let mut components: Vec<AdmittedComponent> = admissions
        .iter()
        .map(wamn_package_control::component_admission::ComponentAdmission::facts)
        .filter(|fact| {
            manifest.components.iter().any(|component| {
                component.descriptor.component_digest.as_str() == fact.component_digest
            })
        })
        .cloned()
        .collect();
    components.extend(
        palette
            .iter()
            .map(|component| component.facts.component.clone()),
    );
    let requirements = admissions
        .iter()
        .flat_map(|admission| admission.requirements().iter())
        .filter(|requirement| {
            manifest.components.iter().any(|component| {
                component.descriptor.component_digest.as_str() == requirement.component_digest()
            })
        })
        .cloned()
        .collect();
    let facts = LocalApplicationFacts {
        manifest_digest: digest.clone(),
        components,
        requirements,
        bindings: Vec::new(),
    };
    Ok(AssembledLocalRelease {
        published: PublishedRelease {
            manifest,
            digest,
            canonical_bytes,
        },
        facts,
    })
}

async fn publish_candidate(
    args: &PublishReleaseRequest,
    registry: &PackageRegistry,
    release_artifact_base: &str,
    entry: Option<&EntryPublicationInputs<'_>>,
) -> anyhow::Result<PublishedRelease> {
    ensure!(
        !args.environment.is_empty(),
        "environment must not be empty"
    );
    let (_, application) = select_application(&args.applications, args.application.as_deref())?;
    let (coordinates, opened) =
        package_sources::open_application(registry, application, &args.packages).await?;
    let release_components = descriptor_components(&args.project_env_key(), &coordinates, &opened)?;
    let roots = opened
        .iter()
        .map(|package| package.root().to_owned())
        .collect::<Vec<_>>();
    let (attachment_paths, _) = package_files(&roots);
    let authored_attachments = read_package_attachments(&attachment_paths)?;
    let (package_manifests, route_contracts) = read_package_manifests(&roots)?;
    let attachments = resolve_route_methods(&authored_attachments, &route_contracts)?;
    let packages = coordinates.into_iter().collect::<BTreeSet<_>>();
    let request = PublishReleaseManifest {
        org: &args.org,
        project: &args.project,
        environment: &args.environment,
        packages: &packages,
        attachments: &attachments,
        environment_is_disposable: false,
    };
    let mut published = publish_release_with_package_manifests(
        &request,
        &release_components,
        &package_manifests,
        &route_contracts,
    )?;
    if let Some(inputs) = entry {
        let palette = crate::palette::read(inputs.route_guest, &args.project_env_key())?;
        let base = wamn_catalog::parse_component_artifact_base(inputs.component_artifact_base)?;
        let credentials =
            wamn_package_artifact::registry_credentials::read_registry_push_credentials(
                &registry.registry_auth_file,
                base.registry(),
            )?;
        for component in &palette {
            let fact = &component.facts.component;
            let reference: oci_client::Reference = format!(
                "{}:{}",
                inputs.component_artifact_base,
                wamn_catalog::component_digest_tag(&fact.component_digest)?
            )
            .parse()?;
            wamn_package_control::component_admission::publish_and_verify(
                &reference,
                inputs.component_artifact_base,
                registry.insecure_registry,
                &registry.oci_ca_paths,
                &component.bytes,
                &wamn_engine::component_artifact::component_artifact_config_bytes(fact),
                fact,
                credentials.as_ref(),
            )
            .await?;
            published
                .manifest
                .components
                .insert(ServingComponent::project(
                    fact,
                    components::palette_connections(
                        &component.facts.connections,
                        &package_manifests,
                    ),
                )?);
        }
        let bytes = published.manifest.canonical_bytes();
        let (manifest, digest) = ServingManifest::from_canonical_bytes(&bytes)?;
        published = PublishedRelease {
            manifest,
            digest,
            canonical_bytes: bytes,
        };
    }
    entry_publication::publish(
        &mut published,
        &release_components,
        registry,
        &args.applications,
        args.application.as_deref(),
        entry,
    )
    .await?;
    push_manifest_bytes(
        &published.canonical_bytes,
        release_artifact_base,
        registry.insecure_registry,
        &registry.oci_ca_paths,
        &registry.registry_auth_file,
    )
    .await?;
    Ok(published)
}

/// Read the environment's lifecycle setting from its registry row.
async fn read_environment_disposable(
    control_database_url: &str,
    org: &str,
    env: &str,
) -> anyhow::Result<bool> {
    on_control_plane(control_database_url, async |control| {
        Ok(sqlx::query(wamn_registry::sql::ENVIRONMENT_DISPOSABLE)
            .bind(org)
            .bind(env)
            .fetch_one(&mut *control)
            .await?
            .get(0))
    })
    .await
}

pub(crate) async fn on_control_plane<F, T>(
    control_database_url: &str,
    write: F,
) -> anyhow::Result<T>
where
    F: AsyncFnOnce(&mut Client) -> anyhow::Result<T>,
{
    let mut control = wamn_postgres::connect(control_database_url)
        .await
        .context("connect to the control database")?;

    let outcome = write(&mut control).await;
    drop(control);
    match outcome {
        Ok(outcome) => Ok(outcome),
        Err(error) => Err(error),
    }
}

pub fn publish_release_with_package_manifests(
    request: &PublishReleaseManifest<'_>,
    component_facts: &ReleaseComponents,
    package_manifests: &BTreeMap<String, wamn_compiler::manifest::PackageManifest>,
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
    let route_attachments = ServingAttachment::routes(request.attachments.clone());

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
        host_routes: BTreeSet::from([wamn_catalog::HostRouteSet::Application]),
    };
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
    facts: BTreeMap<(String, String), Vec<AdmittedComponent>>,
    connections: BTreeMap<(String, String), Vec<ComponentConnection>>,
    /// The verified statement instance record of each composed component, by
    /// component digest.
    statement_instances: BTreeMap<String, BTreeMap<String, String>>,
}

impl ReleaseComponents {
    /// Add one admitted component and its connections. A package names each
    /// component once.
    pub fn insert(
        &mut self,
        fact: AdmittedComponent,
        connections: Vec<ComponentConnection>,
        statement_instances: BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        if !statement_instances.is_empty() {
            self.statement_instances
                .insert(fact.component_digest.clone(), statement_instances);
        }
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

    /// `serving` with the statement set of each named `statements` import of
    /// its component: the overlay's from the component's own descriptor, and
    /// each base member's from that base's admitted descriptor in the release,
    /// found by the digest that the base dependency pins.
    fn with_statement_instances(
        &self,
        serving: ServingComponent,
    ) -> Result<ServingComponent, PublishManifestError> {
        let Some(instances) = self
            .statement_instances
            .get(&serving.descriptor.component_digest)
        else {
            return Ok(serving);
        };
        let component = serving.descriptor.component.clone();
        serving
            .with_statement_instances(instances, &|digest| {
                self.facts
                    .values()
                    .flatten()
                    .find(|fact| fact.component_digest == digest)
            })
            .map_err(|error| {
                PublishManifestError::with_source(
                    PublishManifestErrorType::Component,
                    format!("component {component:?} does not bind its statement instances"),
                    error,
                )
            })
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

/// The attachment documents and the `wamn.json` of the packages at `roots`. A
/// package with no `publication/attachments.json` authors no attachments, and
/// its generated attachments are read beside the missing document.
fn package_files(roots: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let attachments = roots
        .iter()
        .map(|root| root.join("publication/attachments.json"))
        .collect();
    let manifests = roots
        .iter()
        .map(|root| wamn_compiler::package_manifest_path(root))
        .collect();
    (attachments, manifests)
}

/// Read the canonical bytes of an installed release.
pub async fn read_release(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &str,
    manifest_digest: &str,
) -> Result<Option<Vec<u8>>, PublishManifestError> {
    sqlx::query(ASSIGN_PROJECT_ENV_KEY_SQL)
        .bind(project_env_key)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|error| storage("bind the release project identity", error))?;
    sqlx::query(READ_RELEASE_SQL)
        .bind(project_env_key)
        .bind(manifest_digest)
        .fetch_optional(&mut **transaction)
        .await
        .map(|row| row.map(|row| row.get(0)))
        .map_err(|error| storage("read the release", error))
}

fn storage(context: &'static str, error: sqlx::Error) -> PublishManifestError {
    PublishManifestError::with_source(PublishManifestErrorType::Storage, context, error)
}

fn descriptor_components(
    project_env_key: &str,
    packages: &[PackageCoordinate],
    opened: &[OpenedPackage],
) -> anyhow::Result<ReleaseComponents> {
    let mut components = ReleaseComponents::default();
    for (package, opened) in packages.iter().zip(opened) {
        let target = ComponentPackageCoordinate::new(
            project_env_key,
            package.package_id(),
            package.package_version(),
        )
        .context("name the release component target")?;
        for descriptor in wamn_package_control::package_artifact::read_descriptors(opened.root())? {
            let facts = descriptor.into_admitted(target.clone());
            wamn_catalog::verify_stored_effect_projection(&facts.component).with_context(|| {
                format!(
                    "component {:?} of package {}@{} carries an invalid effect projection",
                    facts.component.component,
                    package.package_id(),
                    package.package_version()
                )
            })?;
            wamn_package_control::component_admission::require_admitted_statements(
                &facts.component,
            )
            .with_context(|| {
                format!(
                    "component {:?} of package {}@{} carries refused SQL",
                    facts.component.component,
                    package.package_id(),
                    package.package_version()
                )
            })?;
            components.insert(
                facts.component,
                facts.connections,
                facts.statement_instances,
            )?;
        }
    }
    Ok(components)
}

/// Bind release behavior and ownership to its verified package manifests.
fn validate_release_package_manifests(
    request: &PublishReleaseManifest<'_>,
    package_manifests: &BTreeMap<String, wamn_compiler::manifest::PackageManifest>,
) -> Result<(), PublishManifestError> {
    let expected_package_ids = request
        .packages
        .iter()
        .map(PackageCoordinate::package_id)
        .collect::<BTreeSet<_>>();
    let presented_package_ids = package_manifests
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
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
        if manifest.package.id != package.package_id()
            || manifest.package.version != package.package_version()
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
    use wamn_package_control::test_support::{closure_component, source_manifest};

    #[test]
    fn one_application_release_has_one_digest_across_environments() {
        let manifest = source_manifest();
        let packages = BTreeSet::from([PackageCoordinate::new(
            &manifest.package.id,
            &manifest.package.version,
        )
        .unwrap()]);
        let manifests = BTreeMap::from([(manifest.package.id.clone(), manifest)]);
        let publish = |environment| {
            let request = PublishReleaseManifest {
                org: "acme",
                project: "apps",
                environment,
                packages: &packages,
                attachments: &BTreeMap::new(),
                environment_is_disposable: false,
            };
            let mut component = closure_component("source_fixture", None);
            component.target.package_id = "source_fixture".to_owned();
            component.target.project_env_key = request.project_env_key();
            let mut components = ReleaseComponents::default();
            components
                .insert(component, Vec::new(), BTreeMap::new())
                .unwrap();
            publish_release_with_package_manifests(
                &request,
                &components,
                &manifests,
                &BTreeMap::new(),
            )
            .unwrap()
        };
        let staging = publish("staging");
        let production = publish("production");
        assert_eq!(staging.canonical_bytes, production.canonical_bytes);
        assert_eq!(staging.digest, production.digest);
    }

    fn refresh_definition_hash(attachment: &mut ServingAttachment) {
        attachment.definition_hash = wamn_catalog::DefinitionHash::parse(
            wamn_digest::canonical_json_sha256(&attachment.definition),
        )
        .expect("the canonicalizer emits a valid definition hash");
    }

    fn fixture_route_contracts(
        attachments: &BTreeMap<String, ServingAttachment>,
    ) -> RouteContracts {
        attachments
            .values()
            .map(|attachment| {
                let AttachmentTarget::Route { operation, .. } = &attachment.target;
                (
                    (attachment.package_id.clone(), operation.clone()),
                    OperationType::Command.into(),
                )
            })
            .collect()
    }

    fn package_http_attachment(
        attachment_id: &str,
        package_id: &str,
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
            type_: wamn_catalog::AttachmentType::Http,
            package_id: package_id.to_owned(),
            target: wamn_catalog::AttachmentTarget::Route {
                component: component.to_owned(),
                operation: operation.to_owned(),
            },
            definition_hash: wamn_catalog::DefinitionHash::parse(definition_hash)
                .expect("the canonicalizer emits a valid definition hash"),
            definition,
            auth_policy: serde_json::json!({"modes": ["pat"]}),
            registered_operation: Some(operation.to_owned()),
        }
    }

    #[test]
    fn package_attachment_documents_merge_into_one_release() {
        let base = BTreeMap::from([
            (
                "first".to_owned(),
                package_http_attachment(
                    "first",
                    "source_fixture",
                    "read",
                    "source-fixture:item/get@1.0.0",
                    "/items/get",
                ),
            ),
            (
                "second".to_owned(),
                package_http_attachment(
                    "second",
                    "source_fixture",
                    "list",
                    "source-fixture:item/list@1.0.0",
                    "/items/list",
                ),
            ),
        ]);
        let overlay = BTreeMap::from([(
            "third".to_owned(),
            package_http_attachment(
                "third",
                "observer_fixture",
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
                .filter(|attachment| attachment.package_id == "source_fixture")
                .count(),
            2
        );
        assert_eq!(
            authored
                .values()
                .filter(|attachment| attachment.package_id == "observer_fixture")
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
            "source_fixture",
            "fixture_widget_update",
            "source-fixture:widget/update@1.0.0",
            "/widget/update",
        );
        let overlay = package_http_attachment(
            "fixture-http",
            "observer_fixture",
            "fixture_widget_update",
            "observer-fixture:widget/update@3.0.0",
            "/overlay/widget/update",
        );
        let error = merge_package_attachment_documents(vec![
            (
                PathBuf::from("apps/source_fixture/publication/attachments.json"),
                BTreeMap::from([("fixture-http".to_owned(), base)]),
            ),
            (
                PathBuf::from("apps/observer_fixture/publication/attachments.json"),
                BTreeMap::from([("fixture-http".to_owned(), overlay)]),
            ),
        ])
        .expect_err("one attachment identity cannot have two package owners");

        assert_eq!(
            error.error_type(),
            PublishManifestErrorType::DuplicateAttachmentId
        );
        assert_eq!(error.error_type().as_str(), "duplicate-attachment-id");
        assert!(error.detail().contains("fixture-http"));
        assert!(error.detail().contains("apps/source_fixture"));
        assert!(error.detail().contains("apps/observer_fixture"));
    }

    #[test]
    fn package_attachment_route_collisions_use_the_existing_governor() {
        let first = package_http_attachment(
            "first-http",
            "source_fixture",
            "widget_get",
            "source-fixture:widget/get@1.0.0",
            "/widget/{id}",
        );
        let second = package_http_attachment(
            "second-http",
            "observer_fixture",
            "quality_load_widget_detail",
            "observer-fixture:quality/load-widget-detail@3.0.0",
            "/widget/{widget_id}",
        );
        let authored = merge_package_attachment_documents(vec![
            (
                PathBuf::from("apps/source_fixture/publication/attachments.json"),
                BTreeMap::from([("first-http".to_owned(), first)]),
            ),
            (
                PathBuf::from("apps/observer_fixture/publication/attachments.json"),
                BTreeMap::from([("second-http".to_owned(), second)]),
            ),
        ])
        .expect("distinct attachment identities merge before route validation");
        let error = resolve_route_methods(&authored, &fixture_route_contracts(&authored))
            .expect_err("canonical route collisions remain refused after package merging");

        assert_eq!(error.error_type(), PublishManifestErrorType::Document);
        assert!(error.detail().contains("canonical path and method"));
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
            type_: wamn_catalog::AttachmentType::Http,
            package_id: "source_fixture".to_owned(),
            target: wamn_catalog::AttachmentTarget::Route {
                component: "widget".to_owned(),
                operation: "source-fixture:widget/get@1.0.0".to_owned(),
            },
            definition_hash: wamn_catalog::DefinitionHash::parse(authored_hash.clone())
                .expect("the canonicalizer emits a valid definition hash"),
            definition,
            auth_policy: serde_json::json!({"modes": ["pat"]}),
            registered_operation: Some("source-fixture:widget/get@1.0.0".to_owned()),
        };
        let authored = BTreeMap::from([("fixture-http".to_owned(), attachment)]);

        let resolved = resolve_route_methods(&authored, &fixture_route_contracts(&authored))
            .expect("a routed release needs no route host");
        assert!(
            resolved["fixture-http"].definition["route"]
                .get("host")
                .is_none(),
            "a release names no route host"
        );
        assert_ne!(
            resolved["fixture-http"].definition_hash.as_str(),
            authored_hash
        );
        assert_eq!(
            resolved["fixture-http"].definition_hash.as_str(),
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
        assert!(package_host.detail().contains("remove it"));

        let mut non_routed = package_authored;
        non_routed
            .get_mut("fixture-http")
            .expect("the attachment exists")
            .type_ = wamn_catalog::AttachmentType::Internal;
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
        assert!(extra.detail().contains("exactly a string path field"));

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
        colliding.insert("fixture-http-alias".to_owned(), second);
        let collision = resolve_route_methods(&colliding, &fixture_route_contracts(&colliding))
            .expect_err("one release cannot carry ambiguous route templates");
        assert_eq!(collision.error_type(), PublishManifestErrorType::Document);
        assert!(collision.detail().contains("canonical path and method"));
    }

    #[test]
    fn release_publish_writes_the_method_from_the_operation_type() {
        let route = |id: &str, operation: &str, path: &str| {
            let mut attachment = package_http_attachment(id, "source_fixture", id, operation, path);
            attachment.target = wamn_catalog::AttachmentTarget::Route {
                component: "widget".to_owned(),
                operation: operation.to_owned(),
            };
            (id.to_owned(), attachment)
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
                "wiring-http".to_owned(),
                package_http_attachment(
                    "wiring-http",
                    "source_fixture",
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
                    "source_fixture".to_owned(),
                    format!("source-fixture:{operation}@1.0.0"),
                ),
                RouteContract::from(type_),
            )
        })
        .collect();
        let resolved =
            resolve_route_methods(&authored, &types).expect("every route resolves a method");
        for (id, method) in [
            ("get-http", "GET"),
            ("query-http", "GET"),
            ("load-http", "GET"),
            ("update-http", "POST"),
            ("record-http", "POST"),
            ("wiring-http", "POST"),
        ] {
            assert_eq!(resolved[id].definition["route"]["method"], method, "{id}");
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
        assert!(
            refusal.detail().contains("route.method"),
            "{}",
            refusal.detail()
        );
    }

    #[test]
    fn unchanged_members_keep_the_composed_entry_bytes() {
        let bytes = wat::parse_str("(component)").unwrap();
        let mut member = closure_component("entry", None);
        member.component_digest = wamn_digest::bytes_sha256(&bytes);
        let published = |member: &AdmittedComponent| {
            let manifest = ServingManifest::new(
                ServingRelease {
                    packages: [PackageCoordinate::new("base", "1.0.0").unwrap()].into(),
                },
                [ServingComponent::project(member, Vec::new()).unwrap()].into(),
                BTreeSet::new(),
                BTreeMap::new(),
            )
            .unwrap();
            PublishedRelease {
                digest: manifest.digest(),
                canonical_bytes: manifest.canonical_bytes(),
                manifest,
            }
        };
        let applications = [(
            "application".to_owned(),
            wamn_catalog::ApplicationDefinition {
                connections: BTreeMap::new(),
                packages: ["base".to_owned()].into(),
            },
        )]
        .into();
        // These route bytes cannot compose. A matching entry must reuse its bytes.
        let route_bytes = b"route";
        let before = b"already composed";
        let mut entry = Some(ComposedApplication {
            bytes: before.to_vec().into_boxed_slice(),
            digest: wamn_digest::bytes_sha256(before),
            route_digest: wamn_digest::bytes_sha256(route_bytes),
            application: wamn_catalog::ServingEntryMember {
                package_id: "base".to_owned(),
                component: "entry".to_owned(),
                digest: member.component_digest.clone(),
            },
            application_digests: [member.component_digest.clone()].into(),
        });
        assert!(
            !refresh_composed_entry(
                &mut entry,
                &published(&member),
                &[(&member, &bytes)],
                route_bytes,
                &applications,
            )
            .unwrap()
        );
        assert_eq!(entry.as_ref().unwrap().bytes(), before);

        let changed = wat::parse_str("(component (component))").unwrap();
        member.component_digest = wamn_digest::bytes_sha256(&changed);
        assert!(
            refresh_composed_entry(
                &mut entry,
                &published(&member),
                &[(&member, &changed)],
                route_bytes,
                &applications,
            )
            .is_err(),
            "a changed member must attempt composition"
        );
        assert_eq!(
            entry.as_ref().unwrap().bytes(),
            before,
            "a failed composition keeps the previous entry"
        );
    }

    #[test]
    fn application_membership_requires_one_covering_component() {
        let bytes = [
            wat::parse_str("(component)").unwrap(),
            wat::parse_str("(component (component))").unwrap(),
            wat::parse_str("(component (component (component)))").unwrap(),
        ];
        let facts: Vec<_> = ["base", "overlay", "outer"]
            .into_iter()
            .zip(&bytes)
            .map(|(package, bytes)| {
                let mut fact = closure_component("entry", None);
                fact.target.package_id = package.into();
                fact.component_digest = wamn_digest::bytes_sha256(bytes);
                fact
            })
            .collect();
        let manifest = ServingManifest::new(
            ServingRelease {
                packages: facts
                    .iter()
                    .map(|fact| PackageCoordinate::new(&fact.target.package_id, "1.0.0").unwrap())
                    .collect(),
            },
            facts
                .iter()
                .map(|fact| ServingComponent::project(fact, Vec::new()).unwrap())
                .collect(),
            facts[..2]
                .iter()
                .map(|fact| ServingRoute {
                    package_id: fact.target.package_id.clone(),
                    component: "entry".into(),
                    operation: "run".into(),
                    type_: OperationType::Get,
                    reads: BTreeSet::new(),
                    revision: None,
                    idempotency: None,
                })
                .collect(),
            BTreeMap::new(),
        )
        .unwrap();
        let published = PublishedRelease {
            digest: manifest.digest(),
            canonical_bytes: manifest.canonical_bytes(),
            manifest,
        };
        let applications = [(
            "orders".into(),
            wamn_catalog::ApplicationDefinition {
                connections: std::collections::BTreeMap::new(),
                packages: ["base".into(), "overlay".into()].into(),
            },
        )]
        .into();
        let candidates = || {
            facts
                .iter()
                .zip(&bytes)
                .map(|(fact, bytes)| (fact, bytes.as_slice()))
                .collect::<Vec<_>>()
        };
        // No selector is needed for a single application. The larger outer
        // component is outside its declared packages and cannot become its member.
        let (member, _) =
            covering_application(&published, &candidates(), &applications, None).unwrap();
        assert_eq!(member.target.package_id, "overlay");
        assert_eq!(
            covering_application(&published, &candidates(), &applications, Some("orders"))
                .unwrap()
                .0,
            member
        );
        let error =
            covering_application(&published, &candidates()[..1], &applications, None).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<PublishManifestError>()
                .unwrap()
                .error_type(),
            PublishManifestErrorType::ApplicationCoverage
        );

        let mut two = applications.clone();
        two.insert(
            "rest".into(),
            wamn_catalog::ApplicationDefinition {
                connections: BTreeMap::new(),
                packages: ["base".into()].into(),
            },
        );
        assert!(covering_application(&published, &candidates(), &two, None).is_err());
        let mut both = two.clone();
        both.get_mut("rest")
            .unwrap()
            .packages
            .insert("overlay".into());
        let error = covering_application(&published, &candidates(), &both, None).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<PublishManifestError>()
                .unwrap()
                .error_type(),
            PublishManifestErrorType::ApplicationSelection
        );
        assert_eq!(
            covering_application(&published, &candidates(), &both, Some("orders"))
                .unwrap()
                .0,
            member
        );

        let mut ambiguous = applications.clone();
        ambiguous
            .get_mut("orders")
            .unwrap()
            .packages
            .insert("outer".into());
        let error = covering_application(&published, &candidates(), &ambiguous, None).unwrap_err();
        let error = error.downcast_ref::<PublishManifestError>().unwrap();
        assert_eq!(
            error.error_type(),
            PublishManifestErrorType::AmbiguousApplicationMember
        );
    }

    /// A package directory whose `descriptors/` holds the canonical descriptor
    /// of `fact` with one connection, as an unpacked artifact holds it.
    fn unpacked_descriptor(label: &str, fact: &AdmittedComponent) -> (PathBuf, Vec<u8>) {
        let root = std::env::temp_dir().join(format!(
            "wamn-publish-descriptor-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(
            root.join(wamn_package_control::package_artifact::DESCRIPTOR_DIRECTORY),
        )
        .unwrap();
        let descriptor = wamn_catalog::ComponentDescriptor::new(
            fact.clone(),
            vec![ComponentConnection {
                store_alias: "labels".to_owned().into(),
                requirement_type: wamn_catalog::ComponentConnectionType::Blobstore,
                mode: wamn_catalog::ConnectionMode::Write,
            }],
        );
        let bytes = wamn_digest::canonical_json_bytes(&serde_json::to_value(&descriptor).unwrap());
        std::fs::write(
            root.join(wamn_package_control::package_artifact::DESCRIPTOR_DIRECTORY)
                .join(format!("{}.json", fact.component)),
            &bytes,
        )
        .unwrap();
        (root, bytes)
    }

    /// The release component carries the descriptor layer byte for byte, and
    /// two projects read one component from one artifact.
    #[tokio::test]
    async fn a_release_component_carries_its_descriptor_layer_in_every_project_env_key() {
        let fact = closure_component("registered-component", Some("base:widget/get@1.0.0"));
        let (root, layer) = unpacked_descriptor("layer", &fact);
        let opened = wamn_package_control::package_artifact::open_package_source(
            wamn_package_control::package_artifact::PackageSource::Directory(root.clone()),
        )
        .await
        .unwrap();
        let packages = [PackageCoordinate::new("base", "1.0.0").unwrap()];
        let project = |project_env_key: &str| {
            let components =
                descriptor_components(project_env_key, &packages, std::slice::from_ref(&opened))
                    .unwrap();
            let fact = &components.facts[&("base".to_owned(), "1.0.0".to_owned())][0];
            assert_eq!(fact.target.project_env_key, project_env_key);
            project_serving_component(fact, components.connections(fact).unwrap()).unwrap()
        };
        let first = project("acme--dev--plant");
        let second = project("acme--prod--plant");
        assert_eq!(first, second);
        assert_eq!(
            wamn_digest::canonical_json_bytes(&serde_json::to_value(&first.descriptor).unwrap()),
            layer
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A descriptor file that is not canonical JSON is not a layer a release
    /// can carry byte for byte.
    #[test]
    fn a_descriptor_that_is_not_canonical_refuses() {
        let fact = closure_component("registered-component", None);
        let (root, layer) = unpacked_descriptor("pretty", &fact);
        let value: serde_json::Value = serde_json::from_slice(&layer).unwrap();
        std::fs::write(
            root.join(wamn_package_control::package_artifact::DESCRIPTOR_DIRECTORY)
                .join("registered-component.json"),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
        let error = wamn_package_control::package_artifact::read_descriptors(&root).unwrap_err();
        assert!(
            format!("{error:#}").contains("is not canonical JSON"),
            "{error:#}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn dependency_manifest(digest: &str) -> wamn_compiler::manifest::PackageManifest {
        let mut document: serde_json::Value = serde_json::from_str(include_str!(
            "../../package/tests/fixtures/component_package/generated/wamn.json"
        ))
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
            model.schema = "own_schema".into();
        }
        assert_eq!(
            application_runtime_schema(&manifest, &BTreeMap::new(), &BTreeSet::new())
                .unwrap()
                .as_deref(),
            Some("own_schema")
        );
        for model in manifest.models.values_mut() {
            model.schema = "base_schema".into();
            model.owner = "base_package".into();
        }
        assert_eq!(
            application_runtime_schema(&manifest, &BTreeMap::new(), &BTreeSet::new())
                .unwrap()
                .as_deref(),
            Some("base_schema")
        );
        let mut extra = manifest.models.values().next().unwrap().clone();
        extra.schema = "other_schema".into();
        manifest.models.insert("other".into(), extra);
        assert!(application_runtime_schema(&manifest, &BTreeMap::new(), &BTreeSet::new()).is_err());
    }

    #[test]
    fn composed_schema_uses_base_dependencies_without_copied_models() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let mut overlay = dependency_manifest(&digest);
        overlay.models.clear();
        overlay.custom_operations.clear();
        overlay.internal_relations.clear();
        let mut base = source_manifest();
        base.package.id = "base".into();
        base.custom_operations.clear();
        base.internal_relations.clear();
        for model in base.models.values_mut() {
            model.schema = "base_schema".into();
        }
        let mut consumer = base.clone();
        consumer.package.id = "consumer".into();
        for model in consumer.models.values_mut() {
            model.schema = "consumer_schema".into();
        }
        let mut dependency = overlay.base_dependencies["base"].clone();
        dependency.package = "consumer".into();
        dependency.digest = format!("sha256:{}", "b".repeat(64));
        overlay
            .base_dependencies
            .insert("consumer".into(), dependency);
        let mut descriptor = wamn_catalog::ComponentDescriptor::named(
            "overlay",
            "1.0.0",
            format!("sha256:{}", "c".repeat(64)),
        );
        descriptor
            .statement_instances
            .insert("sql-0".into(), digest);
        let components = BTreeSet::from([ServingComponent {
            package_id: overlay.package.id.clone(),
            runtime_schema: None,
            descriptor,
            statement_instances: BTreeMap::new(),
        }]);
        let manifests = BTreeMap::from([("base".into(), base), ("consumer".into(), consumer)]);
        assert_eq!(
            application_runtime_schema(&overlay, &manifests, &components)
                .unwrap()
                .as_deref(),
            Some("base_schema")
        );
        assert!(application_runtime_schema(&overlay, &BTreeMap::new(), &components).is_err());
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
        id: &str,
        version: &str,
        base: (&str, &str),
        digest: &str,
    ) -> wamn_compiler::manifest::PackageManifest {
        let mut document = serde_json::to_value(dependency_manifest(digest))
            .expect("the dependency fixture manifest serializes");
        document["package"] = serde_json::json!({"id": id, "version": version});
        document["base_dependencies"]["base"]["package"] = serde_json::json!(base.0);
        document["base_dependencies"]["base"]["version"] = serde_json::json!(base.1);
        serde_json::from_value(document).expect("the pinning manifest parses")
    }

    #[test]
    fn base_dependencies_expand_the_exact_release_closure_and_refuse_cycles() {
        let base_operation = "base:stock/record-widget@1.0.0";
        let overlay_operation = "overlay:stock/record-widget@3.0.0";
        let mut base = closure_component("base-component", Some(base_operation));
        base.target.package_id = "base".to_owned();
        let mut overlay = closure_component("overlay-component", Some(overlay_operation));
        overlay.target.package_id = "overlay".to_owned();
        overlay.target.package_version = "3.0.0".to_owned();
        overlay.component_digest = format!("sha256:{}", "b".repeat(64));
        let roots = BTreeMap::from([("entry".to_owned(), overlay.clone())]);
        let facts = BTreeMap::from([
            (("base".to_owned(), "1.0.0".to_owned()), vec![base.clone()]),
            (
                ("overlay".to_owned(), "3.0.0".to_owned()),
                vec![overlay.clone()],
            ),
        ]);
        let manifests = BTreeMap::from([(
            "overlay".to_owned(),
            pinning_manifest(
                "overlay",
                "3.0.0",
                ("base", "1.0.0"),
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
        let projected =
            project_serving_component(&overlay, Vec::new()).expect("the composed overlay projects");
        assert_eq!(
            projected.descriptor.operations[overlay_operation]
                .registered_operation
                .as_deref(),
            Some(overlay_operation),
            "the overlay entry requires its own permission alone"
        );

        let mut cyclic = manifests;
        cyclic.insert(
            "base".to_owned(),
            pinning_manifest(
                "base",
                "1.0.0",
                ("overlay", "3.0.0"),
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
        assert!(error.detail().contains("cycle"));
    }

    /// wamn-10yt.48 at the closure: an author edits a base package, the built
    /// digest moves, and the overlay's authored pin still names the old bytes.
    /// A durable release refuses that; a disposable target built both halves
    /// from this same tree, so it resolves the base by coordinate.
    #[test]
    fn a_disposable_target_expands_a_closure_whose_pinned_base_digest_moved() {
        let base_operation = "base:stock/record-widget@1.0.0";
        let overlay_operation = "overlay:stock/record-widget@3.0.0";
        let mut base = closure_component("base-component", Some(base_operation));
        base.target.package_id = "base".to_owned();
        let mut overlay = closure_component("overlay-component", Some(overlay_operation));
        overlay.target.package_id = "overlay".to_owned();
        overlay.target.package_version = "3.0.0".to_owned();
        overlay.component_digest = format!("sha256:{}", "b".repeat(64));
        let stale_pin = format!("sha256:{}", "d".repeat(64));
        assert_ne!(stale_pin, base.component_digest);
        let roots = BTreeMap::from([("entry".to_owned(), overlay.clone())]);
        let facts = BTreeMap::from([
            (("base".to_owned(), "1.0.0".to_owned()), vec![base.clone()]),
            (
                ("overlay".to_owned(), "3.0.0".to_owned()),
                vec![overlay.clone()],
            ),
        ]);
        let manifests = BTreeMap::from([(
            "overlay".to_owned(),
            pinning_manifest("overlay", "3.0.0", ("base", "1.0.0"), &stale_pin),
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

    #[test]
    fn release_projection_preserves_operation_targeted_statement_facts() {
        let operation = "base:widget/get@1.0.0";
        let sql = "SELECT row_version FROM widget WHERE id = $1";
        let path = "generated/sql/widget/get.sql".to_owned();
        let mut admitted = closure_component("base-component", Some(operation));
        admitted
            .operations
            .get_mut(operation)
            .expect("fixture has the registered operation")
            .statements
            .insert(
                path.clone(),
                wamn_catalog::ComponentSqlStatement {
                    name: "get".to_owned(),
                    path: "generated/sql/widget/get.sql".to_owned(),
                    sql: sql.to_owned(),
                    binds: Vec::new(),
                    columns: Vec::new(),
                    relations: Vec::new(),
                    transactional: false,
                },
            );

        let serving = project_serving_component(&admitted, Vec::new())
            .expect("an admitted component projects to serving facts");

        assert_eq!(
            serving.descriptor.operations[operation].statement(&path),
            admitted.operations[operation].statement(&path)
        );
    }
}

mod manifest_push {
    //! Push canonical format-1 serving-manifest bytes as one OCI data artifact.
    //!
    //! The manifest's RFC 8785 SHA-256 identity derives its immutable OCI tag. An
    //! exact retry pulls and verifies the existing artifact and performs no push.
    //! A tag holding any other layout or bytes refuses instead of being replaced.
    //!
    //! `publish-release` pushes the bytes it composed before it records them
    //! (docs/architecture/components.md#application-http-composition).

    use std::fmt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
    use oci_client::manifest::OciImageManifest;
    use oci_client::secrets::RegistryAuth;
    use oci_client::{Client as OciClient, Reference};
    use wamn_catalog::{ManifestDigest, ServingManifest, ServingRelease};
    use wamn_package_artifact::read_ca_bundles;
    use wamn_package_artifact::registry_credentials::read_registry_push_credentials;
    use wamn_package_artifact::{artifact_is_absent, registry_auth};
    use wamn_runtime::release_manifest_artifact::{
        RELEASE_MANIFEST_CONFIG_BYTES, ReleaseManifestArtifactBlobs,
        release_manifest_artifact_layout, release_manifest_artifact_reference,
        verify_release_manifest_artifact_layout,
    };

    /// Bound each registry connect/read phase without adding a deployment knob.
    const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);

    /// Stable prefix every release-manifest OCI refusal renders with.
    pub const RELEASE_MANIFEST_PUBLISH_REFUSAL: &str = "release-manifest-publish-refused";

    /// Stable classification of a release-manifest publication refusal.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ReleaseManifestPushErrorType {
        Document,
        Reference,
        Credential,
        TrustAnchor,
        Transport,
        Conflict,
    }

    impl ReleaseManifestPushErrorType {
        const fn as_str(self) -> &'static str {
            match self {
                Self::Document => "document",
                Self::Reference => "reference",
                Self::Credential => "credential",
                Self::TrustAnchor => "trust-anchor",
                Self::Transport => "transport",
                Self::Conflict => "conflict",
            }
        }
    }

    /// Contextual refusal from the release-manifest OCI boundary.
    #[derive(Debug)]
    pub struct ReleaseManifestPushError {
        type_: ReleaseManifestPushErrorType,
        refusal: &'static str,
        detail: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    }

    impl ReleaseManifestPushError {
        /// Stable refusal class for callers that must not match display text.
        pub const fn error_type(&self) -> ReleaseManifestPushErrorType {
            self.type_
        }

        /// Stable literal naming the rejected invariant.
        pub const fn refusal(&self) -> &'static str {
            self.refusal
        }

        fn new(
            type_: ReleaseManifestPushErrorType,
            refusal: &'static str,
            detail: impl Into<String>,
        ) -> Self {
            Self {
                type_,
                refusal,
                detail: detail.into(),
                source: None,
            }
        }

        fn with_source(
            type_: ReleaseManifestPushErrorType,
            refusal: &'static str,
            detail: impl Into<String>,
            source: impl std::error::Error + Send + Sync + 'static,
        ) -> Self {
            Self {
                type_,
                refusal,
                detail: detail.into(),
                source: Some(Box::new(source)),
            }
        }
    }

    impl fmt::Display for ReleaseManifestPushError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                formatter,
                "{RELEASE_MANIFEST_PUBLISH_REFUSAL} ({}; {}): {}",
                self.type_.as_str(),
                self.refusal,
                self.detail
            )
        }
    }

    impl std::error::Error for ReleaseManifestPushError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.source.as_deref().map(|source| source as _)
        }
    }

    /// Whether this invocation created the artifact or matched an exact retry.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ReleaseManifestPushDisposition {
        Pushed,
        AlreadyPresent,
    }

    /// Verified result of pushing one canonical serving manifest.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct PushedReleaseManifest {
        pub digest: ManifestDigest,
        pub disposition: ReleaseManifestPushDisposition,
        /// Effective-release coordinate carried by the pushed bytes.
        pub release: ServingRelease,
    }

    /// Push canonical format-1 bytes or make sure that their exact artifact already exists.
    pub async fn push_manifest_bytes(
        canonical_bytes: &[u8],
        artifact_base: &str,
        insecure_registry: bool,
        oci_ca_paths: &[PathBuf],
        registry_auth_file: &Path,
    ) -> Result<PushedReleaseManifest, ReleaseManifestPushError> {
        let (admitted, digest) =
            ServingManifest::from_canonical_bytes(canonical_bytes).map_err(|source| {
                ReleaseManifestPushError::with_source(
                    ReleaseManifestPushErrorType::Document,
                    "release-manifest-document-refused",
                    "input is not canonical format-1 ServingManifest JSON",
                    source,
                )
            })?;
        let release = admitted.release;
        let artifact = release_manifest_artifact_reference(artifact_base, digest.as_str())
            .map_err(|source| {
                ReleaseManifestPushError::with_source(
                    ReleaseManifestPushErrorType::Reference,
                    "release-manifest-artifact-reference-refused",
                    "artifact base or manifest digest cannot form an immutable OCI reference",
                    source,
                )
            })?;
        let credentials = read_registry_push_credentials(registry_auth_file, artifact.registry())
            .map_err(|source| {
            ReleaseManifestPushError::with_source(
                ReleaseManifestPushErrorType::Credential,
                "release-manifest-registry-credential-refused",
                format!("load push credential for registry {}", artifact.registry()),
                source,
            )
        })?;
        let reference = Reference::with_tag(
            artifact.registry().to_owned(),
            artifact.repository().to_owned(),
            artifact.tag().to_owned(),
        );
        let ca_bundles = read_ca_bundles(oci_ca_paths).map_err(|source| {
            ReleaseManifestPushError::with_source(
                ReleaseManifestPushErrorType::TrustAnchor,
                "release-manifest-ca-bundle-unreadable",
                format!("read CA bundles for registry {}", artifact.registry()),
                source,
            )
        })?;
        let client = registry_client(artifact.registry(), insecure_registry, ca_bundles)?;
        let auth = registry_auth(credentials.as_ref());

        if probe_exact_artifact(&client, &reference, &auth, canonical_bytes, &digest).await? {
            return Ok(PushedReleaseManifest {
                digest,
                disposition: ReleaseManifestPushDisposition::AlreadyPresent,
                release,
            });
        }

        let (layer, config, manifest) = release_manifest_artifact_layout(canonical_bytes);
        client
            .push(
                &reference,
                std::slice::from_ref(&layer),
                config,
                &auth,
                Some(manifest),
            )
            .await
            .map_err(|source| {
                ReleaseManifestPushError::with_source(
                    ReleaseManifestPushErrorType::Transport,
                    "release-manifest-artifact-push-failed",
                    format!("push release-manifest artifact {reference}"),
                    source,
                )
            })?;

        if !probe_exact_artifact(&client, &reference, &auth, canonical_bytes, &digest).await? {
            return Err(ReleaseManifestPushError::new(
                ReleaseManifestPushErrorType::Transport,
                "release-manifest-artifact-not-visible",
                format!("pushed release-manifest artifact {reference} is not readable"),
            ));
        }
        Ok(PushedReleaseManifest {
            digest,
            disposition: ReleaseManifestPushDisposition::Pushed,
            release,
        })
    }

    /// One registry client for this push, built here rather than borrowed from
    /// `wash_runtime::oci`.
    ///
    /// That module exposes no client-construction seam: its transfer surface is
    /// `pull_component`/`push_component`, both fixed to `WASM_LAYER_MEDIA_TYPE` and
    /// a `WasmConfig` blob, and `push_component` refuses non-wasm bytes outright
    /// through `wit_component::decode_reader`. A release manifest is canonical
    /// JSON. This path also needs `HttpsExcept` for a single insecure registry and
    /// `OciErrorCode::ManifestUnknown` discrimination for the exact-retry probe,
    /// neither of which survives that API. See standing trigger 5 in
    /// `docs/architecture/native-alignment.md#retained-wamn-implementations` (`wamn-kdhw`).
    ///
    /// It is built through `TryFrom`, not `Client::new`: that constructor answers a
    /// rejected configuration with a warning and a wholly default client, which
    /// drops the trust roots, the protocol and the timeouts and turns an unusable
    /// CA bundle into a confusing TLS failure on the push itself.
    fn registry_client(
        registry: &str,
        insecure_registry: bool,
        ca_bundles: Vec<Vec<u8>>,
    ) -> Result<OciClient, ReleaseManifestPushError> {
        let protocol = if insecure_registry {
            ClientProtocol::HttpsExcept(vec![registry.to_owned()])
        } else {
            ClientProtocol::Https
        };
        OciClient::try_from(ClientConfig {
            protocol,
            read_timeout: Some(REGISTRY_IO_TIMEOUT),
            connect_timeout: Some(REGISTRY_IO_TIMEOUT),
            extra_root_certificates: ca_bundles
                .into_iter()
                .map(|data| Certificate {
                    encoding: CertificateEncoding::Pem,
                    data,
                })
                .collect(),
            ..ClientConfig::default()
        })
        .map_err(|source| {
            ReleaseManifestPushError::with_source(
                ReleaseManifestPushErrorType::TrustAnchor,
                "release-manifest-registry-client-unusable",
                format!("build the push client for registry {registry}"),
                source,
            )
        })
    }

    async fn probe_exact_artifact(
        client: &OciClient,
        reference: &Reference,
        auth: &RegistryAuth,
        expected_bytes: &[u8],
        expected_digest: &ManifestDigest,
    ) -> Result<bool, ReleaseManifestPushError> {
        let deadline = tokio::time::Instant::now() + REGISTRY_IO_TIMEOUT;
        let (manifest, _) = match wamn_package_artifact::registry_pull::retry(
            reference,
            deadline,
            || client.pull_image_manifest(reference, auth),
            |error| error,
        )
        .await
        {
            Ok(found) => found,
            Err(source) if artifact_is_absent(&source) => return Ok(false),
            Err(source) => {
                return Err(ReleaseManifestPushError::with_source(
                    ReleaseManifestPushErrorType::Transport,
                    "release-manifest-artifact-probe-failed",
                    format!("probe release-manifest artifact {reference}"),
                    source,
                ));
            }
        };
        let verified = verify_manifest_layout(
            &manifest,
            expected_digest.as_str(),
            expected_bytes.len(),
            reference,
        )?;

        let body =
            wamn_package_artifact::registry_pull::blob(client, reference, verified.layer, deadline)
                .await
                .map_err(|source| {
                    ReleaseManifestPushError::with_source(
                        ReleaseManifestPushErrorType::Transport,
                        "release-manifest-artifact-body-unavailable",
                        format!("pull release-manifest body {reference}"),
                        source,
                    )
                })?;
        if body != expected_bytes {
            return Err(conflict(
                reference,
                "release-manifest-artifact-body-mismatch",
            ));
        }

        let config = wamn_package_artifact::registry_pull::blob(
            client,
            reference,
            verified.config,
            deadline,
        )
        .await
        .map_err(|source| {
            ReleaseManifestPushError::with_source(
                ReleaseManifestPushErrorType::Transport,
                "release-manifest-artifact-config-unavailable",
                format!("pull release-manifest config {reference}"),
                source,
            )
        })?;
        if config != RELEASE_MANIFEST_CONFIG_BYTES {
            return Err(conflict(
                reference,
                "release-manifest-artifact-config-body-mismatch",
            ));
        }
        Ok(true)
    }

    /// Translate the shared layout contract's refusal into this boundary's error.
    fn verify_manifest_layout<'a>(
        manifest: &'a OciImageManifest,
        expected_digest: &str,
        expected_size: usize,
        reference: &Reference,
    ) -> Result<ReleaseManifestArtifactBlobs<'a>, ReleaseManifestPushError> {
        verify_release_manifest_artifact_layout(manifest, expected_digest, Some(expected_size))
            .map_err(|refusal| conflict(reference, refusal.refusal()))
    }

    fn conflict(reference: &Reference, refusal: &'static str) -> ReleaseManifestPushError {
        ReleaseManifestPushError::new(
            ReleaseManifestPushErrorType::Conflict,
            refusal,
            format!("existing release-manifest artifact {reference} is not exact"),
        )
    }

    #[cfg(test)]
    mod tests {
        use wamn_runtime::component_artifact_source::OCI_CA_PATHS_ENV;
        use wamn_runtime::release_manifest_artifact::RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE;

        use super::*;

        const CANONICAL_MANIFEST: &[u8] = br#"{"attachments":{},"components":[{"descriptor":{"component":"http-request","component-digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","connections":[],"effects":[],"imports":[],"imports-fingerprint":"","interface-version":"0.1","operations":{"wamn:node/handler@0.1.0":{"input-ports":[],"output-ports":[],"parameters":[]}}},"package-id":"orders"}],"format-version":11,"release":{"packages":[{"package-id":"orders","package-version":"1.0.0"}]},"routes":[]}"#;

        fn fixture_reference() -> Reference {
            Reference::with_tag(
                "registry.example".to_owned(),
                "wamn/releases".to_owned(),
                "a".repeat(64),
            )
        }

        #[test]
        fn exact_layout_carries_only_canonical_manifest_bytes() {
            let (_, digest) = ServingManifest::from_canonical_bytes(CANONICAL_MANIFEST)
                .expect("fixture is a canonical format-1 manifest");
            let (layer, config, manifest) = release_manifest_artifact_layout(CANONICAL_MANIFEST);
            let verified = verify_manifest_layout(
                &manifest,
                digest.as_str(),
                CANONICAL_MANIFEST.len(),
                &fixture_reference(),
            )
            .expect("push layout verifies");

            assert_eq!(&layer.data[..], CANONICAL_MANIFEST);
            assert_eq!(&config.data[..], RELEASE_MANIFEST_CONFIG_BYTES);
            assert_eq!(verified.layer.digest, digest.as_str());
            assert_eq!(manifest.layers.len(), 1);
        }

        #[test]
        fn unsupported_format_and_noncanonical_documents_refuse_before_transport() {
            let unsupported = std::str::from_utf8(CANONICAL_MANIFEST)
                .expect("fixture is UTF-8")
                .replacen("\"format-version\":11", "\"format-version\":9", 1);
            let unsupported = ServingManifest::from_canonical_bytes(unsupported.as_bytes())
                .expect_err("format nine refuses");
            assert!(format!("{unsupported}").contains("unsupported-serving-manifest-version"));

            let mut indented = CANONICAL_MANIFEST.to_vec();
            indented.insert(1, b' ');
            assert!(ServingManifest::from_canonical_bytes(&indented).is_err());
        }

        #[test]
        fn wrong_or_multi_layer_layout_refuses_as_conflict() {
            let (_, digest) = ServingManifest::from_canonical_bytes(CANONICAL_MANIFEST)
                .expect("fixture is a canonical format-1 manifest");
            let (_, _, mut manifest) = release_manifest_artifact_layout(CANONICAL_MANIFEST);
            manifest.layers[0].media_type = "application/octet-stream".to_owned();
            let error = verify_manifest_layout(
                &manifest,
                digest.as_str(),
                CANONICAL_MANIFEST.len(),
                &fixture_reference(),
            )
            .expect_err("foreign layer layout refuses");
            assert_eq!(error.error_type(), ReleaseManifestPushErrorType::Conflict);
            assert_eq!(error.refusal(), "release-manifest-artifact-layer-mismatch");

            let duplicate = manifest.layers[0].clone();
            manifest.layers[0].media_type = RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE.to_owned();
            manifest.layers.push(duplicate);
            let error = verify_manifest_layout(
                &manifest,
                digest.as_str(),
                CANONICAL_MANIFEST.len(),
                &fixture_reference(),
            )
            .expect_err("multiple layers refuse");
            assert_eq!(
                error.refusal(),
                "release-manifest-artifact-layer-cardinality-mismatch"
            );
        }

        #[tokio::test]
        #[ignore = "requires: env:WAMN_RELEASE_MANIFEST_ARTIFACT_BASE, env:WAMN_REGISTRY_AUTH_FILE"]
        async fn pushing_same_bytes_uploads_nothing() {
            wamn_test_postgres::require_ignored_prerequisites(
                "requires: env:WAMN_RELEASE_MANIFEST_ARTIFACT_BASE, env:WAMN_REGISTRY_AUTH_FILE",
            );
            let artifact_base = std::env::var("WAMN_RELEASE_MANIFEST_ARTIFACT_BASE")
                .expect("set WAMN_RELEASE_MANIFEST_ARTIFACT_BASE to a disposable repository");
            let registry_auth_file = std::env::var("WAMN_REGISTRY_AUTH_FILE")
                .expect("set WAMN_REGISTRY_AUTH_FILE to its Docker config credential");
            // A CA names a TLS registry; without one the registry stays plain HTTP.
            let oci_ca_paths: Vec<PathBuf> = std::env::var(OCI_CA_PATHS_ENV)
                .map(|paths| paths.split(',').map(PathBuf::from).collect())
                .unwrap_or_default();
            let insecure_registry = oci_ca_paths.is_empty();

            let first = push_manifest_bytes(
                CANONICAL_MANIFEST,
                &artifact_base,
                insecure_registry,
                &oci_ca_paths,
                Path::new(&registry_auth_file),
            )
            .await
            .expect("first publication converges");
            let retry = push_manifest_bytes(
                CANONICAL_MANIFEST,
                &artifact_base,
                insecure_registry,
                &oci_ca_paths,
                Path::new(&registry_auth_file),
            )
            .await
            .expect("exact retry converges");

            assert_eq!(first.digest, retry.digest);
            assert_eq!(
                retry.disposition,
                ReleaseManifestPushDisposition::AlreadyPresent
            );
        }
    }
}

#[cfg(test)]
mod effective_release_live;

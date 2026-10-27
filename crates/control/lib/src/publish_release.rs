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

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::{
    AdmittedComponent, AttachmentTarget, ComponentConnection, ComponentPackageScope,
    ManifestDigest, OperationType, PackageCoordinate, RouteCanonicalization,
    SERVING_MANIFEST_FORMAT_VERSION, ServingAttachment, ServingComponent, ServingManifest,
    ServingRelation, ServingRelease, ServingRoute,
};
use wamn_engine::release_manifest::ReleaseScope;
use wamn_schema_control::{
    BareSchemaName, HttpRoute as AuthoredHttpRoute, canonical_http_route_template,
    normalize_http_route,
};

use crate::package_artifact::{OpenedPackage, PackageRegistry};
use crate::verification_policy::AuthoritativeEnvironmentPolicy;

mod attachments;
mod components;
mod entry_publication;
mod package_sources;

use attachments::{
    read_package_attachments, resolve_route_methods, validate_attachment_definition_hashes,
};
use components::{
    project_serving_component, resolve_component_dependency_closure, resolve_route_component,
};
pub use package_sources::package_route;
use package_sources::read_package_manifests;

pub use manifest_push::push_manifest_bytes;

#[cfg(test)]
use attachments::merge_package_attachment_documents;

const CLAIM_ENV_KEY_SQL: &str = "SELECT set_config('app.env_key', $1, false)";
const SELECT_APPLIED_PACKAGE_DIGEST_SQL: &str = "\
SELECT artifact_digest FROM catalog.packages \
 WHERE env_key = $1 AND package_id = $2 AND package_version = $3";
const READ_RELEASE_SQL: &str = "\
SELECT canonical_bytes FROM catalog.releases \
 WHERE env_key = $1 AND manifest_digest = $2";

fn projected_environment_policy_sql(run_schema: &BareSchemaName) -> String {
    format!(
        "SELECT expected_environment, source_policy_org, source_policy_hash \
           FROM {}.environment_policies WHERE env_key = $1",
        run_schema.quoted()
    )
}

/// Inputs owned by the release publisher.
#[derive(Debug)]
pub struct PublishReleaseManifest<'a> {
    pub org: &'a str,
    pub project: &'a str,
    pub environment: &'a str,
    pub verified_publisher_principal: &'a str,
    pub packages: &'a BTreeSet<PackageCoordinate>,
    pub attachments: &'a BTreeMap<String, ServingAttachment>,
    /// Whether provisioning marked this target's environment disposable.
    ///
    /// Read from the projection wamn-10yt.38 writes into the control store, so
    /// the condition is the TARGET and never an operator's say-so. It selects
    /// the dependency rule below and changes nothing else.
    pub environment_is_disposable: bool,
}

impl PublishReleaseManifest<'_> {
    /// The composed environment key derived from the environment coordinate.
    pub fn env_key(&self) -> String {
        wamn_control_registry::project_env_key(self.org, self.project, self.environment)
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
    EnvironmentPolicyAbsent,
    EnvironmentPolicyMismatch,
    EnvironmentPolicySourceMismatch,
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
            Self::EnvironmentPolicyAbsent => "environment-policy-not-converged",
            Self::EnvironmentPolicyMismatch => "environment-policy-environment-mismatch",
            Self::EnvironmentPolicySourceMismatch => "environment-policy-source-mismatch",
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
    pub fn new(kind: PublishManifestErrorType, detail: impl Into<String>) -> Self {
        Self {
            type_: kind,
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
type RouteContracts = BTreeMap<(String, String), RouteContract>;

/// What a route takes from the generated contract `operation.json` of its
/// operation: the kind, the relations a read reads, and the revision field of
/// a `get`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RouteContract {
    kind: OperationType,
    reads: BTreeSet<ServingRelation>,
    revision: Option<String>,
    idempotency: Option<String>,
    canonicalization: Option<RouteCanonicalization>,
}

#[cfg(test)]
impl From<OperationType> for RouteContract {
    fn from(kind: OperationType) -> Self {
        Self {
            kind,
            reads: BTreeSet::new(),
            revision: None,
            idempotency: None,
            canonicalization: None,
        }
    }
}

/// Inputs of one effective-release publication.
#[derive(Debug)]
pub struct PublishReleaseRequest {
    /// Application membership from environment.k.
    pub applications: wamn_catalog::EnvironmentApplications,
    /// Selects one declared application; omitted for a one-application release.
    pub application: Option<String>,
    /// Owner connection to the project-environment database the release is published in.
    pub database_url: String,
    /// Owner connection to the control database holding the environment
    /// policy and the disposable marker.
    pub control_database_url: String,
    /// Registry organization owning the environment policy.
    pub org: String,
    pub project: String,
    /// Environment the release is published for.
    pub environment: String,
    /// Principal already authenticated by the publication boundary.
    pub verified_publisher_principal: String,
    /// Run-plane schema holding the tenant's environment policy.
    pub run_schema: String,
    /// Exact package membership.
    pub packages: Vec<PackageCoordinate>,
}

impl PublishReleaseRequest {
    /// The composed environment key derived from the environment coordinate.
    pub fn env_key(&self) -> String {
        wamn_control_registry::project_env_key(&self.org, &self.project, &self.environment)
    }
}

/// Parse one exact `PACKAGE_ID@PACKAGE_VERSION` package coordinate.
pub fn parse_package(value: &str) -> Result<PackageCoordinate, String> {
    let (package_id, package_version) = value
        .rsplit_once('@')
        .ok_or_else(|| "expected PACKAGE_ID@PACKAGE_VERSION".to_owned())?;
    PackageCoordinate::new(package_id, package_version).map_err(|error| error.to_string())
}

impl PublishReleaseRequest {
    fn scope(&self) -> ReleaseScope {
        ReleaseScope::new(self.env_key().as_str(), self.environment.as_str())
    }

    fn verified_run_schema(&self) -> anyhow::Result<BareSchemaName> {
        BareSchemaName::new(self.run_schema.clone())
            .map_err(|error| anyhow::anyhow!("invalid --run-schema {:?}: {error}", self.run_schema))
    }
}

/// Publish one effective release from the package artifacts in
/// `registry`: push its manifest to `release_artifact_base` with the
/// credential and trust roots of `registry` (§8.1), and return its manifest
/// digest. It writes OCI only.
pub async fn publish_release(
    request: PublishReleaseRequest,
    registry: &PackageRegistry,
    release_artifact_base: &str,
) -> anyhow::Result<ManifestDigest> {
    Ok(publish_candidate(&request, registry, release_artifact_base)
        .await?
        .digest)
}

/// Assemble a candidate only in a provisioned disposable target, without publication.
///
/// The packages are the directories `package_roots`, and the components their
/// in-process admissions: the dev loop pushes nothing and reads no registry.
pub async fn publish_local(
    args: PublishReleaseRequest,
    package_roots: &[PathBuf],
    admissions: &[crate::component_admission::ComponentAdmission],
) -> anyhow::Result<(
    PublishedRelease,
    wamn_runtime::local_application::LocalApplicationFacts,
)> {
    wamn_runtime::local_application::require_local_target(
        &args.database_url,
        &args.env_key(),
        &args.environment,
    )
    .await?;
    ensure!(
        read_projected_environment_disposable(&args.control_database_url, &args.env_key()).await?,
        "local candidates require a provisioned disposable environment"
    );
    let (client, connection) = tokio_postgres::connect(&args.database_url, NoTls).await?;
    let driver = tokio::spawn(connection);
    client
        .query_one(CLAIM_ENV_KEY_SQL, &[&args.env_key()])
        .await?;
    let assembled = assemble_local_release(&args, package_roots, admissions)?;
    let run_schema = args.verified_run_schema()?;
    let policy = crate::verification_policy::read_authoritative_environment_policy(
        &args.control_database_url,
        &args.org,
        &args.environment,
        false,
    )
    .await?;
    let projected =
        read_projected_environment_policy(&client, &run_schema, &args.env_key()).await?;
    verify_projected_environment_policy(projected.as_ref(), &policy, &args.scope(), &run_schema)?;
    drop(client);
    driver.abort();
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

    pub(crate) fn application(&self) -> &wamn_catalog::ServingEntryMember {
        &self.application
    }
}

/// Build an HTTP application from an admitted member and pinned platform route bytes.
///
/// The member can already contain its admitted bases. The pin must identify
/// the supplied route layer exactly.
pub fn compose_application(
    application: &crate::component_admission::ComponentAdmission,
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
        wamn_engine::component_admission::component_digest(route_bytes) == route_digest,
        "route layer bytes differ from pinned digest {route_digest}"
    );
    let bytes = wamn_component_composer::compose_application(
        route_bytes.to_vec(),
        application_bytes.to_vec(),
    )
    .context("compose the route layer with the admitted application")?;
    wamn_engine::statement_instances::statement_instances(&bytes)
        .context("validate composed application statement ownership")?;
    let digest = wamn_engine::component_admission::component_digest(&bytes);
    let application_digests = application_digests(application, application_bytes)?;
    Ok(ComposedApplication {
        bytes: bytes.into_boxed_slice(),
        digest,
        route_digest: route_digest.to_owned(),
        application: wamn_catalog::ServingEntryMember {
            package_id: application.scope.package_id.as_str().to_owned(),
            component: application.component.as_str().to_owned(),
            digest: application.component_digest.as_str().to_owned(),
        },
        application_digests,
    })
}

/// The digests of an admitted application and of the members it embeds.
fn application_digests(
    application: &AdmittedComponent,
    application_bytes: &[u8],
) -> anyhow::Result<BTreeSet<String>> {
    let mut digests = BTreeSet::from([application.component_digest.as_str().to_owned()]);
    for payload in wasmparser::Parser::new(0).parse_all(application_bytes) {
        if let wasmparser::Payload::ComponentSection {
            unchecked_range, ..
        } = payload?
        {
            let member = &application_bytes
                [usize::try_from(unchecked_range.start)?..usize::try_from(unchecked_range.end)?];
            digests.insert(wamn_engine::component_admission::component_digest(member));
        }
    }
    Ok(digests)
}

/// Whether one application, with the members it embeds, serves every release route.
///
/// Publication requires exactly one declared application member to pass this test.
fn covers_routes(
    published: &PublishedRelease,
    application: &AdmittedComponent,
    application_bytes: &[u8],
) -> anyhow::Result<bool> {
    let digests = application_digests(application, application_bytes)?;
    Ok(published.manifest.routes.iter().all(|route| {
        published.manifest.components.iter().any(|component| {
            component.package_id == route.package_id
                && component.descriptor.component == route.component
                && digests.contains(component.descriptor.component_digest.as_str())
        })
    }))
}

/// Record the exact composed entry before publication hashes the release.
///
/// The record names the released application components embedded in `entry`.
/// The manifest retains their original digests for SQL and dependency authority.
pub fn record_composed_entry(
    published: &mut PublishedRelease,
    entry: &ComposedApplication,
    reference: &str,
) -> anyhow::Result<()> {
    let members: BTreeSet<_> = published
        .manifest
        .components
        .iter()
        .filter(|component| {
            entry
                .application_digests
                .contains(&component.descriptor.component_digest)
        })
        .map(|component| wamn_catalog::ServingEntryMember {
            package_id: component.package_id.clone(),
            component: component.descriptor.component.clone(),
            digest: component.descriptor.component_digest.clone(),
        })
        .collect();
    ensure!(
        members.contains(&entry.application),
        "composed application is outside the released members"
    );
    let mut manifest = published.manifest.clone();
    manifest.composed_entry = Some(wamn_catalog::ServingComposedEntry {
        reference: reference.to_owned(),
        route_digest: entry.route_digest.clone(),
        application: entry.application.clone(),
        members,
    });
    let canonical_bytes = manifest.canonical_bytes();
    let (manifest, digest) = ServingManifest::from_canonical_bytes(&canonical_bytes)?;
    *published = PublishedRelease {
        manifest,
        digest,
        canonical_bytes,
    };
    Ok(())
}

/// Compose the routed application before its artifact reference is recorded.
pub fn compose_release_entry(
    published: &PublishedRelease,
    admissions: &[crate::component_admission::ComponentAdmission],
    route_bytes: &[u8],
    applications: &wamn_catalog::EnvironmentApplications,
    application: Option<&str>,
) -> anyhow::Result<ComposedApplication> {
    compose_covering_entry(
        published,
        admissions
            .iter()
            .map(|admission| (admission.facts(), admission.bytes()))
            .collect(),
        route_bytes,
        applications,
        application,
    )
}

/// Compose the unique member of the declared application that covers its routes.
pub fn compose_covering_entry(
    published: &PublishedRelease,
    candidates: Vec<(&AdmittedComponent, &[u8])>,
    route_bytes: &[u8],
    applications: &wamn_catalog::EnvironmentApplications,
    application: Option<&str>,
) -> anyhow::Result<ComposedApplication> {
    let (member, bytes) = covering_application(published, candidates, applications, application)?;
    compose_pinned_application(member, bytes, route_bytes)
}

pub(crate) fn compose_pinned_application(
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

fn covering_application<'a>(
    published: &PublishedRelease,
    candidates: Vec<(&'a AdmittedComponent, &'a [u8])>,
    applications: &wamn_catalog::EnvironmentApplications,
    application: Option<&str>,
) -> anyhow::Result<(&'a AdmittedComponent, &'a [u8])> {
    let selected = match application {
        Some(name) => applications.get_key_value(name),
        None if applications.len() == 1 => applications.first_key_value(),
        None => None,
    };
    let (name, definition) = selected.ok_or_else(|| {
        PublishManifestError::new(
            PublishManifestErrorType::ApplicationSelection,
            format!(
                "select one application from environment.k: {}; requested {:?}",
                applications.keys().cloned().collect::<Vec<_>>().join(", "),
                application
            ),
        )
    })?;
    let mut covering = Vec::new();
    for (member, bytes) in candidates {
        if definition
            .packages
            .contains(member.scope.package_id.as_str())
            && published.manifest.components.iter().any(|released| {
                released.package_id == member.scope.package_id
                    && released.descriptor.component == member.component
                    && released.descriptor.component_digest == member.component_digest
            })
            && covers_routes(published, member, bytes)?
        {
            covering.push((member, bytes));
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
                    .map(|(member, _)| format!("{}::{}", member.scope.package_id, member.component))
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
/// its admission. It reads no database, so the two database URLs of `args` go
/// unused.
pub fn assemble_local_release(
    args: &PublishReleaseRequest,
    package_roots: &[PathBuf],
    admissions: &[crate::component_admission::ComponentAdmission],
) -> anyhow::Result<AssembledLocalRelease> {
    use wamn_runtime::local_application::LocalApplicationFacts;
    let (attachment_paths, manifest_paths) = package_files(package_roots);
    let authored = read_package_attachments(&attachment_paths, &manifest_paths)?;
    let (package_manifests, route_contracts) = read_package_manifests(&manifest_paths)?;
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
        verified_publisher_principal: &args.verified_publisher_principal,
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
            fact.scope.env_key == args.env_key()
                && packages
                    .iter()
                    .any(|package| package.package_id() == fact.scope.package_id
                        && package.package_version() == fact.scope.package_version),
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
    let components = admissions
        .iter()
        .map(super::component_admission::ComponentAdmission::facts)
        .filter(|fact| {
            manifest.components.iter().any(|component| {
                component.descriptor.component_digest.as_str() == fact.component_digest
            })
        })
        .cloned()
        .collect();
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
) -> anyhow::Result<PublishedRelease> {
    ensure!(
        !args.environment.is_empty(),
        "environment must not be empty"
    );
    ensure!(
        !args.verified_publisher_principal.is_empty(),
        "verified-publisher-principal must not be empty"
    );
    // Every package opens from its artifact before anything is written, so a
    // package the registry does not hold refuses here.
    let mut opened = Vec::new();
    for package in &args.packages {
        opened.push(
            crate::package_artifact::open_package(
                registry,
                package.package_id(),
                package.package_version(),
            )
            .await
            .with_context(|| {
                format!(
                    "open the package artifact of {}@{}",
                    package.package_id(),
                    package.package_version()
                )
            })?,
        );
    }
    let release_components = descriptor_components(&args.env_key(), &args.packages, &opened)?;
    let roots = opened
        .iter()
        .map(|package| package.root().to_owned())
        .collect::<Vec<_>>();
    let (attachment_paths, manifest_paths) = package_files(&roots);
    let authored_attachments = read_package_attachments(&attachment_paths, &manifest_paths)?;
    let (package_manifests, route_contracts) = read_package_manifests(&manifest_paths)?;
    // The installed check compares each package's artifact digest, as
    // apply-package recorded it, with the digest the registry records.
    let package_artifact_digests = args
        .packages
        .iter()
        .zip(&opened)
        .map(|(package, opened)| {
            (
                package.package_id().to_owned(),
                opened
                    .package_root()
                    .artifact_digest
                    .expect("a package artifact opens with its registry digest"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let attachments = resolve_route_methods(&authored_attachments, &route_contracts)?;
    let packages = args.packages.iter().cloned().collect::<BTreeSet<_>>();
    ensure!(
        packages.len() == args.packages.len(),
        "effective release repeats a package coordinate"
    );
    let mut package_ids = BTreeSet::new();
    ensure!(
        packages
            .iter()
            .all(|package| package_ids.insert(package.package_id())),
        "effective release names more than one version of a package"
    );
    for manifest in package_manifests.values() {
        let coordinate = PackageCoordinate::new(&manifest.package.id, &manifest.package.version)
            .context("package manifest carries an invalid coordinate")?;
        ensure!(
            packages.contains(&coordinate),
            "package manifest {}@{} is outside the effective release membership",
            manifest.package.id,
            manifest.package.version,
        );
    }
    ensure!(
        package_manifests.len() == packages.len()
            && packages
                .iter()
                .all(|package| package_manifests.contains_key(package.package_id())),
        "publish-release requires one exact package manifest for every release package"
    );
    let environment_is_disposable =
        read_projected_environment_disposable(&args.control_database_url, &args.env_key())
            .await
            .context("resolve the release target's disposable marker")?;
    let request = PublishReleaseManifest {
        org: &args.org,
        project: &args.project,
        environment: &args.environment,
        verified_publisher_principal: &args.verified_publisher_principal,
        packages: &packages,
        attachments: &attachments,
        environment_is_disposable,
    };
    let run_schema = args.verified_run_schema()?;
    let source_policy = crate::verification_policy::read_authoritative_environment_policy(
        &args.control_database_url,
        &args.org,
        &args.environment,
        false,
    )
    .await
    .context("read the authoritative environment policy before release publish")?;

    let (client, connection) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect to the release project environment")?;
    let connection_task = tokio::spawn(connection);
    let published = read_and_push(
        &client,
        &request,
        &release_components,
        &package_manifests,
        &package_artifact_digests,
        &route_contracts,
        &run_schema,
        &source_policy,
        registry,
        release_artifact_base,
        args,
    )
    .await;
    match published {
        Ok(published) => {
            drop(client);
            connection_task
                .await
                .context("join the release publish connection")?
                .context("drive the release publish connection")?;
            Ok(published)
        }
        Err(error) => {
            connection_task.abort();
            Err(error)
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "each input is one independent fact of the release"
)]
async fn read_and_push(
    client: &Client,
    request: &PublishReleaseManifest<'_>,
    components: &ReleaseComponents,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    package_artifact_digests: &BTreeMap<String, String>,
    route_contracts: &RouteContracts,
    run_schema: &BareSchemaName,
    source_policy: &AuthoritativeEnvironmentPolicy,
    registry: &PackageRegistry,
    release_artifact_base: &str,
    args: &PublishReleaseRequest,
) -> anyhow::Result<PublishedRelease> {
    let published = publish_release_with_package_manifests(
        client,
        request,
        components,
        package_manifests,
        package_artifact_digests,
        route_contracts,
    )
    .await?;
    let projected =
        read_projected_environment_policy(client, run_schema, &request.env_key()).await?;
    verify_projected_environment_policy(
        projected.as_ref(),
        source_policy,
        &ReleaseScope::new(request.env_key(), request.environment),
        run_schema,
    )?;
    let mut published = published;
    entry_publication::publish(
        &mut published,
        components,
        registry,
        &args.applications,
        args.application.as_deref(),
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

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ProjectedEnvironmentPolicy {
    expected_environment: String,
    source_policy_org: Option<String>,
    source_policy_hash: Option<String>,
}

pub(crate) async fn read_projected_environment_policy(
    client: &Client,
    run_schema: &BareSchemaName,
    env_key: &str,
) -> Result<Option<ProjectedEnvironmentPolicy>, PublishManifestError> {
    client
        .query_opt(&projected_environment_policy_sql(run_schema), &[&env_key])
        .await
        .map(|row| {
            row.map(|row| ProjectedEnvironmentPolicy {
                expected_environment: row.get(0),
                source_policy_org: row.get(1),
                source_policy_hash: row.get(2),
            })
        })
        .map_err(|error| storage("read the provisioned environment policy", error))
}

pub(crate) fn verify_projected_environment_policy(
    projected: Option<&ProjectedEnvironmentPolicy>,
    source_policy: &AuthoritativeEnvironmentPolicy,
    scope: &ReleaseScope,
    run_schema: &BareSchemaName,
) -> Result<(), PublishManifestError> {
    let Some(projected) = projected else {
        return Err(environment_policy_absent(scope, run_schema));
    };
    verify_environment_name(&projected.expected_environment, scope)?;
    if projected.source_policy_org.as_deref() != Some(source_policy.source_policy_org.as_ref())
        || projected.source_policy_hash.as_deref()
            != Some(source_policy.source_policy_hash.as_ref())
    {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::EnvironmentPolicySourceMismatch,
            format!(
                "environment {:?} policy source differs: projected-org={:?}, authoritative-org={:?}, projected-hash={:?}, authoritative-hash={:?}; rerun the verification policy projection",
                scope.environment,
                projected.source_policy_org.as_deref().unwrap_or("<absent>"),
                source_policy.source_policy_org,
                projected
                    .source_policy_hash
                    .as_deref()
                    .unwrap_or("<absent>"),
                source_policy.source_policy_hash,
            ),
        ));
    }
    Ok(())
}

fn environment_policy_absent(
    scope: &ReleaseScope,
    run_schema: &BareSchemaName,
) -> PublishManifestError {
    PublishManifestError::new(
        PublishManifestErrorType::EnvironmentPolicyAbsent,
        format!(
            "tenant {:?} has no row in {}.environment_policies; run `wamn-ctl env apply`",
            scope.env_key,
            run_schema.as_str()
        ),
    )
}

fn verify_environment_name(
    expected_environment: &str,
    scope: &ReleaseScope,
) -> Result<(), PublishManifestError> {
    if expected_environment != scope.environment {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::EnvironmentPolicyMismatch,
            format!(
                "release environment {:?} differs from provisioned environment {:?}",
                scope.environment, expected_environment
            ),
        ));
    }
    Ok(())
}

/// Whether provisioning marked this tenant's environment disposable.
///
/// The same projection the admit path reads (wamn-10yt.38), from the same
/// control store, read ONCE per release publish rather than once per fact. An
/// absent relation is a stale store and says so rather than defaulting
/// quietly. A tenant with no projected row
/// is DURABLE, which is what keeps the change additive.
const SELECT_ENVIRONMENT_DISPOSABLE_SQL: &str = "SELECT coalesce((\
         SELECT disposable FROM catalog.tenant_environments WHERE env_key = $1\
     ), false)";

async fn read_projected_environment_disposable(
    control_database_url: &str,
    env_key: &str,
) -> anyhow::Result<bool> {
    on_control_plane(control_database_url, async |control| {
        let row = control
            .query_one(SELECT_ENVIRONMENT_DISPOSABLE_SQL, &[&env_key])
            .await
            .context("read the projected environment's disposable marker")?;
        Ok(row.get(0))
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
    let (mut control, connection) = tokio_postgres::connect(control_database_url, NoTls)
        .await
        .context("connect to the control database")?;
    let connection_task = tokio::spawn(connection);
    let outcome = write(&mut control).await;
    drop(control);
    match outcome {
        Ok(outcome) => {
            connection_task
                .await
                .context("join the control-plane connection")?
                .context("drive the control-plane connection")?;
            Ok(outcome)
        }
        Err(error) => {
            connection_task.abort();
            Err(error)
        }
    }
}

#[cfg(test)]
fn sha256(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
    )
}

async fn publish_release_with_package_manifests(
    client: &Client,
    request: &PublishReleaseManifest<'_>,
    components: &ReleaseComponents,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    package_artifact_digests: &BTreeMap<String, String>,
    route_contracts: &RouteContracts,
) -> Result<PublishedRelease, PublishManifestError> {
    publish_release_from_sources(
        client,
        request,
        components,
        package_manifests,
        package_artifact_digests,
        route_contracts,
    )
    .await
}

async fn publish_release_from_sources(
    client: &Client,
    request: &PublishReleaseManifest<'_>,
    component_facts: &ReleaseComponents,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    package_artifact_digests: &BTreeMap<String, String>,
    route_contracts: &RouteContracts,
) -> Result<PublishedRelease, PublishManifestError> {
    client
        .query_one(CLAIM_ENV_KEY_SQL, &[&request.env_key()])
        .await
        .map_err(|error| storage("claim the release tenant", error))?;
    validate_request(request)?;
    validate_release_package_manifests(
        client,
        request,
        package_manifests,
        package_artifact_digests,
    )
    .await?;

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

fn validate_request(request: &PublishReleaseManifest<'_>) -> Result<(), PublishManifestError> {
    if request.environment.is_empty() || request.verified_publisher_principal.is_empty() {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::Release,
            "environment and publisher principal are required",
        ));
    }
    if request.packages.is_empty() {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::Release,
            "an effective release requires at least one exact package pair",
        ));
    }
    let mut ids = BTreeSet::new();
    if !request
        .packages
        .iter()
        .all(|package| ids.insert(package.package_id()))
    {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::Release,
            "an effective release cannot contain two versions of one package",
        ));
    }
    validate_attachment_definition_hashes(request.attachments)?;
    Ok(())
}

/// Bind the release's behavior and ownership inputs to the exact package
/// artifact already installed by `apply-package`: the artifact digest it
/// recorded equals the digest the registry records.
async fn validate_release_package_manifests(
    client: &Client,
    request: &PublishReleaseManifest<'_>,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    package_artifact_digests: &BTreeMap<String, String>,
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
    let digested_package_ids = package_artifact_digests
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if presented_package_ids != expected_package_ids || digested_package_ids != expected_package_ids
    {
        return Err(PublishManifestError::new(
            PublishManifestErrorType::PackageManifest,
            format!(
                "release package manifests must exactly match membership; expected={expected_package_ids:?}, presented={presented_package_ids:?}, digested={digested_package_ids:?}; supply one exact wamn.json per release package"
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
                        "package {coordinate} has no presented manifest; supply the exact wamn.json applied by apply-package"
                    ),
                )
            })?;
        if manifest.package.id != package.package_id()
            || manifest.package.version != package.package_version()
        {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::PackageManifest,
                format!(
                    "package {coordinate} is paired with manifest {}@{}; supply the exact wamn.json applied by apply-package",
                    manifest.package.id, manifest.package.version
                ),
            ));
        }
        let presented_digest = package_artifact_digests
            .get(package.package_id())
            .ok_or_else(|| {
                PublishManifestError::new(
                    PublishManifestErrorType::PackageManifest,
                    format!("package {coordinate} has no package artifact digest"),
                )
            })?;
        let Some(row) = client
            .query_opt(
                SELECT_APPLIED_PACKAGE_DIGEST_SQL,
                &[
                    &request.env_key(),
                    &package.package_id(),
                    &package.package_version(),
                ],
            )
            .await
            .map_err(|error| storage("read the applied package artifact digest", error))?
        else {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::PackageManifest,
                format!(
                    "package {coordinate} is not applied; run apply-package against the target project environment"
                ),
            ));
        };
        let Some(applied_digest) = row.get::<_, Option<String>>(0) else {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::PackageManifest,
                format!(
                    "{}: package {coordinate} was applied by wamn dev from a working tree; apply the verified artifact to a release environment",
                    wamn_schema_control::PACKAGE_COORDINATE_WITHOUT_ARTIFACT_REFUSAL
                ),
            ));
        };
        if applied_digest != *presented_digest {
            return Err(PublishManifestError::new(
                PublishManifestErrorType::PackageManifest,
                format!(
                    "package {coordinate} has registry artifact digest {presented_digest}, but apply-package installed {applied_digest}; apply the verified artifact"
                ),
            ));
        }
    }
    Ok(())
}

/// Derive the one schema that an application's SQL resolves against.
pub(crate) fn application_runtime_schema(
    manifest: &wamn_schema_generator::PackageManifest,
    manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    components: &BTreeSet<ServingComponent>,
) -> anyhow::Result<Option<String>> {
    let member_digests = components
        .iter()
        .filter(|component| component.package_id == manifest.package.id)
        .flat_map(|component| component.descriptor.statement_instances.values())
        .collect::<BTreeSet<_>>();
    let mut members = vec![manifest];
    for dependency in manifest.base_dependencies.values() {
        let composed = member_digests.contains(&dependency.digest)
            || components.iter().any(|component| {
                component.package_id == dependency.package
                    && member_digests.contains(&component.descriptor.component_digest)
            });
        if composed {
            members.push(
                manifests
                    .get(&dependency.package)
                    .with_context(|| format!("missing base manifest {}", dependency.package))?,
            );
        }
    }
    let schemas = members
        .into_iter()
        .flat_map(|manifest| {
            manifest
                .models
                .values()
                .map(|model| model.schema.as_str())
                .chain(
                    manifest
                        .internal_relations
                        .values()
                        .map(|relation| relation.schema.as_str()),
                )
                .chain(manifest.custom_operations.values().flat_map(|operation| {
                    operation
                        .relations
                        .iter()
                        .map(|relation| relation.schema.as_str())
                }))
        })
        .collect::<BTreeSet<_>>();
    match schemas.into_iter().collect::<Vec<_>>().as_slice() {
        [] if manifest.declares_no_sql() => Ok(None),
        [schema] => Ok(Some((*schema).to_owned())),
        _ => anyhow::bail!(
            "one runtime schema is required for package {}",
            manifest.package.id
        ),
    }
}

/// Select applications whose admitted component includes the integration base.
pub(crate) fn composes_integration(
    manifest: &wamn_schema_generator::PackageManifest,
    components: &BTreeSet<ServingComponent>,
) -> bool {
    manifest.base_dependencies.values().any(|dependency| {
        dependency.package == "wamn_integration"
            && components.iter().any(|application| {
                application.package_id == manifest.package.id
                    && application
                        .descriptor
                        .statement_instances
                        .values()
                        .any(|digest| {
                            digest == &dependency.digest
                                || components.iter().any(|base| {
                                    base.package_id == dependency.package
                                        && &base.descriptor.component_digest == digest
                                })
                        })
            })
    })
}

/// Resolve the declared dependency permissions of an integration application.
/// Publication resolves them against the released package manifests.
pub(crate) fn application_dependency_permissions(
    manifest: &wamn_schema_generator::PackageManifest,
    manifests: &BTreeMap<&str, &wamn_schema_generator::PackageManifest>,
) -> anyhow::Result<BTreeSet<String>> {
    let mut permissions = BTreeSet::new();
    for dependency in manifest.base_dependencies.values() {
        for operation in &dependency.operations {
            let base = manifests
                .get(dependency.package.as_str())
                .with_context(|| format!("missing dependency manifest {}", dependency.package))?;
            ensure!(
                base.package.version == dependency.version,
                "dependency {} version differs from the release",
                dependency.package
            );
            let registered = base
                .custom_operations
                .get(operation)
                .map(|operation| operation.permission().is_some())
                .or_else(|| {
                    base.models
                        .values()
                        .flat_map(|model| model.operations.values())
                        .find(|declared| declared.permission == *operation)
                        .map(|_| true)
                })
                .with_context(|| {
                    format!(
                        "declared dependency {}:{operation} is absent from the release",
                        dependency.package
                    )
                })?;
            if registered {
                let canonical =
                    wamn_schema_generator::canonical_operation_identity(&base.package, operation)?;
                permissions.insert(
                    canonical
                        .split_once('@')
                        .expect("canonical operation has a version")
                        .0
                        .to_owned(),
                );
            }
        }
    }
    Ok(permissions)
}

fn pin_runtime_schemas(
    components: BTreeSet<ServingComponent>,
    manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
) -> Result<BTreeSet<ServingComponent>, PublishManifestError> {
    let dependency_manifests = manifests
        .iter()
        .map(|(id, manifest)| (id.as_str(), manifest))
        .collect();
    let schemas = manifests
        .iter()
        .map(|(id, manifest)| {
            if composes_integration(manifest, &components) {
                application_dependency_permissions(manifest, &dependency_manifests)?;
            }
            application_runtime_schema(manifest, manifests, &components)
                .map(|schema| (id.clone(), schema))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()
        .map_err(|error| {
            PublishManifestError::new(PublishManifestErrorType::PackageManifest, error.to_string())
        })?;
    Ok(components
        .into_iter()
        .map(|mut component| {
            component.runtime_schema = schemas.get(&component.package_id).cloned().flatten();
            component
        })
        .collect())
}

/// The admitted components of the release packages: the facts of each package
/// by `(package_id, package_version)`, and the declared connections of each
/// component by `(package_id, component)`. Together they are the descriptors
/// the release carries.
#[derive(Debug, Default)]
struct ReleaseComponents {
    facts: BTreeMap<(String, String), Vec<AdmittedComponent>>,
    connections: BTreeMap<(String, String), Vec<ComponentConnection>>,
    /// The verified statement instance record of each composed component, by
    /// component digest.
    statement_instances: BTreeMap<String, BTreeMap<String, String>>,
}

impl ReleaseComponents {
    /// Add one admitted component and its connections. A package names each
    /// component once.
    fn insert(
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
                    (fact.scope.package_id.clone(), fact.component.clone()),
                    connections,
                )
                .is_none(),
            "package {}@{} names component {} twice",
            fact.scope.package_id,
            fact.scope.package_version,
            fact.component
        );
        self.facts
            .entry((
                fact.scope.package_id.clone(),
                fact.scope.package_version.clone(),
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
            .get(&(fact.scope.package_id.clone(), fact.component.clone()))
            .cloned()
            .ok_or_else(|| {
                PublishManifestError::new(
                    PublishManifestErrorType::Component,
                    format!(
                        "component {:?} of package {:?} has no descriptor",
                        fact.component, fact.scope.package_id
                    ),
                )
            })
    }
}

/// The admitted components of the opened package artifacts, each from its
/// descriptor layer, in the scope of `env_key`.
fn project_components(
    components: &ReleaseComponents,
) -> Result<BTreeSet<ServingComponent>, PublishManifestError> {
    components
        .facts
        .values()
        .flatten()
        .map(|fact| {
            components.with_statement_instances(project_serving_component(
                fact,
                components.connections(fact)?,
            )?)
        })
        .collect()
}

fn descriptor_components(
    env_key: &str,
    packages: &[PackageCoordinate],
    opened: &[OpenedPackage],
) -> anyhow::Result<ReleaseComponents> {
    let mut components = ReleaseComponents::default();
    for (package, opened) in packages.iter().zip(opened) {
        let scope =
            ComponentPackageScope::new(env_key, package.package_id(), package.package_version())
                .context("name the release component scope")?;
        for descriptor in crate::package_artifact::read_descriptors(opened.root())? {
            let facts = descriptor.into_admitted(scope.clone());
            wamn_catalog::verify_stored_effect_projection(&facts.component).with_context(|| {
                format!(
                    "component {:?} of package {}@{} carries an invalid effect projection",
                    facts.component.component,
                    package.package_id(),
                    package.package_version()
                )
            })?;
            crate::component_admission::check_admitted_statements(&facts.component).with_context(
                || {
                    format!(
                        "component {:?} of package {}@{} carries refused SQL",
                        facts.component.component,
                        package.package_id(),
                        package.package_version()
                    )
                },
            )?;
            components.insert(
                facts.component,
                facts.connections,
                facts.statement_instances,
            )?;
        }
    }
    Ok(components)
}

/// The attachment documents and the `wamn.json` of the packages at `roots`. A
/// package with no `publication/attachments.json` has no attachment document.
fn package_files(roots: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let attachments = roots
        .iter()
        .map(|root| root.join("publication/attachments.json"))
        .filter(|path| path.is_file())
        .collect();
    let manifests = roots
        .iter()
        .map(|root| wamn_schema_generator::package_manifest_path(root))
        .collect();
    (attachments, manifests)
}

/// Project operation contracts for released components and route attachments.
///
/// The kind comes from the generated contract of the operation, or from the
/// published manifest a promotion copies.
fn project_routes(
    request: &PublishReleaseManifest<'_>,
    route_contracts: &RouteContracts,
    component_facts: &ReleaseComponents,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    components: &mut BTreeSet<ServingComponent>,
) -> Result<BTreeSet<ServingRoute>, PublishManifestError> {
    let rule = DependencyDigestRule::for_environment(request.environment_is_disposable);
    let mut routes = BTreeSet::new();
    for (attachment_id, attachment) in request.attachments {
        let AttachmentTarget::Route {
            component,
            operation,
        } = &attachment.target;
        let package = request
            .packages
            .iter()
            .find(|package| package.package_id() == attachment.package_id)
            .ok_or_else(|| {
                PublishManifestError::new(
                    PublishManifestErrorType::Component,
                    format!(
                        "route attachment {attachment_id:?} names package {:?} outside the effective release membership",
                        attachment.package_id
                    ),
                )
            })?;
        let facts = component_facts
            .facts
            .get(&(
                package.package_id().to_owned(),
                package.package_version().to_owned(),
            ))
            .map_or(&[][..], Vec::as_slice);
        let fact = resolve_route_component(attachment_id, attachment, component, operation, facts)?;
        let contract = route_contracts
            .get(&(attachment.package_id.clone(), operation.clone()))
            .ok_or_else(|| {
                PublishManifestError::new(
                    PublishManifestErrorType::PackageManifest,
                    format!(
                        "route operation {operation:?} of package {:?} has no generated contract kind; regenerate the package evidence",
                        attachment.package_id
                    ),
                )
            })?;
        let roots = BTreeMap::from([(operation.clone(), fact.clone())]);
        resolve_component_dependency_closure(
            &roots,
            &component_facts.facts,
            package_manifests,
            rule,
        )?;
        components.insert(
            component_facts.with_statement_instances(project_serving_component(
                fact,
                component_facts.connections(fact)?,
            )?)?,
        );
        routes.insert(ServingRoute {
            package_id: attachment.package_id.clone(),
            component: component.clone(),
            operation: operation.clone(),
            type_: contract.kind,
            reads: contract.reads.clone(),
            revision: contract.revision.clone(),
            idempotency: contract.idempotency.clone(),
            canonicalization: contract.canonicalization.clone(),
        });
    }
    // Workflow nodes also use these contracts to begin item transactions and
    // claim their keys, for private and registered commands alike. An HTTP
    // attachment is not required to call an operation.
    for component in components.iter() {
        for operation in component.operations.keys() {
            let Some(contract) =
                route_contracts.get(&(component.package_id.clone(), operation.clone()))
            else {
                continue;
            };
            if contract.kind == wamn_catalog::OperationType::EventHandler {
                continue;
            }
            routes.insert(ServingRoute {
                package_id: component.package_id.clone(),
                component: component.descriptor.component.clone(),
                operation: operation.clone(),
                type_: contract.kind,
                reads: contract.reads.clone(),
                revision: contract.revision.clone(),
                idempotency: contract.idempotency.clone(),
                canonicalization: contract.canonicalization.clone(),
            });
        }
    }
    Ok(routes)
}

/// Read the canonical bytes of an installed release.
pub async fn read_release(
    transaction: &Transaction<'_>,
    env_key: &str,
    manifest_digest: &str,
) -> Result<Option<Vec<u8>>, PublishManifestError> {
    transaction
        .query_one(CLAIM_ENV_KEY_SQL, &[&env_key])
        .await
        .map_err(|error| storage("claim the release tenant", error))?;
    transaction
        .query_opt(READ_RELEASE_SQL, &[&env_key, &manifest_digest])
        .await
        .map(|row| row.map(|row| row.get(0)))
        .map_err(|error| storage("read the release", error))
}

fn storage(context: &'static str, error: tokio_postgres::Error) -> PublishManifestError {
    PublishManifestError::with_source(PublishManifestErrorType::Storage, context, error)
}

#[cfg(test)]
mod effective_release_live;

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_catalog::AdmittedComponentOperation;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn refresh_definition_hash(attachment: &mut ServingAttachment) {
        attachment.definition_hash = wamn_catalog::DefinitionHash::parse(
            wamn_execution_contract::canonical_json_sha256(&attachment.definition),
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
            "kind": "http",
            "route": {"path": path},
        });
        let definition_hash = wamn_execution_contract::canonical_json_sha256(&definition);
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

    #[tokio::test]
    async fn two_applications_with_different_model_schemas_publish_and_pass_apply_analysis() {
        use crate::environment::analyse::{Authorities, Policy, ProjectState, ReleaseFacts};
        let _lock = wamn_test_postgres::lock();
        let database = wamn_catalog::test_database::tenant();
        let (client, connection) = tokio_postgres::connect(database.url(), NoTls)
            .await
            .unwrap();
        tokio::spawn(connection);
        client
            .batch_execute(&format!(
                "SELECT set_config('app.user_id', '{}', false)",
                wamn_control_provision::PlatformComponent::Provisioning.principal_id()
            ))
            .await
            .unwrap();
        let mut manifests = BTreeMap::new();
        let mut components = ReleaseComponents::default();
        let mut packages = BTreeSet::new();
        let mut digests = BTreeMap::new();
        for (package, schema, digest_byte) in [
            ("consumer", "consumer", 'a'),
            ("integration", "integration", 'b'),
        ] {
            let mut manifest = source_manifest();
            manifest.package.id = package.to_owned();
            for model in manifest.models.values_mut() {
                model.schema = schema.to_owned();
                model.owner = package.to_owned();
            }
            let mut component = closure_component("application", None);
            component.scope.package_id = package.to_owned();
            component.component_digest = format!("sha256:{}", digest_byte.to_string().repeat(64));
            let operation = format!("{package}:job/run@1.0.0");
            let mut export = component.operations.remove("run").unwrap();
            for (ports, name) in [
                (&mut export.input_ports, "input"),
                (&mut export.output_ports, "main"),
            ] {
                ports.push(wamn_catalog::AdmittedComponentPort {
                    name: name.to_owned(),
                    schema: wamn_catalog::ComponentSchema {
                        schema: serde_json::json!({}),
                    },
                });
            }
            component.operations.insert(operation.clone(), export);
            client.execute("INSERT INTO catalog.packages (env_key,package_id,package_version,artifact_digest) VALUES ('tenant-a--fixture--dev',$1,'1.0.0',$2)", &[&package, &component.component_digest]).await.unwrap();
            digests.insert(package.to_owned(), component.component_digest.clone());
            components
                .insert(component, Vec::new(), BTreeMap::new())
                .unwrap();
            packages.insert(PackageCoordinate::new(package, "1.0.0").unwrap());
            manifests.insert(package.to_owned(), manifest);
        }
        let published = publish_release_with_package_manifests(
            &client,
            &PublishReleaseManifest {
                org: "tenant-a",
                project: "fixture",
                environment: "dev",
                verified_publisher_principal: "publisher",
                packages: &packages,
                attachments: &BTreeMap::new(),
                environment_is_disposable: false,
            },
            &components,
            &manifests,
            &digests,
            &RouteContracts::new(),
        )
        .await
        .unwrap();
        assert_eq!(published.manifest.components.len(), 2);
        let document = crate::environment::document::EnvironmentDocument::from_json(
            serde_json::json!({"org":"acme", "project":"erp", "env":"dev", "release":published.digest.as_str(),
                "target": "cloud", "route_host":"erp.acme.example", "policy":"dev",
                "applications": {"erp": {"packages": ["consumer", "integration"]}}}).to_string().as_bytes()
        ).unwrap();
        let authorities = Authorities {
            release_name: "acme--erp--dev".to_owned(),
            connections: BTreeMap::new(),
            policy: Policy {
                name: "dev".to_owned(),
                readiness_budget_seconds: 600,
                drain_bound_seconds: 300,
            },
            row: None,
            project: Some(ProjectState::default()),
            release: Some(ReleaseFacts {
                digest: published.digest.to_string(),
                manifest: published.manifest,
                image_set: crate::release_chart::ImageSet {
                    host: DIGEST.to_owned(),
                    http: DIGEST.to_owned(),
                    materializer: DIGEST.to_owned(),
                },
                qualification: "fixture".to_owned(),
                required_connections: BTreeSet::new(),
            }),
            revision: None,
            live: BTreeMap::new(),
            floor_artifacts: Some(BTreeMap::new()),
        };
        assert!(crate::environment::analyse::refusals(&document, &authorities).is_empty());
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
    fn release_publish_binds_each_attachment_hash_to_its_definition() {
        let definition = serde_json::json!({
            "id": "fixture-http",
            "kind": "http",
            "route": {"path": "/widget/get"},
        });
        let definition_hash = wamn_execution_contract::canonical_json_sha256(&definition);
        let attachment = ServingAttachment {
            type_: wamn_catalog::AttachmentType::Http,
            package_id: "source_fixture".to_owned(),
            target: wamn_catalog::AttachmentTarget::Route {
                component: "widget".to_owned(),
                operation: "source-fixture:widget/get@1.0.0".to_owned(),
            },
            definition_hash: wamn_catalog::DefinitionHash::parse(definition_hash)
                .expect("the canonicalizer emits a valid definition hash"),
            definition,
            auth_policy: serde_json::json!({"modes": ["pat"]}),
            registered_operation: Some("source-fixture:widget/get@1.0.0".to_owned()),
        };
        let attachments = BTreeMap::from([("fixture-http".to_owned(), attachment.clone())]);
        validate_attachment_definition_hashes(&attachments)
            .expect("the exact canonical definition matches its authored hash");

        let mut changed = attachment;
        changed.definition["route"]["path"] = serde_json::json!("/widget/query");
        let error = validate_attachment_definition_hashes(&BTreeMap::from([(
            "fixture-http".to_owned(),
            changed,
        )]))
        .expect_err("changed definition bytes cannot retain the old identity");
        assert_eq!(error.error_type(), PublishManifestErrorType::Document);
    }

    #[test]
    fn release_publish_writes_no_route_host_and_refuses_an_authored_one() {
        let definition = serde_json::json!({
            "id": "fixture-http",
            "kind": "http",
            "route": {"path": "/widget/get"},
        });
        let authored_hash = wamn_execution_contract::canonical_json_sha256(&definition);
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
            wamn_execution_contract::canonical_json_sha256(&resolved["fixture-http"].definition)
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
            .expect_err("every attachment kind refuses an authored route hostname");
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
    fn release_publish_writes_the_method_from_the_operation_kind() {
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
        let kinds: RouteContracts = [
            ("widget/get", OperationType::Get),
            ("widget/query", OperationType::Query),
            ("widget/load", OperationType::Projection),
            ("widget/update", OperationType::Update),
            ("widget/record", OperationType::Command),
            ("widget/flow", OperationType::Command),
        ]
        .into_iter()
        .map(|(operation, kind)| {
            (
                (
                    "source_fixture".to_owned(),
                    format!("source-fixture:{operation}@1.0.0"),
                ),
                RouteContract::from(kind),
            )
        })
        .collect();
        let resolved =
            resolve_route_methods(&authored, &kinds).expect("every route resolves a method");
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
            .expect_err("a route to an operation with no contract kind refuses");
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
        let refusal = resolve_route_methods(&authored_method, &kinds)
            .expect_err("an authored method refuses");
        assert_eq!(refusal.error_type(), PublishManifestErrorType::Document);
        assert!(
            refusal.detail().contains("route.method"),
            "{}",
            refusal.detail()
        );
    }

    fn closure_component(component: &str, registered_operation: Option<&str>) -> AdmittedComponent {
        let operation = registered_operation.unwrap_or("run");
        AdmittedComponent {
            scope: ComponentPackageScope {
                env_key: "tenant-a--fixture--dev".to_owned(),
                package_id: "base".to_owned(),
                package_version: "1.0.0".to_owned(),
            },
            component: component.to_owned(),
            interface_version: "0.1.0".to_owned(),
            operations: BTreeMap::from([(
                operation.to_owned(),
                AdmittedComponentOperation {
                    committed_result_schema: None,
                    registered_operation: registered_operation.map(str::to_owned),
                    host_transaction: false,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
            component_digest: DIGEST.to_owned(),
            imports: Vec::new(),
            imports_fingerprint: DIGEST.to_owned(),
            effects: Vec::new(),
        }
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
                fact.scope.package_id = package.into();
                fact.component_digest = wamn_engine::component_admission::component_digest(bytes);
                fact
            })
            .collect();
        let manifest = ServingManifest::new(
            ServingRelease {
                packages: facts
                    .iter()
                    .map(|fact| PackageCoordinate::new(&fact.scope.package_id, "1.0.0").unwrap())
                    .collect(),
            },
            facts
                .iter()
                .map(|fact| ServingComponent::project(fact, Vec::new()).unwrap())
                .collect(),
            facts[..2]
                .iter()
                .map(|fact| ServingRoute {
                    package_id: fact.scope.package_id.clone(),
                    component: "entry".into(),
                    operation: "run".into(),
                    type_: OperationType::Get,
                    reads: BTreeSet::new(),
                    revision: None,
                    idempotency: None,
                    canonicalization: None,
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
            covering_application(&published, candidates(), &applications, None).unwrap();
        assert_eq!(member.scope.package_id, "overlay");
        assert_eq!(
            covering_application(&published, candidates(), &applications, Some("orders"))
                .unwrap()
                .0,
            member
        );
        let error =
            covering_application(&published, candidates()[..1].to_vec(), &applications, None)
                .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<PublishManifestError>()
                .unwrap()
                .error_type(),
            PublishManifestErrorType::ApplicationCoverage
        );

        let mut ambiguous = applications.clone();
        ambiguous
            .get_mut("orders")
            .unwrap()
            .packages
            .insert("outer".into());
        let error = covering_application(&published, candidates(), &ambiguous, None).unwrap_err();
        let error = error.downcast_ref::<PublishManifestError>().unwrap();
        assert_eq!(
            error.error_type(),
            PublishManifestErrorType::AmbiguousApplicationMember
        );
        assert!(error.detail().contains("overlay::entry"));
        assert!(error.detail().contains("outer::entry"));
    }

    /// A package directory whose `descriptors/` holds the canonical descriptor
    /// of `fact` with one connection, as an unpacked artifact holds it.
    fn unpacked_descriptor(label: &str, fact: &AdmittedComponent) -> (PathBuf, Vec<u8>) {
        let root = std::env::temp_dir().join(format!(
            "wamn-publish-descriptor-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(crate::package_artifact::DESCRIPTOR_DIRECTORY)).unwrap();
        let descriptor = wamn_catalog::ComponentDescriptor::new(
            fact.clone(),
            vec![ComponentConnection {
                store_alias: "labels".to_owned(),
                requirement_type: wamn_catalog::ComponentConnectionType::Blobstore,
            }],
        );
        let bytes = wamn_execution_contract::canonical_json_bytes(
            &serde_json::to_value(&descriptor).unwrap(),
        );
        std::fs::write(
            root.join(crate::package_artifact::DESCRIPTOR_DIRECTORY)
                .join(format!("{}.json", fact.component)),
            &bytes,
        )
        .unwrap();
        (root, bytes)
    }

    /// The release component carries the descriptor layer byte for byte, and
    /// two tenants read one component from one artifact.
    #[tokio::test]
    async fn a_release_component_carries_its_descriptor_layer_in_every_env_key() {
        let fact = closure_component("registered-component", Some("base:widget/get@1.0.0"));
        let (root, layer) = unpacked_descriptor("layer", &fact);
        let opened = crate::package_artifact::open_package_source(
            crate::package_artifact::PackageSource::Directory(root.clone()),
        )
        .await
        .unwrap();
        let packages = [PackageCoordinate::new("base", "1.0.0").unwrap()];
        let project = |env_key: &str| {
            let components =
                descriptor_components(env_key, &packages, std::slice::from_ref(&opened)).unwrap();
            let fact = &components.facts[&("base".to_owned(), "1.0.0".to_owned())][0];
            assert_eq!(fact.scope.env_key, env_key);
            project_serving_component(fact, components.connections(fact).unwrap()).unwrap()
        };
        let first = project("acme--plant--dev");
        let second = project("acme--plant--prod");
        assert_eq!(first, second);
        assert_eq!(
            wamn_execution_contract::canonical_json_bytes(
                &serde_json::to_value(&first.descriptor).unwrap()
            ),
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
            root.join(crate::package_artifact::DESCRIPTOR_DIRECTORY)
                .join("registered-component.json"),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
        let error = crate::package_artifact::read_descriptors(&root).unwrap_err();
        assert!(
            format!("{error:#}").contains("is not canonical JSON"),
            "{error:#}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn dependency_manifest(digest: &str) -> wamn_schema_generator::PackageManifest {
        let mut document: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/component_package/wamn.json"
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

    fn source_manifest() -> wamn_schema_generator::PackageManifest {
        serde_json::from_str(include_str!(
            "../tests/fixtures/component_package/wamn.json"
        ))
        .expect("the fixture source manifest parses")
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
            operations: BTreeMap::new(),
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
    ) -> wamn_schema_generator::PackageManifest {
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
        base.scope.package_id = "base".to_owned();
        let mut overlay = closure_component("overlay-component", Some(overlay_operation));
        overlay.scope.package_id = "overlay".to_owned();
        overlay.scope.package_version = "3.0.0".to_owned();
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
            projected.operations[overlay_operation].permissions,
            BTreeSet::from([overlay_operation.to_owned()]),
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
        base.scope.package_id = "base".to_owned();
        let mut overlay = closure_component("overlay-component", Some(overlay_operation));
        overlay.scope.package_id = "overlay".to_owned();
        overlay.scope.package_version = "3.0.0".to_owned();
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
    fn release_projection_preserves_operation_scoped_statement_facts() {
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
                    transactional: false,
                },
            );

        let serving = project_serving_component(&admitted, Vec::new())
            .expect("an admitted component projects to serving facts");

        assert_eq!(
            serving.operations[operation].statement(&path),
            admitted.operations[operation].statement(&path)
        );
    }

    #[test]
    fn environment_policy_refusals_remain_distinct() {
        let authoritative_hash = format!("sha256:{}", "a".repeat(64));
        let stale_hash = format!("sha256:{}", "b".repeat(64));
        let source_policy = AuthoritativeEnvironmentPolicy {
            source_policy_org: "demo".into(),
            policy: wamn_control_registry::EnvPolicy::prod(),
            source_policy_hash: authoritative_hash.clone().into_boxed_str(),
        };
        let release = ReleaseScope::new("tenant-a", "prod");
        let schema = BareSchemaName::new("wamn_run").unwrap();
        assert_eq!(
            verify_projected_environment_policy(None, &source_policy, &release, &schema)
                .unwrap_err()
                .error_type(),
            PublishManifestErrorType::EnvironmentPolicyAbsent
        );
        let wrong_environment = ProjectedEnvironmentPolicy {
            expected_environment: "dev".to_owned(),
            source_policy_org: Some("demo".to_owned()),
            source_policy_hash: Some(authoritative_hash.clone()),
        };
        assert_eq!(
            verify_projected_environment_policy(
                Some(&wrong_environment),
                &source_policy,
                &release,
                &schema,
            )
            .unwrap_err()
            .error_type(),
            PublishManifestErrorType::EnvironmentPolicyMismatch
        );
        let stale = ProjectedEnvironmentPolicy {
            expected_environment: "prod".to_owned(),
            source_policy_org: Some("demo".to_owned()),
            source_policy_hash: Some(stale_hash.clone()),
        };
        let mismatch =
            verify_projected_environment_policy(Some(&stale), &source_policy, &release, &schema)
                .unwrap_err();
        assert_eq!(
            mismatch.error_type(),
            PublishManifestErrorType::EnvironmentPolicySourceMismatch
        );
        assert!(mismatch.detail().contains("environment \"prod\""));
        assert!(mismatch.detail().contains(&stale_hash));
        assert!(mismatch.detail().contains(&authoritative_hash));

        let exact = ProjectedEnvironmentPolicy {
            expected_environment: "prod".to_owned(),
            source_policy_org: Some("demo".to_owned()),
            source_policy_hash: Some(authoritative_hash),
        };
        verify_projected_environment_policy(Some(&exact), &source_policy, &release, &schema)
            .unwrap();
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

    /// Push canonical format-1 bytes or check that their exact artifact already exists.
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

        const CANONICAL_MANIFEST: &[u8] = br#"{"attachments":{},"components":[{"descriptor":{"component":"http-request","component-digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","connections":[],"effects":[],"imports":[],"imports-fingerprint":"","interface-version":"0.1","operations":{}},"operations":{"wamn:node/handler@0.1.0":{}},"package-id":"orders"}],"format-version":11,"release":{"packages":[{"package-id":"orders","package-version":"1.0.0"}]},"routes":[]}"#;

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
        #[ignore = "requires: WAMN_RELEASE_MANIFEST_ARTIFACT_BASE, WAMN_REGISTRY_AUTH_FILE"]
        async fn pushing_same_bytes_uploads_nothing() {
            wamn_test_postgres::require_prerequisites(&[
                "WAMN_RELEASE_MANIFEST_ARTIFACT_BASE",
                "WAMN_REGISTRY_AUTH_FILE",
            ]);
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

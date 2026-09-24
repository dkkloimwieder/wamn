//! Mint one immutable format-1 effective-release closure.
//!
//! A release is an independent integer identity plus exact package membership.
//! The publisher resolves every wiring and component from those package pairs,
//! freezes the relational closure and canonical manifest in one transaction,
//! then projects only the release identity to the control plane so a later
//! deployment attestation can reference it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use serde::de::DeserializeOwned;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentEffect, AdmittedComponentOperation, AttachmentTarget,
    ComponentPackageScope, EffectiveReleaseId, ManifestDigest, OperationKind, PackageCoordinate,
    SERVING_MANIFEST_FORMAT_VERSION, ServingAttachment, ServingComponent, ServingManifest,
    ServingRegistration, ServingRelease, ServingRoute, ServingWiring, WiringDocument,
    validate_resolved_wiring_compatibility,
};
use wamn_control_registry::Triple;
use wamn_schema_control::{
    BareSchemaName, HttpRoute as AuthoredHttpRoute, canonical_http_route_template,
    normalize_http_route,
};

use crate::verification_policy::AuthoritativeEnvironmentPolicy;

mod attachments;
mod components;
mod package_sources;

use attachments::{
    read_package_attachments, resolve_route_host_overlay, validate_attachment_definition_hashes,
};
use components::{
    project_serving_component, resolve_component_dependency_closure, resolve_route_component,
    resolve_wiring_components, resolved_wiring_entry_operation, validate_anonymous_wiring_closure,
};
use package_sources::read_package_manifests;

pub use components::effect_free_operation_dependencies;

#[cfg(test)]
use attachments::merge_package_attachment_documents;
#[cfg(test)]
use package_sources::validate_package_metadata;

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";
const INSERT_RELEASE_SQL: &str = "\
INSERT INTO catalog.effective_releases (\
       tenant_id, effective_release_id, environment, verified_publisher_principal\
     ) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING";
const LOCK_RELEASE_SQL: &str = "\
SELECT environment, verified_publisher_principal \
  FROM catalog.effective_releases \
 WHERE tenant_id = $1 AND effective_release_id = $2 FOR UPDATE";
const INSERT_PACKAGE_SQL: &str = "\
INSERT INTO catalog.effective_release_packages (\
       tenant_id, effective_release_id, package_id, package_version\
     ) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING";
const SELECT_PACKAGES_SQL: &str = "\
SELECT package_id, package_version \
  FROM catalog.effective_release_packages \
 WHERE tenant_id = $1 AND effective_release_id = $2 \
 ORDER BY package_id COLLATE \"C\", package_version COLLATE \"C\" FOR SHARE";
const SELECT_APPLIED_PACKAGE_MANIFEST_SQL: &str = "\
SELECT manifest_sha256 FROM catalog.packages \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 FOR SHARE";
const SELECT_COMPONENT_FACTS_SQL: &str = "\
SELECT component, interface_version, operations::text, component_digest, \
       imports::text, imports_fingerprint, effects::text \
  FROM catalog.component_library \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
 ORDER BY component COLLATE \"C\", interface_version COLLATE \"C\" FOR SHARE";
const SELECT_WIRING_SQL: &str = "\
SELECT wiring_hash, graph_json::text \
  FROM catalog.wirings \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
   AND wiring_id = $4 AND version = $5 FOR SHARE";
const SELECT_RELEASE_COMPONENTS_SQL: &str = "\
SELECT wiring_package_id, wiring_package_version, wiring_id, wiring_version, node_id, \
       package_id, package_version, component_digest, route_component, route_operation \
  FROM catalog.release_components \
 WHERE tenant_id = $1 AND effective_release_id = $2 FOR SHARE";
const INSERT_RELEASE_COMPONENT_SQL: &str = "\
INSERT INTO catalog.release_components (\
       tenant_id, effective_release_id, wiring_package_id, wiring_package_version, \
       wiring_id, wiring_version, node_id, package_id, package_version, component_digest, \
       route_component, route_operation\
     ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)";
const SELECT_RELEASE_SNAPSHOT_SQL: &str = "\
SELECT manifest_digest, canonical_bytes \
  FROM catalog.release_manifest_v3_snapshots \
 WHERE tenant_id = $1 AND effective_release_id = $2 FOR SHARE";
const READ_RELEASE_SNAPSHOT_SQL: &str = "\
SELECT canonical_bytes FROM catalog.release_manifest_v3_snapshots \
 WHERE tenant_id = $1 AND effective_release_id = $2";
const INSERT_RELEASE_SNAPSHOT_SQL: &str = "\
INSERT INTO catalog.release_manifest_v3_snapshots (\
       tenant_id, effective_release_id, manifest_digest, canonical_bytes\
     ) VALUES ($1, $2, $3, $4)";

fn expected_environment_sql(run_schema: &BareSchemaName) -> String {
    format!(
        "SELECT expected_environment FROM {}.environment_policies WHERE tenant_id = $1",
        run_schema.quoted()
    )
}

fn projected_environment_policy_sql(run_schema: &BareSchemaName) -> String {
    format!(
        "SELECT expected_environment, source_policy_org, source_policy_hash \
           FROM {}.environment_policies WHERE tenant_id = $1",
        run_schema.quoted()
    )
}

/// One exact package-owned wiring included in an effective release.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseWiringTarget {
    pub package_id: String,
    pub package_version: String,
    pub wiring_id: String,
    pub wiring_version: u32,
}

impl std::str::FromStr for ReleaseWiringTarget {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (package, wiring) = value.split_once("::").ok_or_else(|| {
            "expected PACKAGE_ID@PACKAGE_VERSION::WIRING_ID=WIRING_VERSION".to_owned()
        })?;
        let (package_id, package_version) = package
            .rsplit_once('@')
            .ok_or_else(|| "wiring target requires an exact package version".to_owned())?;
        let (wiring_id, wiring_version) = wiring
            .split_once('=')
            .ok_or_else(|| "wiring target requires WIRING_ID=WIRING_VERSION".to_owned())?;
        let package = PackageCoordinate::new(package_id, package_version)
            .map_err(|error| error.to_string())?;
        ensure_token(wiring_id, "wiring id")?;
        let wiring_version = wiring_version
            .parse::<u32>()
            .map_err(|_| "wiring version must be a positive integer".to_owned())?;
        if wiring_version == 0 {
            return Err("wiring version must be greater than zero".to_owned());
        }
        Ok(Self {
            package_id: package.package_id().to_owned(),
            package_version: package.package_version().to_owned(),
            wiring_id: wiring_id.to_owned(),
            wiring_version,
        })
    }
}

fn ensure_token(value: &str, name: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(format!("{name} must be non-empty and free of whitespace"));
    }
    Ok(())
}

/// Inputs owned by the release publisher.
#[derive(Debug)]
pub struct MintReleaseManifest<'a> {
    pub tenant_id: &'a str,
    pub effective_release_id: i32,
    pub environment: &'a str,
    pub verified_publisher_principal: &'a str,
    pub packages: &'a BTreeSet<PackageCoordinate>,
    pub wirings: &'a BTreeSet<ReleaseWiringTarget>,
    pub attachments: &'a BTreeMap<String, ServingAttachment>,
    /// Whether provisioning marked this target's environment disposable.
    ///
    /// Read from the projection wamn-10yt.38 writes into the control store, so
    /// the condition is the TARGET and never an operator's say-so. It selects
    /// the dependency rule below and changes nothing else.
    pub environment_is_disposable: bool,
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

/// The deployment-attestation key derived from mounted release bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeploymentCoordinate {
    pub triple: Triple,
    pub tenant_id: String,
    pub effective_release_id: u32,
}

impl DeploymentCoordinate {
    pub fn new(org: &str, project: &str, release: &ServingRelease) -> Self {
        Self {
            triple: Triple::new(org, project, release.environment.as_str()),
            tenant_id: release.tenant_id.clone(),
            effective_release_id: release.effective_release_id.get(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MintedReleaseManifest {
    pub manifest: ServingManifest,
    pub digest: ManifestDigest,
    pub canonical_bytes: Vec<u8>,
}

pub const RELEASE_MANIFEST_MINT_REFUSAL: &str = "release-manifest-mint-refused";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MintManifestErrorKind {
    Storage,
    Release,
    PackageManifest,
    GeneratedPackageMetadata,
    PolicyContractUnsatisfied,
    Wiring,
    Component,
    OperationDependency,
    UnauthenticatedRegisteredOperation,
    UnauthenticatedWrite,
    Registration,
    ClosureConflict,
    Document,
    DuplicateAttachmentId,
    RouteHostUnbound,
    EnvironmentPolicyAbsent,
    EnvironmentPolicyMismatch,
    EnvironmentPolicySourceMismatch,
}

impl MintManifestErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Release => "release",
            Self::PackageManifest => "package-manifest",
            Self::GeneratedPackageMetadata => "package-weld",
            Self::PolicyContractUnsatisfied => "policy-contract-unsatisfied",
            Self::Wiring => "wiring",
            Self::Component => "component",
            Self::OperationDependency => "operation-dependency",
            Self::UnauthenticatedRegisteredOperation => "unauthenticated-registered-operation",
            Self::UnauthenticatedWrite => "unauthenticated-write",
            Self::Registration => "registration",
            Self::ClosureConflict => "closure-conflict",
            Self::Document => "document",
            Self::DuplicateAttachmentId => "duplicate-attachment-id",
            Self::RouteHostUnbound => "route-host-unbound",
            Self::EnvironmentPolicyAbsent => "environment-policy-not-converged",
            Self::EnvironmentPolicyMismatch => "environment-policy-environment-mismatch",
            Self::EnvironmentPolicySourceMismatch => "environment-policy-source-mismatch",
        }
    }
}

#[derive(Debug)]
pub struct MintManifestError {
    kind: MintManifestErrorKind,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl MintManifestError {
    pub fn new(kind: MintManifestErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            source: None,
        }
    }

    pub fn with_source(
        kind: MintManifestErrorKind,
        detail: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }

    pub const fn kind(&self) -> MintManifestErrorKind {
        self.kind
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl std::fmt::Display for MintManifestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{RELEASE_MANIFEST_MINT_REFUSAL} ({}): {}",
            self.kind.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for MintManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// The contract kind of each generated operation, keyed by package id and
/// exact operation.
type RouteKinds = BTreeMap<(String, String), OperationKind>;

/// What one release component member binds: a wiring node or a route.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MemberBinding {
    Wiring {
        package_id: String,
        package_version: String,
        wiring_id: String,
        wiring_version: u32,
        node_id: String,
    },
    Route {
        component: String,
        operation: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ReleaseComponentMembership {
    binding: MemberBinding,
    package_id: String,
    package_version: String,
    component_digest: String,
}

/// Inputs of one effective-release publication.
#[derive(Debug)]
pub struct PublishReleaseRequest {
    /// Owner connection to the project-environment database the release is minted in.
    pub database_url: String,
    /// Owner connection to the control database the release identity is projected to.
    pub control_database_url: String,
    /// Registry organization of the deployment coordinate.
    pub org: String,
    /// Registry project of the deployment coordinate.
    pub project: String,
    /// Tenant owning the release.
    pub tenant: String,
    /// Integer identity of the release.
    pub effective_release_id: u32,
    /// Environment the release is minted for.
    pub environment: String,
    /// Principal already authenticated by the publication boundary.
    pub verified_publisher_principal: String,
    /// Run-plane schema holding the tenant's environment policy.
    pub run_schema: String,
    /// Exact package membership.
    pub packages: Vec<PackageCoordinate>,
    /// Exact package-owned wirings.
    pub wirings: Vec<ReleaseWiringTarget>,
    /// Package-owned attachment documents.
    pub attachments: Vec<PathBuf>,
    /// Deployment-owned hostname applied to every HTTP route.
    pub route_host: Option<String>,
    /// Exact `wamn.json` for every package in the release.
    pub package_manifests: Vec<PathBuf>,
}

/// Parse one exact `PACKAGE_ID@PACKAGE_VERSION` package coordinate.
pub fn parse_package(value: &str) -> Result<PackageCoordinate, String> {
    let (package_id, package_version) = value
        .rsplit_once('@')
        .ok_or_else(|| "expected PACKAGE_ID@PACKAGE_VERSION".to_owned())?;
    PackageCoordinate::new(package_id, package_version).map_err(|error| error.to_string())
}

impl PublishReleaseRequest {
    fn deployment_coordinate(&self, release: &ServingRelease) -> DeploymentCoordinate {
        DeploymentCoordinate::new(&self.org, &self.project, release)
    }

    fn verified_run_schema(&self) -> anyhow::Result<BareSchemaName> {
        BareSchemaName::new(self.run_schema.clone())
            .map_err(|error| anyhow::anyhow!("invalid --run-schema {:?}: {error}", self.run_schema))
    }
}

/// Mint one effective release, project its identity, and return its manifest digest.
pub async fn publish_release(request: PublishReleaseRequest) -> anyhow::Result<ManifestDigest> {
    let minted = mint_candidate(&request, false).await?;
    let coordinate = request.deployment_coordinate(&minted.manifest.release);
    report_deployment_coordinate(&coordinate, &minted.digest);
    project_release_identity(&request.control_database_url, &coordinate).await?;
    Ok(minted.digest)
}

/// Assemble a candidate only in a provisioned disposable target, without publication.
pub async fn mint_local(
    mut args: PublishReleaseRequest,
    admissions: &[crate::push_component::ComponentAdmission],
    documents: Vec<(ComponentPackageScope, WiringDocument)>,
) -> anyhow::Result<(
    MintedReleaseManifest,
    wamn_runtime::local_application::LocalApplicationFacts,
)> {
    use wamn_runtime::local_application::{LocalApplicationFacts, LocalWiringFacts};
    wamn_runtime::local_application::require_local_target(
        &args.database_url,
        &args.tenant,
        &args.environment,
    )
    .await?;
    ensure!(
        read_projected_environment_disposable(&args.control_database_url, &args.tenant).await?,
        "local candidates require a provisioned disposable environment"
    );
    let (mut client, connection) = tokio_postgres::connect(&args.database_url, NoTls).await?;
    let driver = tokio::spawn(connection);
    let transaction = client.transaction().await?;
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&args.tenant])
        .await?;
    let latest: Option<i32> = transaction
        .query_one(
            "SELECT max(effective_release_id) FROM catalog.effective_releases WHERE tenant_id = $1",
            &[&args.tenant],
        )
        .await?
        .try_get(0)?;
    args.effective_release_id = u32::try_from(latest.unwrap_or(0))?
        .checked_add(1)
        .context("local release identity exhausted")?
        .max(args.effective_release_id);
    let authored = read_package_attachments(&args.attachments, &args.package_manifests)?;
    let attachments = resolve_route_host_overlay(&authored, args.route_host.as_deref())?;
    let (package_manifests, _, route_kinds) = read_package_manifests(&args.package_manifests)?;
    let packages = args.packages.iter().cloned().collect::<BTreeSet<_>>();
    let targets = args.wirings.iter().cloned().collect::<BTreeSet<_>>();
    ensure!(
        packages.len() == args.packages.len() && targets.len() == args.wirings.len(),
        "local candidate repeats a package or wiring"
    );
    let request = MintReleaseManifest {
        tenant_id: &args.tenant,
        effective_release_id: i32::try_from(args.effective_release_id)?,
        environment: &args.environment,
        verified_publisher_principal: &args.verified_publisher_principal,
        packages: &packages,
        wirings: &targets,
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
    let mut component_facts = BTreeMap::<(String, String), Vec<AdmittedComponent>>::new();
    for admission in admissions {
        let fact = admission.facts();
        ensure!(
            fact.scope.tenant_id == args.tenant
                && packages
                    .iter()
                    .any(|package| package.package_id() == fact.scope.package_id
                        && package.package_version() == fact.scope.package_version),
            "local admission is outside package membership"
        );
        component_facts
            .entry((
                fact.scope.package_id.clone(),
                fact.scope.package_version.clone(),
            ))
            .or_default()
            .push(fact.clone());
    }
    let mut components = BTreeSet::new();
    let mut wirings = BTreeSet::new();
    let mut membership = BTreeSet::new();
    let mut entry_targets = BTreeMap::<String, Vec<ReleaseWiringTarget>>::new();
    let mut one_node = Vec::new();
    let mut local_wirings = Vec::new();
    ensure!(
        documents.len() == targets.len(),
        "local wiring closure is incomplete"
    );
    for (scope, document) in documents {
        let target = ReleaseWiringTarget {
            package_id: scope.package_id.clone(),
            package_version: scope.package_version.clone(),
            wiring_id: document.wiring_id.clone(),
            wiring_version: document.version,
        };
        ensure!(
            scope.tenant_id == args.tenant && targets.contains(&target),
            "local wiring is outside release membership"
        );
        let entry_operation = project_wiring_document(
            &request,
            &target,
            &scope,
            &document,
            &component_facts,
            &package_manifests,
            &mut components,
            &mut wirings,
            &mut membership,
            &mut one_node,
        )?;
        let node_components = resolve_wiring_components(
            &document,
            &scope,
            &component_facts,
            package_manifests.get(&scope.package_id),
            DependencyDigestRule::for_environment(true),
        )?;
        local_wirings.push(LocalWiringFacts {
            scope,
            document,
            node_components,
        });
        entry_targets
            .entry(entry_operation)
            .or_default()
            .push(target);
    }
    let routes = project_routes(
        &request,
        &route_kinds,
        &component_facts,
        &mut components,
        &mut membership,
    )?;
    let registrations = derive_serving_registrations(&package_manifests, &entry_targets)?;
    refuse_unregistered_one_node_wirings(&one_node, &registrations)?;
    let manifest = ServingManifest {
        format_version: SERVING_MANIFEST_FORMAT_VERSION,
        release: ServingRelease {
            tenant_id: args.tenant.clone(),
            effective_release_id: EffectiveReleaseId::new(args.effective_release_id)?,
            environment: args.environment.clone(),
            packages: packages.clone(),
        },
        components,
        routes,
        wirings,
        attachments: attachments.clone(),
        registrations,
    };
    let canonical_bytes = manifest.canonical_bytes();
    let (manifest, digest) = ServingManifest::from_canonical_bytes(&canonical_bytes)?;
    let components = admissions
        .iter()
        .map(super::push_component::ComponentAdmission::facts)
        .filter(|fact| {
            manifest
                .components
                .iter()
                .any(|component| component.digest.as_str() == fact.component_digest)
        })
        .cloned()
        .collect();
    let requirements = admissions
        .iter()
        .flat_map(|admission| admission.requirements().iter())
        .filter(|requirement| {
            manifest
                .components
                .iter()
                .any(|component| component.digest.as_str() == requirement.component_digest())
        })
        .cloned()
        .collect();
    let facts = LocalApplicationFacts {
        manifest_digest: digest.clone(),
        components,
        wirings: local_wirings,
        requirements,
        bindings: Vec::new(),
    };
    let run_schema = args.verified_run_schema()?;
    let policy = crate::verification_policy::read_authoritative_environment_policy(
        &args.control_database_url,
        &args.org,
        &args.environment,
        false,
    )
    .await?;
    let projected =
        read_projected_environment_policy(&transaction, &run_schema, &args.tenant).await?;
    verify_projected_environment_policy(
        projected.as_ref(),
        &policy,
        &manifest.release,
        &run_schema,
    )?;
    // The retained run-plane FK needs only this session-local identity and
    // package membership. No immutable component slots or publication facts.
    establish_release(&transaction, &request).await?;
    transaction.commit().await?;
    drop(client);
    driver.abort();
    Ok((
        MintedReleaseManifest {
            manifest,
            digest,
            canonical_bytes,
        },
        facts,
    ))
}

async fn mint_candidate(
    args: &PublishReleaseRequest,
    local: bool,
) -> anyhow::Result<MintedReleaseManifest> {
    ensure!(
        args.effective_release_id > 0,
        "effective-release-id must be greater than zero"
    );
    ensure!(
        !args.environment.is_empty(),
        "environment must not be empty"
    );
    ensure!(
        !args.verified_publisher_principal.is_empty(),
        "verified-publisher-principal must not be empty"
    );
    let authored_attachments =
        read_package_attachments(&args.attachments, &args.package_manifests)?;
    let attachments =
        resolve_route_host_overlay(&authored_attachments, args.route_host.as_deref())?;
    let (package_manifests, package_manifest_hashes, route_kinds) =
        read_package_manifests(&args.package_manifests)?;
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
    let wirings = args.wirings.iter().cloned().collect::<BTreeSet<_>>();
    ensure!(
        wirings.len() == args.wirings.len(),
        "effective release repeats a wiring target"
    );
    let release_id = i32::try_from(args.effective_release_id)
        .context("effective-release-id exceeds PostgreSQL integer")?;
    let environment_is_disposable =
        read_projected_environment_disposable(&args.control_database_url, &args.tenant)
            .await
            .context("resolve the release target's disposable marker")?;
    ensure!(
        !local || environment_is_disposable,
        "local candidates require a provisioned disposable environment"
    );
    let request = MintReleaseManifest {
        tenant_id: &args.tenant,
        effective_release_id: release_id,
        environment: &args.environment,
        verified_publisher_principal: &args.verified_publisher_principal,
        packages: &packages,
        wirings: &wirings,
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
    .context("read the authoritative environment policy before release mint")?;

    let (mut client, connection) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect to the release project environment")?;
    let connection_task = tokio::spawn(connection);
    let minted = mint_in_transaction(
        &mut client,
        &request,
        &package_manifests,
        &package_manifest_hashes,
        &route_kinds,
        &run_schema,
        &source_policy,
    )
    .await;
    match minted {
        Ok(minted) => {
            drop(client);
            connection_task
                .await
                .context("join the release mint connection")?
                .context("drive the release mint connection")?;
            Ok(minted)
        }
        Err(error) => {
            connection_task.abort();
            Err(error)
        }
    }
}

async fn mint_in_transaction(
    client: &mut Client,
    request: &MintReleaseManifest<'_>,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    package_manifest_hashes: &BTreeMap<String, String>,
    route_kinds: &RouteKinds,
    run_schema: &BareSchemaName,
    source_policy: &AuthoritativeEnvironmentPolicy,
) -> anyhow::Result<MintedReleaseManifest> {
    let transaction = client
        .transaction()
        .await
        .context("begin the release mint")?;
    let minted = mint_release_manifest_with_package_manifests(
        &transaction,
        request,
        package_manifests,
        package_manifest_hashes,
        route_kinds,
    )
    .await?;
    let projected =
        read_projected_environment_policy(&transaction, run_schema, request.tenant_id).await?;
    verify_projected_environment_policy(
        projected.as_ref(),
        source_policy,
        &minted.manifest.release,
        run_schema,
    )?;
    transaction
        .commit()
        .await
        .context("commit the release mint")?;
    Ok(minted)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ProjectedEnvironmentPolicy {
    expected_environment: String,
    source_policy_org: Option<String>,
    source_policy_hash: Option<String>,
}

pub(crate) async fn read_projected_environment_policy(
    transaction: &Transaction<'_>,
    run_schema: &BareSchemaName,
    tenant_id: &str,
) -> Result<Option<ProjectedEnvironmentPolicy>, MintManifestError> {
    transaction
        .query_opt(&projected_environment_policy_sql(run_schema), &[&tenant_id])
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

pub(crate) async fn read_expected_environment(
    transaction: &Transaction<'_>,
    run_schema: &BareSchemaName,
    tenant_id: &str,
) -> Result<Option<String>, MintManifestError> {
    transaction
        .query_opt(&expected_environment_sql(run_schema), &[&tenant_id])
        .await
        .map(|row| row.map(|row| row.get(0)))
        .map_err(|error| storage("read the provisioned environment policy", error))
}

pub(crate) fn verify_provisioned_environment(
    expected_environment: Option<&str>,
    release: &ServingRelease,
    run_schema: &BareSchemaName,
) -> Result<(), MintManifestError> {
    let Some(expected_environment) = expected_environment else {
        return Err(environment_policy_absent(release, run_schema));
    };
    verify_environment_name(expected_environment, release)
}

pub(crate) fn verify_projected_environment_policy(
    projected: Option<&ProjectedEnvironmentPolicy>,
    source_policy: &AuthoritativeEnvironmentPolicy,
    release: &ServingRelease,
    run_schema: &BareSchemaName,
) -> Result<(), MintManifestError> {
    let Some(projected) = projected else {
        return Err(environment_policy_absent(release, run_schema));
    };
    verify_environment_name(&projected.expected_environment, release)?;
    if projected.source_policy_org.as_deref() != Some(source_policy.source_policy_org.as_ref())
        || projected.source_policy_hash.as_deref()
            != Some(source_policy.source_policy_hash.as_ref())
    {
        return Err(MintManifestError::new(
            MintManifestErrorKind::EnvironmentPolicySourceMismatch,
            format!(
                "environment {:?} policy source differs: projected-org={:?}, authoritative-org={:?}, projected-hash={:?}, authoritative-hash={:?}; rerun the verification policy projection",
                release.environment,
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
    release: &ServingRelease,
    run_schema: &BareSchemaName,
) -> MintManifestError {
    MintManifestError::new(
        MintManifestErrorKind::EnvironmentPolicyAbsent,
        format!(
            "tenant {:?} has no row in {}.environment_policies; run reconcile-run-plane",
            release.tenant_id,
            run_schema.as_str()
        ),
    )
}

fn verify_environment_name(
    expected_environment: &str,
    release: &ServingRelease,
) -> Result<(), MintManifestError> {
    if expected_environment != release.environment {
        return Err(MintManifestError::new(
            MintManifestErrorKind::EnvironmentPolicyMismatch,
            format!(
                "release environment {:?} differs from provisioned environment {:?}",
                release.environment, expected_environment
            ),
        ));
    }
    Ok(())
}

pub fn report_deployment_coordinate(
    coordinate: &DeploymentCoordinate,
    manifest_hash: &ManifestDigest,
) {
    tracing::info!(
        org = %coordinate.triple.org,
        project = %coordinate.triple.project,
        environment = %coordinate.triple.env,
        tenant = %coordinate.tenant_id,
        effective_release_id = coordinate.effective_release_id,
        manifest_hash = %manifest_hash,
        "release carries a complete deployment attestation coordinate"
    );
}

/// Whether provisioning marked this tenant's environment disposable.
///
/// The same projection the admit path reads (wamn-10yt.38), from the same
/// control store, read ONCE per release mint rather than once per fact. The
/// control database always carries `catalog`, because the release identity is
/// projected into it a few lines later, so an absent relation is a stale store
/// and says so rather than defaulting quietly. A tenant with no projected row
/// is DURABLE, which is what keeps the change additive.
const SELECT_ENVIRONMENT_DISPOSABLE_SQL: &str = "SELECT coalesce((\
         SELECT disposable FROM catalog.tenant_environments WHERE tenant_id = $1\
     ), false)";

async fn read_projected_environment_disposable(
    control_database_url: &str,
    tenant_id: &str,
) -> anyhow::Result<bool> {
    on_control_plane(control_database_url, async |control| {
        let row = control
            .query_one(SELECT_ENVIRONMENT_DISPOSABLE_SQL, &[&tenant_id])
            .await
            .context("read the projected environment's disposable marker")?;
        Ok(row.get(0))
    })
    .await
}

async fn on_control_plane<F, T>(control_database_url: &str, write: F) -> anyhow::Result<T>
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

// A CHECK refusal can include unrestricted source provenance in the failing row.
// Keep the message and constraint, but omit PostgreSQL DETAIL and HINT.
fn render_driver_failure(error: &tokio_postgres::Error) -> String {
    match error.as_db_error() {
        Some(database) => match database.constraint() {
            Some(constraint) => format!("{} ({constraint})", database.message()),
            None => database.message().to_owned(),
        },
        None => error.to_string(),
    }
}

pub async fn project_release_identity(
    control_database_url: &str,
    coordinate: &DeploymentCoordinate,
) -> anyhow::Result<()> {
    let effective_release_id = i32::try_from(coordinate.effective_release_id)
        .context("effective-release-id exceeds PostgreSQL integer")?;
    let identity = wamn_schema_control::attestation::EffectiveReleaseIdentity {
        tenant_id: &coordinate.tenant_id,
        effective_release_id,
        environment: coordinate.triple.env.as_str(),
    };
    let statement = wamn_schema_control::attestation::project_effective_release_identity(&identity);
    on_control_plane(control_database_url, async |control| {
        let storage = |error: tokio_postgres::Error| {
            anyhow::Error::new(
                wamn_schema_control::attestation::translate_projection_failure(
                    &identity,
                    &render_driver_failure(&error),
                ),
            )
        };
        let transaction = control.transaction().await.map_err(storage)?;
        transaction
            .query_one(CLAIM_TENANT_SQL, &[&identity.tenant_id])
            .await
            .map_err(storage)?;
        let params = crate::sql_params::as_postgres(&statement.params);
        transaction
            .execute(statement.sql.as_str(), &params)
            .await
            .map_err(storage)?;
        // The separate read sees the winner after a concurrent insert finishes.
        let winner = transaction
            .query_one(
                wamn_schema_control::attestation::read_effective_release_identity_sql(),
                &[&identity.tenant_id, &identity.effective_release_id],
            )
            .await
            .map_err(storage)?;
        wamn_schema_control::attestation::check_projected_identity(
            &identity,
            winner.try_get(0).map_err(storage)?,
        )?;
        transaction.commit().await.map_err(storage)
    })
    .await
}

pub async fn attest_deployment(
    control_database_url: &str,
    coordinate: &DeploymentCoordinate,
    manifest_hash: &ManifestDigest,
    source_commit: Option<&str>,
) -> anyhow::Result<String> {
    let effective_release_id = i32::try_from(coordinate.effective_release_id)
        .context("effective-release-id exceeds PostgreSQL integer")?;
    on_control_plane(control_database_url, async |control| {
        let proposed_attested_at: String = control
            .query_one("SELECT clock_timestamp()::text", &[])
            .await
            .context("read the proposed control database attestation instant")?
            .get(0);
        let unresolved = wamn_schema_control::attestation::Attestation {
            tenant_id: &coordinate.tenant_id,
            environment_instance: "",
            effective_release_id,
            org_id: &coordinate.triple.org,
            project_id: &coordinate.triple.project,
            environment: coordinate.triple.env.as_str(),
            deployed_manifest_hash: manifest_hash.as_str(),
            source_commit,
            attested_at: &proposed_attested_at,
        };
        let storage = |error: tokio_postgres::Error| {
            anyhow::Error::new(wamn_schema_control::attestation::translate_failure(
                &unresolved,
                &render_driver_failure(&error),
            ))
        };
        let transaction = control.transaction().await.map_err(storage)?;
        transaction
            .query_one(CLAIM_TENANT_SQL, &[&unresolved.tenant_id])
            .await
            .map_err(storage)?;
        let projected = transaction
            .query_opt(
                wamn_schema_control::attestation::read_environment_instance_sql(),
                &[&unresolved.tenant_id],
            )
            .await
            .map_err(storage)?;
        let environment_instance: String = match projected {
            Some(row) => row.try_get(0).map_err(storage)?,
            None => String::new(),
        };
        let attestation = wamn_schema_control::attestation::Attestation {
            environment_instance: &environment_instance,
            ..unresolved
        };
        let storage = |error: tokio_postgres::Error| {
            anyhow::Error::new(wamn_schema_control::attestation::translate_failure(
                &attestation,
                &render_driver_failure(&error),
            ))
        };
        let statement = wamn_schema_control::attestation::register_attestation(&attestation);
        let params = crate::sql_params::as_postgres(&statement.params);
        let inserted = transaction
            .query_opt(statement.sql.as_str(), &params)
            .await
            .map_err(storage)?;
        let winner: chrono::DateTime<chrono::Utc> = if let Some(row) = inserted {
            row.try_get(0).map_err(storage)?
        } else {
            // Read after INSERT waits, so a concurrent winner is visible.
            let row = transaction
                .query_one(
                    wamn_schema_control::attestation::read_attestation_sql(),
                    &params[..6],
                )
                .await
                .map_err(storage)?;
            wamn_schema_control::attestation::check_attestation(
                &attestation,
                row.try_get(0).map_err(storage)?,
                row.try_get(1).map_err(storage)?,
            )?;
            row.try_get(2).map_err(storage)?
        };
        transaction.commit().await.map_err(storage)?;
        Ok(winner.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
    })
    .await
}

fn sha256(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
    )
}

/// Mint a release promoted from a published one.
///
/// The source manifest is the authority once published, so its registrations
/// and the kinds of its routes carry over. Promotion never reads a package
/// folder.
pub async fn mint_promoted_release_manifest(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
    registrations: &BTreeMap<String, ServingRegistration>,
    routes: &BTreeSet<ServingRoute>,
) -> Result<MintedReleaseManifest, MintManifestError> {
    let package_manifests = BTreeMap::new();
    let route_kinds = routes
        .iter()
        .map(|route| {
            (
                (route.package_id.clone(), route.operation.clone()),
                route.kind,
            )
        })
        .collect();
    mint_release_manifest_from_sources(
        transaction,
        request,
        &package_manifests,
        None,
        &route_kinds,
        Some(registrations),
    )
    .await
}

async fn mint_release_manifest_with_package_manifests(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    package_manifest_hashes: &BTreeMap<String, String>,
    route_kinds: &RouteKinds,
) -> Result<MintedReleaseManifest, MintManifestError> {
    mint_release_manifest_from_sources(
        transaction,
        request,
        package_manifests,
        Some(package_manifest_hashes),
        route_kinds,
        None,
    )
    .await
}

async fn mint_release_manifest_from_sources(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    package_manifest_hashes: Option<&BTreeMap<String, String>>,
    route_kinds: &RouteKinds,
    promoted_registrations: Option<&BTreeMap<String, ServingRegistration>>,
) -> Result<MintedReleaseManifest, MintManifestError> {
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&request.tenant_id])
        .await
        .map_err(|error| storage("claim the release tenant", error))?;
    validate_request(request)?;
    if let Some(package_manifest_hashes) = package_manifest_hashes {
        validate_release_package_manifests(
            transaction,
            request,
            package_manifests,
            package_manifest_hashes,
        )
        .await?;
    }
    establish_release(transaction, request).await?;

    let mut components = BTreeSet::new();
    let mut wirings = BTreeSet::new();
    let mut membership = BTreeSet::new();
    let mut component_facts = BTreeMap::new();
    let mut entry_targets = BTreeMap::<String, Vec<ReleaseWiringTarget>>::new();
    let mut one_node = Vec::new();
    for package in request.packages {
        let scope = ComponentPackageScope {
            tenant_id: request.tenant_id.to_owned(),
            package_id: package.package_id().to_owned(),
            package_version: package.package_version().to_owned(),
        };
        let facts = load_component_facts(transaction, &scope).await?;
        component_facts.insert(
            (scope.package_id.clone(), scope.package_version.clone()),
            facts,
        );
    }
    for target in request.wirings {
        let package = PackageCoordinate::new(&target.package_id, &target.package_version)
            .expect("ReleaseWiringTarget parsing admitted this coordinate");
        if !request.packages.contains(&package) {
            return Err(MintManifestError::new(
                MintManifestErrorKind::Wiring,
                format!(
                    "wiring {}@{}::{}/{} is outside the effective release membership",
                    target.package_id,
                    target.package_version,
                    target.wiring_id,
                    target.wiring_version
                ),
            ));
        }
        let scope = ComponentPackageScope {
            tenant_id: request.tenant_id.to_owned(),
            package_id: target.package_id.clone(),
            package_version: target.package_version.clone(),
        };
        let entry_operation = resolve_wiring(
            transaction,
            request,
            target,
            &scope,
            &component_facts,
            package_manifests,
            &mut components,
            &mut wirings,
            &mut membership,
            &mut one_node,
        )
        .await?;
        entry_targets
            .entry(entry_operation)
            .or_default()
            .push(target.clone());
    }

    let routes = project_routes(
        request,
        route_kinds,
        &component_facts,
        &mut components,
        &mut membership,
    )?;
    let registrations = if let Some(registrations) = promoted_registrations {
        registrations.clone()
    } else {
        derive_serving_registrations(package_manifests, &entry_targets)?
    };
    refuse_unregistered_one_node_wirings(&one_node, &registrations)?;

    let release_id = EffectiveReleaseId::new(
        u32::try_from(request.effective_release_id).expect("validate_request checked release id"),
    )
    .expect("validate_request checked release id");
    let projected = ServingManifest {
        format_version: SERVING_MANIFEST_FORMAT_VERSION,
        release: ServingRelease {
            tenant_id: request.tenant_id.to_owned(),
            effective_release_id: release_id,
            environment: request.environment.to_owned(),
            packages: request.packages.clone(),
        },
        components,
        routes,
        wirings,
        attachments: request.attachments.clone(),
        registrations,
    };
    let canonical_bytes = projected.canonical_bytes();
    let (manifest, digest) =
        ServingManifest::from_canonical_bytes(&canonical_bytes).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Document,
                format!(
                    "effective release {} does not project a deliverable format-1 manifest",
                    request.effective_release_id
                ),
                error,
            )
        })?;
    freeze_release(transaction, request, &membership, &digest, &canonical_bytes).await?;
    Ok(MintedReleaseManifest {
        manifest,
        digest,
        canonical_bytes,
    })
}

fn validate_request(request: &MintReleaseManifest<'_>) -> Result<(), MintManifestError> {
    if request.effective_release_id <= 0
        || request.environment.is_empty()
        || request.verified_publisher_principal.is_empty()
    {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Release,
            "effective release id, environment, and publisher principal are required",
        ));
    }
    if request.packages.is_empty() {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Release,
            "an effective release requires at least one exact package pair",
        ));
    }
    let mut ids = BTreeSet::new();
    if !request
        .packages
        .iter()
        .all(|package| ids.insert(package.package_id()))
    {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Release,
            "an effective release cannot contain two versions of one package",
        ));
    }
    let routed = request
        .attachments
        .values()
        .any(|attachment| matches!(attachment.target, AttachmentTarget::Route { .. }));
    if request.wirings.is_empty() && !routed {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Wiring,
            "a release with no route and no wiring has no executable closure",
        ));
    }
    validate_attachment_definition_hashes(request.attachments)?;
    Ok(())
}

/// Bind the release's behavior and ownership inputs to the exact package bytes
/// already admitted by `apply-package`.
async fn validate_release_package_manifests(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    package_manifest_hashes: &BTreeMap<String, String>,
) -> Result<(), MintManifestError> {
    let expected_package_ids = request
        .packages
        .iter()
        .map(PackageCoordinate::package_id)
        .collect::<BTreeSet<_>>();
    let presented_package_ids = package_manifests
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let hashed_package_ids = package_manifest_hashes
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if presented_package_ids != expected_package_ids || hashed_package_ids != expected_package_ids {
        return Err(MintManifestError::new(
            MintManifestErrorKind::PackageManifest,
            format!(
                "release package manifests must exactly match membership; expected={expected_package_ids:?}, presented={presented_package_ids:?}, hashed={hashed_package_ids:?}; supply one exact wamn.json per release package"
            ),
        ));
    }
    for package in request.packages {
        let coordinate = format!("{}@{}", package.package_id(), package.package_version());
        let manifest = package_manifests
            .get(package.package_id())
            .ok_or_else(|| {
                MintManifestError::new(
                    MintManifestErrorKind::PackageManifest,
                    format!(
                        "package {coordinate} has no presented manifest; supply the exact wamn.json applied by apply-package"
                    ),
                )
            })?;
        if manifest.package.id != package.package_id()
            || manifest.package.version != package.package_version()
        {
            return Err(MintManifestError::new(
                MintManifestErrorKind::PackageManifest,
                format!(
                    "package {coordinate} is paired with manifest {}@{}; supply the exact wamn.json applied by apply-package",
                    manifest.package.id, manifest.package.version
                ),
            ));
        }
        let presented_hash = package_manifest_hashes
            .get(package.package_id())
            .ok_or_else(|| {
                MintManifestError::new(
                    MintManifestErrorKind::PackageManifest,
                    format!(
                        "package {coordinate} has no presented manifest hash; supply the exact wamn.json applied by apply-package"
                    ),
                )
            })?;
        let Some(row) = transaction
            .query_opt(
                SELECT_APPLIED_PACKAGE_MANIFEST_SQL,
                &[
                    &request.tenant_id,
                    &package.package_id(),
                    &package.package_version(),
                ],
            )
            .await
            .map_err(|error| storage("read the applied package manifest identity", error))?
        else {
            return Err(MintManifestError::new(
                MintManifestErrorKind::PackageManifest,
                format!(
                    "package {coordinate} is not applied; run apply-package against the target project environment"
                ),
            ));
        };
        let applied_hash: String = row.get(0);
        if applied_hash != *presented_hash {
            return Err(MintManifestError::new(
                MintManifestErrorKind::PackageManifest,
                format!(
                    "package {coordinate} presented manifest hash {presented_hash} differs from applied hash {applied_hash}; use the exact wamn.json recorded by apply-package"
                ),
            ));
        }
    }
    Ok(())
}

async fn establish_release(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
) -> Result<(), MintManifestError> {
    transaction
        .execute(
            INSERT_RELEASE_SQL,
            &[
                &request.tenant_id,
                &request.effective_release_id,
                &request.environment,
                &request.verified_publisher_principal,
            ],
        )
        .await
        .map_err(|error| storage("register the effective release", error))?;
    let row = transaction
        .query_one(
            LOCK_RELEASE_SQL,
            &[&request.tenant_id, &request.effective_release_id],
        )
        .await
        .map_err(|error| storage("lock the effective release", error))?;
    let environment: String = row.get(0);
    let publisher: String = row.get(1);
    if environment != request.environment || publisher != request.verified_publisher_principal {
        return Err(MintManifestError::new(
            MintManifestErrorKind::ClosureConflict,
            "effective release identity already carries other environment or publisher facts",
        ));
    }
    for package in request.packages {
        transaction
            .execute(
                INSERT_PACKAGE_SQL,
                &[
                    &request.tenant_id,
                    &request.effective_release_id,
                    &package.package_id(),
                    &package.package_version(),
                ],
            )
            .await
            .map_err(|error| storage("record exact release package membership", error))?;
    }
    let observed = transaction
        .query(
            SELECT_PACKAGES_SQL,
            &[&request.tenant_id, &request.effective_release_id],
        )
        .await
        .map_err(|error| storage("read exact release package membership", error))?
        .into_iter()
        .map(|row| {
            PackageCoordinate::new(row.get::<_, String>(0), row.get::<_, String>(1))
                .expect("stored package coordinates passed relation checks")
        })
        .collect::<BTreeSet<_>>();
    if observed != *request.packages {
        return Err(MintManifestError::new(
            MintManifestErrorKind::ClosureConflict,
            "effective release package membership is already frozen to another exact set",
        ));
    }
    Ok(())
}

pub async fn load_component_facts(
    transaction: &Transaction<'_>,
    scope: &ComponentPackageScope,
) -> Result<Vec<AdmittedComponent>, MintManifestError> {
    transaction
        .query(
            SELECT_COMPONENT_FACTS_SQL,
            &[&scope.tenant_id, &scope.package_id, &scope.package_version],
        )
        .await
        .map_err(|error| storage("read admitted component facts", error))?
        .into_iter()
        .map(|row| {
            let component: String = row.get(0);
            let decoded = AdmittedComponent {
                scope: scope.clone(),
                component: component.clone(),
                interface_version: row.get(1),
                operations: decode_json::<BTreeMap<String, AdmittedComponentOperation>>(
                    row.get(2),
                    &component,
                    "operations",
                )?,
                component_digest: row.get(3),
                imports: decode_json(row.get(4), &component, "imports")?,
                imports_fingerprint: row.get(5),
                effects: decode_json::<Vec<AdmittedComponentEffect>>(
                    row.get(6),
                    &component,
                    "effects",
                )?,
            };
            wamn_catalog::verify_stored_effect_projection(&decoded).map_err(|error| {
                MintManifestError::with_source(
                    MintManifestErrorKind::Component,
                    format!("component {component:?} stores an invalid effect projection"),
                    error,
                )
            })?;
            Ok(decoded)
        })
        .collect()
}

fn decode_json<T: DeserializeOwned>(
    stored: &str,
    component: &str,
    field: &'static str,
) -> Result<T, MintManifestError> {
    serde_json::from_str(stored).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Component,
            format!("component {component:?} stores unreadable {field}"),
            error,
        )
    })
}

fn derive_serving_registrations(
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    entry_targets: &BTreeMap<String, Vec<ReleaseWiringTarget>>,
) -> Result<BTreeMap<String, ServingRegistration>, MintManifestError> {
    let mut registrations = BTreeMap::new();
    for manifest in package_manifests.values() {
        for (operation_key, operation) in &manifest.custom_operations {
            let Some(declaration) = operation.registration() else {
                continue;
            };
            let source_manifest = package_manifests
                .get(&declaration.source_package)
                .ok_or_else(|| {
                    MintManifestError::new(
                        MintManifestErrorKind::Registration,
                        format!(
                            "event handler {operation_key:?} source package {:?} is absent while resolving entity {:?}",
                            declaration.source_package, declaration.entity
                        ),
                    )
                })?;
            if !source_manifest.models.contains_key(&declaration.entity) {
                return Err(MintManifestError::new(
                    MintManifestErrorKind::Registration,
                    format!(
                        "event handler {operation_key:?} source package {:?} does not own entity {:?}",
                        declaration.source_package, declaration.entity
                    ),
                ));
            }
            let operation_id = wamn_schema_generator::canonical_operation_identity(
                &manifest.package,
                operation_key,
            )
            .map_err(|error| {
                MintManifestError::with_source(
                    MintManifestErrorKind::Registration,
                    format!("derive exact handler operation {operation_key:?}"),
                    error,
                )
            })?;
            let targets = entry_targets
                .get(&operation_id)
                .into_iter()
                .flatten()
                .filter(|target| {
                    target.package_id == manifest.package.id
                        && target.package_version == manifest.package.version
                })
                .collect::<Vec<_>>();
            if targets.len() != 1 {
                return Err(MintManifestError::new(
                    MintManifestErrorKind::Registration,
                    format!(
                        "event handler {operation_id:?} resolves to {} selected owner wiring(s); expected exactly one",
                        targets.len()
                    ),
                ));
            }
            let target = targets[0];
            let registration_id = format!("{}::{operation_key}", manifest.package.id);
            registrations.insert(
                registration_id,
                ServingRegistration {
                    package_id: manifest.package.id.clone(),
                    source_package_id: declaration.source_package.clone(),
                    wiring_id: target.wiring_id.clone(),
                    wiring_version: target.wiring_version,
                    entity: declaration.entity.clone(),
                    ops: declaration
                        .ops
                        .iter()
                        .map(|op| op.as_str().to_owned())
                        .collect(),
                    input: wamn_catalog::ServingRegistrationInput::Event,
                },
            );
        }
    }
    Ok(registrations)
}

#[allow(clippy::too_many_arguments)]
async fn resolve_wiring(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
    target: &ReleaseWiringTarget,
    scope: &ComponentPackageScope,
    component_facts: &BTreeMap<(String, String), Vec<AdmittedComponent>>,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    components: &mut BTreeSet<ServingComponent>,
    wirings: &mut BTreeSet<ServingWiring>,
    membership: &mut BTreeSet<ReleaseComponentMembership>,
    one_node: &mut Vec<ReleaseWiringTarget>,
) -> Result<String, MintManifestError> {
    let version = i32::try_from(target.wiring_version).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Wiring,
            "wiring version exceeds PostgreSQL integer",
            error,
        )
    })?;
    let Some(row) = transaction
        .query_opt(
            SELECT_WIRING_SQL,
            &[
                &request.tenant_id,
                &target.package_id,
                &target.package_version,
                &target.wiring_id,
                &version,
            ],
        )
        .await
        .map_err(|error| storage("read a release wiring", error))?
    else {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Wiring,
            format!(
                "package {}@{} has no wiring {} version {}",
                target.package_id, target.package_version, target.wiring_id, target.wiring_version
            ),
        ));
    };
    let stored_hash: String = row.get(0);
    let stored_document: String = row.get(1);
    let document_value = serde_json::from_str(&stored_document).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Wiring,
            format!("wiring {:?} stores unreadable graph JSON", target.wiring_id),
            error,
        )
    })?;
    let document = WiringDocument::parse(&document_value).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Wiring,
            format!("wiring {:?} stores an invalid document", target.wiring_id),
            error,
        )
    })?;
    if document.wiring_id != target.wiring_id || document.version != target.wiring_version {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Wiring,
            "wiring row identity differs from its stored document",
        ));
    }
    let derived_hash = document.wiring_hash();
    if stored_hash != derived_hash.as_str() {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Wiring,
            "wiring row hash differs from its canonical document hash",
        ));
    }
    project_wiring_document(
        request,
        target,
        scope,
        &document,
        component_facts,
        package_manifests,
        components,
        wirings,
        membership,
        one_node,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "existing release projection inputs are independent facts"
)]
fn project_wiring_document(
    request: &MintReleaseManifest<'_>,
    target: &ReleaseWiringTarget,
    scope: &ComponentPackageScope,
    document: &WiringDocument,
    component_facts: &BTreeMap<(String, String), Vec<AdmittedComponent>>,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    components: &mut BTreeSet<ServingComponent>,
    wirings: &mut BTreeSet<ServingWiring>,
    membership: &mut BTreeSet<ReleaseComponentMembership>,
    one_node: &mut Vec<ReleaseWiringTarget>,
) -> Result<String, MintManifestError> {
    if document.edges.is_empty() {
        one_node.push(target.clone());
    }
    let rule = DependencyDigestRule::for_environment(request.environment_is_disposable);
    let resolved = resolve_wiring_components(
        document,
        scope,
        component_facts,
        package_manifests.get(&target.package_id),
        rule,
    )?;
    validate_resolved_wiring_compatibility(document, &resolved).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Component,
            format!(
                "wiring {:?} version {} is incompatible with its resolved component facts",
                target.wiring_id, target.wiring_version
            ),
            error,
        )
    })?;
    resolve_component_dependency_closure(&resolved, component_facts, rule)?;
    validate_anonymous_wiring_closure(request.attachments, target, document, &resolved)?;
    let entry_operation = resolved_wiring_entry_operation(document, &resolved)?;
    wirings.insert(ServingWiring {
        package_id: target.package_id.clone(),
        wiring_id: target.wiring_id.clone(),
        wiring_version: target.wiring_version,
        graph_hash: document.wiring_hash(),
    });
    for fact in resolved.values() {
        components.insert(project_serving_component(fact, component_facts, rule)?);
    }
    for (node_id, fact) in resolved {
        membership.insert(ReleaseComponentMembership {
            binding: MemberBinding::Wiring {
                package_id: target.package_id.clone(),
                package_version: target.package_version.clone(),
                wiring_id: target.wiring_id.clone(),
                wiring_version: target.wiring_version,
                node_id,
            },
            package_id: fact.scope.package_id.clone(),
            package_version: fact.scope.package_version.clone(),
            component_digest: fact.component_digest.clone(),
        });
    }
    Ok(entry_operation)
}

/// Project the route of every route attachment, with its component closure and
/// its one release component member.
///
/// The kind comes from the generated contract of the operation, or from the
/// published manifest a promotion copies.
fn project_routes(
    request: &MintReleaseManifest<'_>,
    route_kinds: &RouteKinds,
    component_facts: &BTreeMap<(String, String), Vec<AdmittedComponent>>,
    components: &mut BTreeSet<ServingComponent>,
    membership: &mut BTreeSet<ReleaseComponentMembership>,
) -> Result<BTreeSet<ServingRoute>, MintManifestError> {
    let rule = DependencyDigestRule::for_environment(request.environment_is_disposable);
    let mut routes = BTreeSet::new();
    for (attachment_id, attachment) in request.attachments {
        let AttachmentTarget::Route {
            component,
            operation,
        } = &attachment.target
        else {
            continue;
        };
        let package = request
            .packages
            .iter()
            .find(|package| package.package_id() == attachment.package_id)
            .ok_or_else(|| {
                MintManifestError::new(
                    MintManifestErrorKind::Component,
                    format!(
                        "route attachment {attachment_id:?} names package {:?} outside the effective release membership",
                        attachment.package_id
                    ),
                )
            })?;
        let facts = component_facts
            .get(&(
                package.package_id().to_owned(),
                package.package_version().to_owned(),
            ))
            .map_or(&[][..], Vec::as_slice);
        let fact = resolve_route_component(attachment_id, attachment, component, operation, facts)?;
        let kind = *route_kinds
            .get(&(attachment.package_id.clone(), operation.clone()))
            .ok_or_else(|| {
                MintManifestError::new(
                    MintManifestErrorKind::GeneratedPackageMetadata,
                    format!(
                        "route operation {operation:?} of package {:?} has no generated contract kind; regenerate the package evidence",
                        attachment.package_id
                    ),
                )
            })?;
        let roots = BTreeMap::from([(operation.clone(), fact.clone())]);
        resolve_component_dependency_closure(&roots, component_facts, rule)?;
        components.insert(project_serving_component(fact, component_facts, rule)?);
        membership.insert(ReleaseComponentMembership {
            binding: MemberBinding::Route {
                component: component.clone(),
                operation: operation.clone(),
            },
            package_id: fact.scope.package_id.clone(),
            package_version: fact.scope.package_version.clone(),
            component_digest: fact.component_digest.clone(),
        });
        routes.insert(ServingRoute {
            package_id: attachment.package_id.clone(),
            component: component.clone(),
            operation: operation.clone(),
            kind,
        });
    }
    Ok(routes)
}

/// Refuse a wiring with no edges unless a registration names it.
///
/// A graph with no edges is a route. A registration keeps its one-node wiring
/// until the workflow epic moves registrations off wirings.
fn refuse_unregistered_one_node_wirings(
    one_node: &[ReleaseWiringTarget],
    registrations: &BTreeMap<String, ServingRegistration>,
) -> Result<(), MintManifestError> {
    for target in one_node {
        let registered = registrations.values().any(|registration| {
            registration.package_id == target.package_id
                && registration.wiring_id == target.wiring_id
                && registration.wiring_version == target.wiring_version
        });
        if !registered {
            return Err(MintManifestError::new(
                MintManifestErrorKind::Wiring,
                format!(
                    "wiring {}@{}::{}/{} has no edges; a graph with no edges is a route, so attach its operation as a route",
                    target.package_id,
                    target.package_version,
                    target.wiring_id,
                    target.wiring_version
                ),
            ));
        }
    }
    Ok(())
}

async fn freeze_release(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
    expected: &BTreeSet<ReleaseComponentMembership>,
    digest: &ManifestDigest,
    canonical_bytes: &[u8],
) -> Result<(), MintManifestError> {
    let observed = transaction
        .query(
            SELECT_RELEASE_COMPONENTS_SQL,
            &[&request.tenant_id, &request.effective_release_id],
        )
        .await
        .map_err(|error| storage("read the frozen release component closure", error))?
        .into_iter()
        .map(|row| {
            let binding = match (row.get(8), row.get(9)) {
                (Some(component), Some(operation)) => MemberBinding::Route {
                    component,
                    operation,
                },
                _ => MemberBinding::Wiring {
                    package_id: row.get(0),
                    package_version: row.get(1),
                    wiring_id: row.get(2),
                    wiring_version: positive_u32(row.get(3), "wiring-version")?,
                    node_id: row.get(4),
                },
            };
            Ok(ReleaseComponentMembership {
                binding,
                package_id: row.get(5),
                package_version: row.get(6),
                component_digest: row.get(7),
            })
        })
        .collect::<Result<BTreeSet<_>, MintManifestError>>()?;
    let snapshot = transaction
        .query_opt(
            SELECT_RELEASE_SNAPSHOT_SQL,
            &[&request.tenant_id, &request.effective_release_id],
        )
        .await
        .map_err(|error| storage("read the frozen format-1 snapshot", error))?;

    match (observed.is_empty(), snapshot) {
        (false, Some(snapshot)) => {
            let frozen_digest: String = snapshot.get(0);
            let frozen_bytes: Vec<u8> = snapshot.get(1);
            if observed != *expected
                || frozen_digest != digest.as_str()
                || frozen_bytes != canonical_bytes
            {
                return Err(MintManifestError::new(
                    MintManifestErrorKind::ClosureConflict,
                    "effective release is already frozen to another closure",
                ));
            }
            return Ok(());
        }
        (true, None) => {}
        _ => {
            return Err(MintManifestError::new(
                MintManifestErrorKind::ClosureConflict,
                "release membership and format-1 snapshot are partially frozen",
            ));
        }
    }

    for member in expected {
        let (wiring_package_id, wiring_package_version, wiring_id, wiring_version, node_id) =
            match &member.binding {
                MemberBinding::Wiring {
                    package_id,
                    package_version,
                    wiring_id,
                    wiring_version,
                    node_id,
                } => (
                    Some(package_id),
                    Some(package_version),
                    Some(wiring_id),
                    Some(
                        i32::try_from(*wiring_version)
                            .expect("resolved wiring version fits PostgreSQL integer"),
                    ),
                    Some(node_id),
                ),
                MemberBinding::Route { .. } => (None, None, None, None, None),
            };
        let (route_component, route_operation) = match &member.binding {
            MemberBinding::Route {
                component,
                operation,
            } => (Some(component), Some(operation)),
            MemberBinding::Wiring { .. } => (None, None),
        };
        transaction
            .execute(
                INSERT_RELEASE_COMPONENT_SQL,
                &[
                    &request.tenant_id,
                    &request.effective_release_id,
                    &wiring_package_id,
                    &wiring_package_version,
                    &wiring_id,
                    &wiring_version,
                    &node_id,
                    &member.package_id,
                    &member.package_version,
                    &member.component_digest,
                    &route_component,
                    &route_operation,
                ],
            )
            .await
            .map_err(|error| storage("freeze a release component member", error))?;
    }
    transaction
        .execute(
            INSERT_RELEASE_SNAPSHOT_SQL,
            &[
                &request.tenant_id,
                &request.effective_release_id,
                &digest.as_str(),
                &canonical_bytes,
            ],
        )
        .await
        .map_err(|error| storage("freeze the format-1 manifest", error))?;
    Ok(())
}

pub async fn read_release_snapshot(
    transaction: &Transaction<'_>,
    tenant_id: &str,
    effective_release_id: i32,
) -> Result<Option<Vec<u8>>, MintManifestError> {
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&tenant_id])
        .await
        .map_err(|error| storage("claim the release tenant", error))?;
    transaction
        .query_opt(
            READ_RELEASE_SNAPSHOT_SQL,
            &[&tenant_id, &effective_release_id],
        )
        .await
        .map(|row| row.map(|row| row.get(0)))
        .map_err(|error| storage("read the frozen format-1 snapshot", error))
}

fn positive_u32(value: i32, field: &'static str) -> Result<u32, MintManifestError> {
    let value = u32::try_from(value).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Release,
            format!("{field} is outside the serving-manifest width"),
            error,
        )
    })?;
    if value == 0 {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Release,
            format!("{field} must be greater than zero"),
        ));
    }
    Ok(value)
}

fn storage(context: &'static str, error: tokio_postgres::Error) -> MintManifestError {
    MintManifestError::with_source(MintManifestErrorKind::Storage, context, error)
}

#[cfg(test)]
mod effective_release_live;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn database_diagnostics_omit_row_detail_and_hint() -> anyhow::Result<()> {
        let _lock = wamn_test_postgres::lock();
        let database = wamn_test_postgres::database();
        let (client, connection) =
            tokio_postgres::connect(database.url(), tokio_postgres::NoTls).await?;
        let task = tokio::spawn(connection);
        client.batch_execute("CREATE TEMP TABLE private_diagnostic (payload text, valid bool CONSTRAINT diagnostic_valid CHECK (valid))").await?;
        let marker = "private-person@example.invalid";
        let error = client
            .execute(
                "INSERT INTO private_diagnostic VALUES ($1, false)",
                &[&marker],
            )
            .await
            .unwrap_err();
        assert!(
            error
                .as_db_error()
                .unwrap()
                .detail()
                .unwrap()
                .contains(marker)
        );
        let rendered = render_driver_failure(&error);
        assert!(rendered.contains("diagnostic_valid"));
        assert!(rendered.contains("violates check constraint"));
        assert!(!rendered.contains(marker));
        let error = client.batch_execute("DO $$ BEGIN RAISE EXCEPTION 'diagnostic message' USING DETAIL = 'private-detail-marker', HINT = 'private-hint-marker'; END $$").await.unwrap_err();
        let rendered = render_driver_failure(&error);
        assert!(rendered.contains("diagnostic message"));
        assert!(!rendered.contains("private-detail-marker"));
        assert!(!rendered.contains("private-hint-marker"));
        drop(client);
        task.await??;
        Ok(())
    }

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn refresh_definition_hash(attachment: &mut ServingAttachment) {
        attachment.definition_hash = wamn_catalog::DefinitionHash::parse(
            wamn_execution_contract::canonical_json_sha256(&attachment.definition),
        )
        .expect("the canonicalizer emits a valid definition hash");
    }

    fn package_http_attachment(
        attachment_id: &str,
        package_id: &str,
        wiring_id: &str,
        operation: &str,
        path: &str,
    ) -> ServingAttachment {
        let definition = serde_json::json!({
            "id": attachment_id,
            "kind": "http",
            "route": {"path": path, "method": "POST"},
        });
        let definition_hash = wamn_execution_contract::canonical_json_sha256(&definition);
        ServingAttachment {
            kind: wamn_catalog::AttachmentKind::Http,
            package_id: package_id.to_owned(),
            target: wamn_catalog::AttachmentTarget::Wiring {
                wiring_id: wiring_id.to_owned(),
                wiring_version: 1,
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
        let resolved = resolve_route_host_overlay(&authored, Some("Fixture.Localhost"))
            .expect("merged routes retain deployment-owned host binding");
        assert!(
            resolved
                .values()
                .all(|attachment| attachment.definition["route"]["host"] == "fixture.localhost")
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

        assert_eq!(error.kind(), MintManifestErrorKind::DuplicateAttachmentId);
        assert_eq!(error.kind().as_str(), "duplicate-attachment-id");
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
        let error = resolve_route_host_overlay(&authored, Some("fixture.localhost"))
            .expect_err("canonical route collisions remain refused after package merging");

        assert_eq!(error.kind(), MintManifestErrorKind::Document);
        assert!(error.detail().contains("canonical path and method"));
    }

    #[test]
    fn release_mint_binds_each_attachment_hash_to_its_definition() {
        let definition = serde_json::json!({
            "id": "fixture-http",
            "kind": "http",
            "route": {"path": "/widget/get", "method": "POST"},
        });
        let definition_hash = wamn_execution_contract::canonical_json_sha256(&definition);
        let attachment = ServingAttachment {
            kind: wamn_catalog::AttachmentKind::Http,
            package_id: "source_fixture".to_owned(),
            target: wamn_catalog::AttachmentTarget::Wiring {
                wiring_id: "widget_get".to_owned(),
                wiring_version: 1,
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
        assert_eq!(error.kind(), MintManifestErrorKind::Document);
    }

    #[test]
    fn release_mint_requires_and_applies_the_deployment_route_host() {
        let definition = serde_json::json!({
            "id": "fixture-http",
            "kind": "http",
            "route": {"path": "/widget/get", "method": "POST"},
        });
        let authored_hash = wamn_execution_contract::canonical_json_sha256(&definition);
        let attachment = ServingAttachment {
            kind: wamn_catalog::AttachmentKind::Http,
            package_id: "source_fixture".to_owned(),
            target: wamn_catalog::AttachmentTarget::Wiring {
                wiring_id: "widget_get".to_owned(),
                wiring_version: 1,
            },
            definition_hash: wamn_catalog::DefinitionHash::parse(authored_hash.clone())
                .expect("the canonicalizer emits a valid definition hash"),
            definition,
            auth_policy: serde_json::json!({"modes": ["pat"]}),
            registered_operation: Some("source-fixture:widget/get@1.0.0".to_owned()),
        };
        let authored = BTreeMap::from([("fixture-http".to_owned(), attachment)]);

        let missing = resolve_route_host_overlay(&authored, None)
            .expect_err("a routed release requires its deployment hostname");
        assert_eq!(missing.kind(), MintManifestErrorKind::RouteHostUnbound);
        assert_eq!(missing.kind().as_str(), "route-host-unbound");
        assert!(missing.detail().contains("fixture-http"));
        assert!(missing.detail().contains("--route-host"));

        let resolved = resolve_route_host_overlay(&authored, Some("Route.Example"))
            .expect("the deployment overlay resolves the route hostname");
        assert!(
            authored["fixture-http"].definition["route"]
                .get("host")
                .is_none()
        );
        assert_eq!(
            resolved["fixture-http"].definition["route"]["host"],
            "route.example"
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
        let package_host = resolve_route_host_overlay(&package_authored, None)
            .expect_err("package content cannot author a deployment hostname");
        assert_eq!(package_host.kind(), MintManifestErrorKind::Document);
        assert!(package_host.detail().contains("remove it"));
        assert!(package_host.detail().contains("--route-host"));

        let mut non_routed = package_authored;
        non_routed
            .get_mut("fixture-http")
            .expect("the attachment exists")
            .kind = wamn_catalog::AttachmentKind::Internal;
        let package_host = resolve_route_host_overlay(&non_routed, None)
            .expect_err("every attachment kind refuses an authored route hostname");
        assert_eq!(package_host.kind(), MintManifestErrorKind::Document);

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
        let extra = resolve_route_host_overlay(&extra_route_field, Some("route.example"))
            .expect_err("package route schema admits only path and method");
        assert_eq!(extra.kind(), MintManifestErrorKind::Document);
        assert!(extra.detail().contains("exactly string path and method"));

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
        let collision = resolve_route_host_overlay(&colliding, Some("route.example"))
            .expect_err("one overlay host cannot carry ambiguous route templates");
        assert_eq!(collision.kind(), MintManifestErrorKind::Document);
        assert!(collision.detail().contains("canonical path and method"));
    }

    fn closure_component(component: &str, registered_operation: Option<&str>) -> AdmittedComponent {
        let operation = registered_operation.unwrap_or("run");
        AdmittedComponent {
            scope: ComponentPackageScope {
                tenant_id: "tenant-a".to_owned(),
                package_id: "base".to_owned(),
                package_version: "1.0.0".to_owned(),
            },
            component: component.to_owned(),
            interface_version: "0.1.0".to_owned(),
            operations: BTreeMap::from([(
                operation.to_owned(),
                AdmittedComponentOperation {
                    pre_commit: None,
                    pre_commit_required: false,
                    committed_result_schema: None,
                    fresh_only: false,
                    registered_operation: registered_operation.map(str::to_owned),
                    dependencies: Vec::new(),
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
    fn fresh_only_component_policy_survives_release_projection() {
        let operation = "base:widget/get@1.0.0";
        let mut component = closure_component("registered-component", Some(operation));
        let baseline =
            project_serving_component(&component, &BTreeMap::new(), DependencyDigestRule::Declared)
                .unwrap();
        assert!(!baseline.operations[operation].fresh_only);
        component.operations.get_mut(operation).unwrap().fresh_only = true;
        let projected =
            project_serving_component(&component, &BTreeMap::new(), DependencyDigestRule::Declared)
                .unwrap();
        assert!(projected.operations[operation].fresh_only);
        assert_eq!(
            serde_json::to_value(&projected).unwrap()["operations"][operation]["fresh-only"],
            true
        );
        assert_eq!(
            projected.operations[operation]
                .registered_operation
                .as_deref(),
            Some(operation)
        );
    }

    fn closure_document(with_registered_edge: bool) -> WiringDocument {
        let edges = with_registered_edge
            .then(|| wamn_catalog::WiringEdge {
                from: "entry".to_owned(),
                from_port: wamn_execution_contract::MAIN_PORT.to_owned(),
                to: "registered".to_owned(),
                to_port: None,
            })
            .into_iter()
            .collect();
        WiringDocument::new(
            "stock",
            1,
            "entry",
            BTreeMap::from([
                (
                    "entry".to_owned(),
                    wamn_catalog::WiringNode {
                        component: "entry-component".to_owned(),
                        interface_version: "0.1.0".to_owned(),
                        operation: "run".to_owned(),
                        operation_dependency: None,
                        params: BTreeMap::new(),
                        terminal: None,
                    },
                ),
                (
                    "registered".to_owned(),
                    wamn_catalog::WiringNode {
                        component: "registered-component".to_owned(),
                        interface_version: "0.1.0".to_owned(),
                        operation: "base:widget/get@1.0.0".to_owned(),
                        operation_dependency: None,
                        params: BTreeMap::new(),
                        terminal: None,
                    },
                ),
            ]),
            edges,
            Vec::new(),
        )
        .expect("construct the exact wiring closure")
    }

    fn closure_attachment(mode: &str) -> ServingAttachment {
        ServingAttachment {
            kind: wamn_catalog::AttachmentKind::Http,
            package_id: "base".to_owned(),
            target: wamn_catalog::AttachmentTarget::Wiring {
                wiring_id: "stock".to_owned(),
                wiring_version: 1,
            },
            definition_hash: wamn_catalog::DefinitionHash::parse(DIGEST)
                .expect("fixture definition hash is canonical"),
            definition: serde_json::json!({"route": {}}),
            auth_policy: serde_json::json!({"modes": [mode]}),
            registered_operation: None,
        }
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

    #[test]
    fn release_mint_consumes_package_metadata_and_refuses_unsatisfied_policy() {
        let manifest_bytes = include_bytes!("../tests/fixtures/component_package/wamn.json");
        let manifest = wamn_schema_generator::PackageManifest::from_slice(manifest_bytes)
            .expect("the fixture manifest is valid");
        let metadata_bytes =
            include_bytes!("../tests/fixtures/component_package/generated/package-weld.json");
        let metadata = wamn_schema_generator::GeneratedPackageMetadata::from_slice(metadata_bytes)
            .expect("the fixture metadata is canonical");
        validate_package_metadata(&manifest, &metadata)
            .expect("the fixture manifest and metadata carry one satisfied policy fact");

        let mut unsatisfied_manifest: serde_json::Value =
            serde_json::from_slice(manifest_bytes).unwrap();
        unsatisfied_manifest["required_platform_policy_contract"]["state"] =
            serde_json::json!("unsatisfied");
        let unsatisfied_manifest = wamn_schema_generator::PackageManifest::from_slice(
            &serde_json::to_vec(&unsatisfied_manifest).unwrap(),
        )
        .unwrap();
        let mut unsatisfied_metadata: serde_json::Value =
            serde_json::from_slice(metadata_bytes).unwrap();
        unsatisfied_metadata["required_platform_policy_contract"]["state"] =
            serde_json::json!("unsatisfied");
        unsatisfied_metadata["promotion_state"] =
            serde_json::json!("blocked_unsatisfied_policy_contract");
        let unsatisfied_metadata = wamn_schema_generator::GeneratedPackageMetadata::from_slice(
            &wamn_execution_contract::canonical_json_bytes(&unsatisfied_metadata),
        )
        .unwrap();
        let refusal = validate_package_metadata(&unsatisfied_manifest, &unsatisfied_metadata)
            .expect_err("unsatisfied generated metadata cannot enter a release");
        assert_eq!(
            refusal.kind(),
            MintManifestErrorKind::PolicyContractUnsatisfied
        );
        assert!(refusal.detail().contains("fixture_data_access"));
        assert!(refusal.detail().contains("regenerate"));

        let mut mismatched_metadata: serde_json::Value =
            serde_json::from_slice(metadata_bytes).unwrap();
        mismatched_metadata["required_platform_policy_contract"]["id"] =
            serde_json::json!("different_policy");
        let mismatched_metadata = wamn_schema_generator::GeneratedPackageMetadata::from_slice(
            &wamn_execution_contract::canonical_json_bytes(&mismatched_metadata),
        )
        .unwrap();
        let refusal = validate_package_metadata(&manifest, &mismatched_metadata)
            .expect_err("metadata cannot restate the manifest policy requirement");
        assert_eq!(
            refusal.kind(),
            MintManifestErrorKind::GeneratedPackageMetadata
        );
    }

    fn handler_manifest() -> wamn_schema_generator::PackageManifest {
        serde_json::from_str(include_str!("../tests/fixtures/observer_package/wamn.json"))
            .expect("the fixture handler manifest parses")
    }

    fn source_manifest() -> wamn_schema_generator::PackageManifest {
        serde_json::from_str(include_str!(
            "../tests/fixtures/component_package/wamn.json"
        ))
        .expect("the fixture source manifest parses")
    }

    fn handler_manifest_with_entity(entity: &str) -> wamn_schema_generator::PackageManifest {
        let mut document = serde_json::to_value(handler_manifest())
            .expect("the fixture handler manifest serializes");
        document["custom_operations"]["audit.observe"]["registration"]["entity"] =
            serde_json::Value::String(entity.to_owned());
        serde_json::from_value(document).expect("the mutated handler manifest parses")
    }

    /// Render the fixture dependency from its authored digest.
    fn fixture_overlay_declaration() -> wamn_catalog::ComponentDeclaration {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/observer_package");
        let base_digests = crate::component_declaration::authored_base_digests(&root)
            .expect("the fixture overlay manifest authors its base digest");
        let document = crate::component_declaration::render_declaration_document(
            &root.join("component.json.in"),
            "tenant-a",
            &base_digests,
        )
        .expect("the repository component declaration renders");
        serde_json::from_value(document)
            .expect("the repository component declaration is structurally valid")
    }

    fn resolve_fixture_private_handler_entry() -> String {
        let declaration = fixture_overlay_declaration();
        let document_value = serde_json::from_str(include_str!(
            "../tests/fixtures/observer_package/wiring.json"
        ))
        .expect("the fixture handler wiring parses as JSON");
        let document = WiringDocument::parse(&document_value)
            .expect("the fixture handler wiring is structurally valid");
        let operation = document.nodes[&document.entry].operation.clone();
        let declared = declaration.operations[&operation].clone();
        assert!(
            declared.registered_operation.is_none(),
            "a private event handler must carry no public authorization token"
        );
        let admitted = AdmittedComponent {
            scope: declaration.scope.clone(),
            component: declaration.component,
            interface_version: declaration.interface_version,
            operations: BTreeMap::from([(
                operation.clone(),
                AdmittedComponentOperation {
                    pre_commit: None,
                    pre_commit_required: false,
                    committed_result_schema: None,
                    fresh_only: declared.fresh_only,
                    registered_operation: declared.registered_operation,
                    dependencies: declared.dependencies,
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
        };
        let facts = BTreeMap::from([(
            (
                declaration.scope.package_id.clone(),
                declaration.scope.package_version.clone(),
            ),
            vec![admitted],
        )]);
        let resolved = resolve_wiring_components(
            &document,
            &declaration.scope,
            &facts,
            Some(&handler_manifest()),
            DependencyDigestRule::Declared,
        )
        .expect("the repository private handler resolves through its admitted operation fact");
        validate_resolved_wiring_compatibility(&document, &resolved)
            .expect("the repository private handler wiring is compatible");
        resolved_wiring_entry_operation(&document, &resolved)
            .expect("the private export token remains the serving selector")
    }

    #[test]
    fn serving_registration_is_derived_from_the_exact_handler_and_unique_entry_wiring() {
        let manifest = handler_manifest();
        let manifests = BTreeMap::from([
            ("observer_fixture".to_owned(), manifest),
            ("source_fixture".to_owned(), source_manifest()),
        ]);
        let operation = resolve_fixture_private_handler_entry();
        assert_eq!(operation, "observer-fixture:audit/observe@3.0.0");
        let target = ReleaseWiringTarget {
            package_id: "observer_fixture".to_owned(),
            package_version: "3.0.0".to_owned(),
            wiring_id: "audit_observe".to_owned(),
            wiring_version: 1,
        };
        let targets = BTreeMap::from([(operation.clone(), vec![target.clone()])]);
        let registrations = derive_serving_registrations(&manifests, &targets)
            .expect("one entry wiring resolves the handler operation");
        let registration = &registrations["observer_fixture::audit.observe"];
        assert_eq!(registration.package_id, "observer_fixture");
        assert_eq!(registration.source_package_id, "source_fixture");
        assert_eq!(registration.wiring_id, target.wiring_id);
        assert_eq!(registration.entity, "item");
        assert_eq!(registration.ops, BTreeSet::from(["insert".to_owned()]));

        for selected in [Vec::new(), vec![target.clone(), target]] {
            let targets = BTreeMap::from([(operation.clone(), selected)]);
            let error = derive_serving_registrations(&manifests, &targets)
                .expect_err("zero or multiple handler entry wirings were accepted");
            assert_eq!(error.kind(), MintManifestErrorKind::Registration);
        }

        let manifests = BTreeMap::from([
            (
                "observer_fixture".to_owned(),
                handler_manifest_with_entity("missing"),
            ),
            ("source_fixture".to_owned(), source_manifest()),
        ]);
        let error = derive_serving_registrations(&manifests, &targets)
            .expect_err("a registration source entity absent from its package was accepted");
        assert_eq!(error.kind(), MintManifestErrorKind::Registration);
        for fact in ["audit.observe", "source_fixture", "missing"] {
            assert!(
                error.detail().contains(fact),
                "missing refusal fact {fact:?}"
            );
        }
    }

    fn dependency_document() -> WiringDocument {
        WiringDocument::new(
            "stock",
            1,
            "registered",
            BTreeMap::from([(
                "registered".to_owned(),
                wamn_catalog::WiringNode {
                    component: "registered-component".to_owned(),
                    interface_version: "0.1.0".to_owned(),
                    operation: "base:stock/record-widget@1.0.0".to_owned(),
                    operation_dependency: Some(wamn_catalog::WiringOperationDependency {
                        alias: "base".to_owned(),
                        operation: "stock.record_widget".to_owned(),
                    }),
                    params: BTreeMap::new(),
                    terminal: Some(wamn_catalog::WiringTerminal::Respond),
                },
            )]),
            Vec::new(),
            Vec::new(),
        )
        .expect("construct dependency wiring")
    }

    #[test]
    fn operation_dependency_resolves_the_exact_release_tuple_without_relabeling() {
        let owner = ComponentPackageScope {
            tenant_id: "tenant-a".to_owned(),
            package_id: "source_fixture".to_owned(),
            package_version: "1.0.0".to_owned(),
        };
        let mut base = closure_component(
            "registered-component",
            Some("base:stock/record-widget@1.0.0"),
        );
        base.scope.package_id = "base".to_owned();
        let mut local_same_name = closure_component(
            "registered-component",
            Some("source-fixture:widget/update@1.0.0"),
        );
        local_same_name.component_digest = format!("sha256:{}", "c".repeat(64));
        let facts = BTreeMap::from([
            (("base".to_owned(), "1.0.0".to_owned()), vec![base.clone()]),
            (
                ("source_fixture".to_owned(), "1.0.0".to_owned()),
                vec![local_same_name],
            ),
        ]);

        let resolved = resolve_wiring_components(
            &dependency_document(),
            &owner,
            &facts,
            Some(&dependency_manifest(&base.component_digest)),
            DependencyDigestRule::Declared,
        )
        .expect("the alias resolves its exact package, version, digest, and operation");

        assert_eq!(resolved["registered"], base);
        assert_eq!(resolved["registered"].scope.package_id, "base");
    }

    #[test]
    fn operation_dependency_refuses_digest_and_release_membership_drift() {
        let owner = ComponentPackageScope {
            tenant_id: "tenant-a".to_owned(),
            package_id: "source_fixture".to_owned(),
            package_version: "1.0.0".to_owned(),
        };
        let mut base = closure_component(
            "registered-component",
            Some("base:stock/record-widget@1.0.0"),
        );
        base.scope.package_id = "base".to_owned();
        let facts = BTreeMap::from([(("base".to_owned(), "1.0.0".to_owned()), vec![base])]);

        let digest_error = resolve_wiring_components(
            &dependency_document(),
            &owner,
            &facts,
            Some(&dependency_manifest(&format!("sha256:{}", "d".repeat(64)))),
            DependencyDigestRule::Declared,
        )
        .expect_err("digest drift refuses the dependency");
        assert_eq!(
            digest_error.kind(),
            MintManifestErrorKind::OperationDependency
        );

        let membership_error = resolve_wiring_components(
            &dependency_document(),
            &owner,
            &BTreeMap::new(),
            Some(&dependency_manifest(DIGEST)),
            DependencyDigestRule::Declared,
        )
        .expect_err("a dependency outside the release refuses publication");
        assert_eq!(
            membership_error.kind(),
            MintManifestErrorKind::OperationDependency
        );
        assert!(
            membership_error
                .detail()
                .contains("absent from the effective release")
        );
    }

    /// The other half of wamn-10yt.48, at the stage the Virtualize fix moved
    /// the wall to. An author edits a base package, the built digest moves, and
    /// the overlay's authored pin still names the old bytes. A durable release
    /// refuses that, above. A disposable target built both halves from this
    /// same tree, so it resolves the dependency by coordinate and operation.
    /// Release membership is NOT relaxed: a dependency outside the release
    /// still refuses, because that is a different fact.
    #[test]
    fn a_disposable_target_resolves_a_moved_base_digest_and_still_demands_membership() {
        let owner = ComponentPackageScope {
            tenant_id: "tenant-a".to_owned(),
            package_id: "source_fixture".to_owned(),
            package_version: "1.0.0".to_owned(),
        };
        let mut base = closure_component(
            "registered-component",
            Some("base:stock/record-widget@1.0.0"),
        );
        base.scope.package_id = "base".to_owned();
        let facts = BTreeMap::from([(("base".to_owned(), "1.0.0".to_owned()), vec![base.clone()])]);
        let stale_pin = format!("sha256:{}", "d".repeat(64));
        assert_ne!(stale_pin, base.component_digest);

        let resolved = resolve_wiring_components(
            &dependency_document(),
            &owner,
            &facts,
            Some(&dependency_manifest(&stale_pin)),
            DependencyDigestRule::Built,
        )
        .expect("a disposable target resolves the base it just built");
        assert_eq!(resolved["registered"], base);

        let membership_error = resolve_wiring_components(
            &dependency_document(),
            &owner,
            &BTreeMap::new(),
            Some(&dependency_manifest(&stale_pin)),
            DependencyDigestRule::Built,
        )
        .expect_err("a dependency outside the release refuses on either rule");
        assert_eq!(
            membership_error.kind(),
            MintManifestErrorKind::OperationDependency
        );
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

    #[test]
    fn component_dependencies_expand_the_exact_release_closure_and_refuse_cycles() {
        let base_operation = "base:stock/record-widget@1.0.0";
        let overlay_operation = "overlay:stock/record-widget@3.0.0";
        let mut base = closure_component("base-component", Some(base_operation));
        base.scope.package_id = "base".to_owned();
        let mut overlay = closure_component("overlay-component", Some(overlay_operation));
        overlay.scope.package_id = "overlay".to_owned();
        overlay.scope.package_version = "3.0.0".to_owned();
        overlay.component_digest = format!("sha256:{}", "b".repeat(64));
        overlay
            .operations
            .get_mut(overlay_operation)
            .expect("the overlay operation exists")
            .dependencies = vec![wamn_catalog::ComponentOperationDependency {
            participant: None,
            package: "base".to_owned(),
            version: "1.0.0".to_owned(),
            digest: base.component_digest.clone(),
            operation: base_operation.to_owned(),
        }];
        let roots = BTreeMap::from([("entry".to_owned(), overlay.clone())]);
        let facts = BTreeMap::from([
            (("base".to_owned(), "1.0.0".to_owned()), vec![base.clone()]),
            (
                ("overlay".to_owned(), "3.0.0".to_owned()),
                vec![overlay.clone()],
            ),
        ]);

        let closure =
            resolve_component_dependency_closure(&roots, &facts, DependencyDigestRule::Declared)
                .expect("the exact dependency expands the release component closure");
        assert_eq!(closure.len(), 2);
        assert!(closure.contains(&base));
        assert!(closure.contains(&overlay));
        let folded = project_serving_component(&overlay, &facts, DependencyDigestRule::Declared)
            .expect("the composed overlay folds its base");
        assert_eq!(
            folded.operations[overlay_operation].permissions,
            BTreeSet::from([base_operation.to_owned(), overlay_operation.to_owned()]),
            "the release lists the overlay alone, with its base's authority folded in"
        );

        base.operations
            .get_mut(base_operation)
            .expect("the base operation exists")
            .dependencies = vec![wamn_catalog::ComponentOperationDependency {
            participant: None,
            package: "overlay".to_owned(),
            version: "3.0.0".to_owned(),
            digest: overlay.component_digest.clone(),
            operation: overlay_operation.to_owned(),
        }];
        let cyclic = BTreeMap::from([
            (("base".to_owned(), "1.0.0".to_owned()), vec![base]),
            (("overlay".to_owned(), "3.0.0".to_owned()), vec![overlay]),
        ]);
        let error =
            resolve_component_dependency_closure(&roots, &cyclic, DependencyDigestRule::Declared)
                .expect_err("an exact component dependency cycle was accepted");
        assert_eq!(error.kind(), MintManifestErrorKind::OperationDependency);
        assert!(error.detail().contains("cycle"));
    }

    /// The SECOND authored pin wamn-10yt.48 found. The overlay's component
    /// declaration under `publication/components/` names its base dependency by
    /// digest, and Generate never rewrites that file, so the closure walker
    /// hits the same wall the wiring resolver does. One rule governs both.
    #[test]
    fn a_disposable_target_expands_a_closure_whose_declared_dependency_digest_moved() {
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
        overlay
            .operations
            .get_mut(overlay_operation)
            .expect("the overlay operation exists")
            .dependencies = vec![wamn_catalog::ComponentOperationDependency {
            participant: None,
            package: "base".to_owned(),
            version: "1.0.0".to_owned(),
            digest: stale_pin,
            operation: base_operation.to_owned(),
        }];
        let roots = BTreeMap::from([("entry".to_owned(), overlay.clone())]);
        let facts = BTreeMap::from([
            (("base".to_owned(), "1.0.0".to_owned()), vec![base.clone()]),
            (
                ("overlay".to_owned(), "3.0.0".to_owned()),
                vec![overlay.clone()],
            ),
        ]);

        let refusal =
            resolve_component_dependency_closure(&roots, &facts, DependencyDigestRule::Declared)
                .expect_err("a durable release still demands the declared bytes");
        assert_eq!(refusal.kind(), MintManifestErrorKind::OperationDependency);

        let closure =
            resolve_component_dependency_closure(&roots, &facts, DependencyDigestRule::Built)
                .expect("a disposable target expands the closure it just built");
        assert_eq!(closure.len(), 2);
        assert!(closure.contains(&base));
        assert!(closure.contains(&overlay));
    }

    /// Read the fixture dependency used by effect-projection tests.
    fn fixture_overlay_dependency() -> (
        wamn_catalog::ComponentDeclaration,
        wamn_catalog::ComponentOperationDependency,
    ) {
        let declaration = fixture_overlay_declaration();
        let mut declared = declaration
            .operations
            .values()
            .flat_map(|operation| operation.dependencies.iter());
        let dependency = declared
            .next()
            .expect("the fixture overlay declares an operation dependency")
            .clone();
        assert!(
            declared.next().is_none(),
            "the fixture overlay declares exactly one operation dependency"
        );
        (declaration, dependency)
    }

    /// The admitted fact the declared dependency resolves to, carrying the
    /// effect projection under test.
    fn dependency_facts(
        dependency: &wamn_catalog::ComponentOperationDependency,
        effects: Vec<AdmittedComponentEffect>,
    ) -> BTreeMap<(String, String), Vec<AdmittedComponent>> {
        let mut admitted = closure_component("stock", Some(dependency.operation.as_str()));
        admitted.scope.package_id = dependency.package.clone();
        admitted.scope.package_version = dependency.version.clone();
        admitted.component_digest = dependency.digest.clone();
        admitted.effects = effects;
        BTreeMap::from([(
            (dependency.package.clone(), dependency.version.clone()),
            vec![admitted],
        )])
    }

    /// The case the caller wiring exists to serve. The dependency's admitted
    /// row carries an empty effects array, so the caller identifies the dependency as
    /// effect-free and admission keeps the effect-free case path. The
    /// admission half of that test is
    /// `a_wrapper_whose_whole_closure_is_effect_free_keeps_the_effect_free_case_path`
    /// in `wamn_engine::component_admission`.
    #[test]
    fn a_dependency_admitted_with_no_effects_keeps_the_effect_free_case_path() {
        let (declaration, dependency) = fixture_overlay_dependency();
        let facts = dependency_facts(&dependency, Vec::new());

        let dependencies = effect_free_operation_dependencies(
            &declaration,
            &facts,
            DependencyDigestRule::Declared,
        );

        assert_eq!(dependencies, BTreeSet::from([dependency.operation]));
    }

    /// The negative control uses the same
    /// declaration and the same lookup, except the dependency's admitted row
    /// carries the effect it really holds. The caller returns no dependency, so the
    /// wrapper takes the dependency package into its own projection and loses
    /// the effect-free case path.
    #[test]
    fn a_dependency_admitted_with_one_effect_loses_the_effect_free_case_path() {
        let (declaration, dependency) = fixture_overlay_dependency();
        let facts = dependency_facts(
            &dependency,
            vec![AdmittedComponentEffect {
                package: "wamn:postgres".to_owned(),
                provenance: wamn_catalog::ComponentEffectProvenance::Imported,
                interfaces: vec!["client".to_owned()],
            }],
        );

        let dependencies = effect_free_operation_dependencies(
            &declaration,
            &facts,
            DependencyDigestRule::Declared,
        );

        assert!(
            dependencies.is_empty(),
            "an effectful dependency was classified as pure: {dependencies:?}"
        );
    }

    #[test]
    fn release_projection_preserves_operation_scoped_statement_facts() {
        let operation = "base:widget/get@1.0.0";
        let sql = "SELECT row_version FROM widget WHERE id = $1";
        let digest = sha256(sql.as_bytes());
        let mut admitted = closure_component("base-component", Some(operation));
        admitted
            .operations
            .get_mut(operation)
            .expect("fixture has the registered operation")
            .statements
            .insert(
                digest.clone(),
                wamn_catalog::ComponentSqlStatement {
                    name: "get".to_owned(),
                    path: "generated/sql/widget/get.sql".to_owned(),
                    sql: sql.to_owned(),
                    binds: Vec::new(),
                    columns: Vec::new(),
                    transactional: false,
                },
            );

        let serving =
            project_serving_component(&admitted, &BTreeMap::new(), DependencyDigestRule::Declared)
                .expect("an admitted component projects to serving facts");

        assert_eq!(
            serving.operations[operation].statement(&digest),
            admitted.operations[operation].statement(&digest)
        );
    }

    #[test]
    fn release_mint_refuses_an_anonymous_path_to_a_registered_operation() {
        let target = ReleaseWiringTarget {
            package_id: "base".to_owned(),
            package_version: "1.0.0".to_owned(),
            wiring_id: "stock".to_owned(),
            wiring_version: 1,
        };
        let facts = BTreeMap::from([
            (
                "entry".to_owned(),
                closure_component("entry-component", None),
            ),
            (
                "registered".to_owned(),
                closure_component("registered-component", Some("base:widget/get@1.0.0")),
            ),
        ]);
        let anonymous = BTreeMap::from([(
            "fixture-http".to_owned(),
            closure_attachment(wamn_catalog::NO_AUTHENTICATION_MODE),
        )]);
        let error =
            validate_anonymous_wiring_closure(&anonymous, &target, &closure_document(true), &facts)
                .expect_err("anonymous reachability must fail at release mint");
        assert_eq!(
            error.kind(),
            MintManifestErrorKind::UnauthenticatedRegisteredOperation
        );
        assert_eq!(
            error.detail(),
            "attachment \"fixture-http\" reaches registered operation \
             \"base:widget/get@1.0.0\" at node \"registered\"; set \
             auth-policy modes = [\"pat\"]"
        );

        let mut dependency_facts = facts.clone();
        dependency_facts
            .get_mut("entry")
            .unwrap()
            .operations
            .get_mut("run")
            .unwrap()
            .dependencies = vec![wamn_catalog::ComponentOperationDependency {
            participant: None,
            package: "base".to_owned(),
            version: "1.0.0".to_owned(),
            digest: DIGEST.to_owned(),
            operation: "base:widget/get@1.0.0".to_owned(),
        }];
        let error = validate_anonymous_wiring_closure(
            &anonymous,
            &target,
            &closure_document(false),
            &dependency_facts,
        )
        .expect_err("anonymous dependency reachability must fail at release mint");
        assert_eq!(
            error.kind(),
            MintManifestErrorKind::UnauthenticatedRegisteredOperation
        );
        assert!(error.detail().contains("through component dependency"));

        assert!(
            validate_anonymous_wiring_closure(
                &anonymous,
                &target,
                &closure_document(false),
                &facts,
            )
            .is_ok(),
            "a disconnected registered component is not a reachable path"
        );
        for modes in [
            serde_json::json!(["pat"]),
            serde_json::json!(["session"]),
            serde_json::json!(["pat", "session"]),
        ] {
            let mut attachment = closure_attachment(wamn_catalog::PAT_AUTHENTICATION_MODE);
            attachment.auth_policy = serde_json::json!({"modes": modes});
            let protected = BTreeMap::from([("fixture-http".to_owned(), attachment)]);
            assert!(
                validate_anonymous_wiring_closure(
                    &protected,
                    &target,
                    &closure_document(true),
                    &facts,
                )
                .is_ok(),
                "authenticated modes satisfy the anonymous-closure guard"
            );
        }
    }

    /// Spec test 14: an anonymous attachment cannot reach a statement that
    /// writes or locks, and it still reaches a statement that only reads.
    #[test]
    fn release_mint_refuses_an_anonymous_closure_that_can_write() {
        let target = ReleaseWiringTarget {
            package_id: "base".to_owned(),
            package_version: "1.0.0".to_owned(),
            wiring_id: "stock".to_owned(),
            wiring_version: 1,
        };
        let anonymous = BTreeMap::from([(
            "fixture-http".to_owned(),
            closure_attachment(wamn_catalog::NO_AUTHENTICATION_MODE),
        )]);
        let closure = |transactional: bool| {
            let sql = if transactional {
                "UPDATE widget SET note = $1"
            } else {
                "SELECT note FROM widget"
            };
            let mut entry = closure_component("entry-component", None);
            entry
                .operations
                .get_mut("run")
                .expect("fixture has the run operation")
                .statements
                .insert(
                    sha256(sql.as_bytes()),
                    wamn_catalog::ComponentSqlStatement {
                        name: "note".to_owned(),
                        path: "sql/note.sql".to_owned(),
                        sql: sql.to_owned(),
                        binds: Vec::new(),
                        columns: Vec::new(),
                        transactional,
                    },
                );
            BTreeMap::from([("entry".to_owned(), entry)])
        };

        let error = validate_anonymous_wiring_closure(
            &anonymous,
            &target,
            &closure_document(false),
            &closure(true),
        )
        .expect_err("an anonymous closure that can write must fail at release mint");
        assert_eq!(error.kind().as_str(), "unauthenticated-write");
        assert_eq!(
            error.detail(),
            "attachment \"fixture-http\" reaches transactional statement \"note\" at \
             node \"entry\"; set auth-policy modes = [\"pat\"]"
        );

        assert!(
            validate_anonymous_wiring_closure(
                &anonymous,
                &target,
                &closure_document(false),
                &closure(false),
            )
            .is_ok(),
            "an anonymous closure that only reads is admitted"
        );
    }

    #[test]
    fn package_and_wiring_coordinates_are_exact() {
        let package = parse_package("source_fixture@1.0.0").unwrap();
        assert_eq!(package.package_id(), "source_fixture");
        assert_eq!(package.package_version(), "1.0.0");

        let wiring = "source_fixture@1.0.0::stock=2"
            .parse::<ReleaseWiringTarget>()
            .unwrap();
        assert_eq!(wiring.package_id, "source_fixture");
        assert_eq!(wiring.package_version, "1.0.0");
        assert_eq!(wiring.wiring_id, "stock");
        assert_eq!(wiring.wiring_version, 2);
        assert!(
            "source_fixture::stock=2"
                .parse::<ReleaseWiringTarget>()
                .is_err()
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
        let release = ServingRelease {
            tenant_id: "tenant-a".to_owned(),
            effective_release_id: EffectiveReleaseId::new(1).unwrap(),
            environment: "prod".to_owned(),
            packages: BTreeSet::from([PackageCoordinate::new("app", "1.0.0").unwrap()]),
        };
        let schema = BareSchemaName::new("wamn_run").unwrap();
        assert_eq!(
            verify_projected_environment_policy(None, &source_policy, &release, &schema)
                .unwrap_err()
                .kind(),
            MintManifestErrorKind::EnvironmentPolicyAbsent
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
            .kind(),
            MintManifestErrorKind::EnvironmentPolicyMismatch
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
            mismatch.kind(),
            MintManifestErrorKind::EnvironmentPolicySourceMismatch
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

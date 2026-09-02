//! Mint one immutable format-3 effective-release closure.
//!
//! A release is an independent integer identity plus exact package membership.
//! The publisher resolves every wiring and component from those package pairs,
//! freezes the relational closure and canonical manifest in one transaction,
//! then projects only the release identity to the control plane so a later
//! deployment attestation can reference it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use clap::Args;
use serde::de::DeserializeOwned;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentEffect, AdmittedComponentOperation, ArtifactHash,
    ComponentPackageScope, EffectiveReleaseId, ManifestDigest, PackageCoordinate,
    SERVING_MANIFEST_FORMAT_VERSION, ServingAttachment, ServingComponent,
    ServingComponentOperation, ServingManifest, ServingRegistration, ServingRelease, ServingWiring,
    WiringDocument, validate_resolved_wiring_compatibility,
};
use wamn_control_registry::Triple;
use wamn_schema_control::{
    BareSchemaName, HttpRoute as AuthoredHttpRoute, canonical_http_route_template,
    normalize_http_route,
};

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
       package_id, package_version, component_digest \
  FROM catalog.release_components \
 WHERE tenant_id = $1 AND effective_release_id = $2 \
 ORDER BY wiring_package_id COLLATE \"C\", wiring_package_version COLLATE \"C\", \
          wiring_id COLLATE \"C\", wiring_version, node_id COLLATE \"C\" FOR SHARE";
const INSERT_RELEASE_COMPONENT_SQL: &str = "\
INSERT INTO catalog.release_components (\
       tenant_id, effective_release_id, wiring_package_id, wiring_package_version, \
       wiring_id, wiring_version, node_id, package_id, package_version, component_digest\
     ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)";
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
    PackageWeld,
    PolicyContractUnsatisfied,
    Wiring,
    Component,
    OperationDependency,
    UnauthenticatedRegisteredOperation,
    Registration,
    ClosureConflict,
    Document,
    RouteHostUnbound,
    EnvironmentPolicyAbsent,
    EnvironmentPolicyMismatch,
}

impl MintManifestErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Release => "release",
            Self::PackageManifest => "package-manifest",
            Self::PackageWeld => "package-weld",
            Self::PolicyContractUnsatisfied => "policy-contract-unsatisfied",
            Self::Wiring => "wiring",
            Self::Component => "component",
            Self::OperationDependency => "operation-dependency",
            Self::UnauthenticatedRegisteredOperation => "unauthenticated-registered-operation",
            Self::Registration => "registration",
            Self::ClosureConflict => "closure-conflict",
            Self::Document => "document",
            Self::RouteHostUnbound => "route-host-unbound",
            Self::EnvironmentPolicyAbsent => "environment-policy-not-converged",
            Self::EnvironmentPolicyMismatch => "environment-policy-environment-mismatch",
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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ReleaseComponentMembership {
    wiring_package_id: String,
    wiring_package_version: String,
    wiring_id: String,
    wiring_version: u32,
    node_id: String,
    package_id: String,
    package_version: String,
    component_digest: String,
}

#[derive(Debug, Args)]
pub struct PublishReleaseArgs {
    #[arg(long)]
    pub database_url: String,
    #[arg(long)]
    pub control_database_url: String,
    #[arg(long)]
    pub org: String,
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub tenant: String,
    #[arg(long)]
    pub effective_release_id: u32,
    #[arg(long)]
    pub environment: String,
    /// Principal already authenticated by the publication boundary.
    #[arg(long)]
    pub verified_publisher_principal: String,
    #[arg(long)]
    pub run_schema: String,
    /// Exact package membership; repeat once per package.
    #[arg(long = "package", value_parser = parse_package, required = true)]
    pub packages: Vec<PackageCoordinate>,
    /// Exact package-owned wiring; repeat once per wiring.
    #[arg(
        long = "wiring",
        value_name = "PACKAGE@VERSION::WIRING=VERSION",
        required = true
    )]
    pub wirings: Vec<ReleaseWiringTarget>,
    #[arg(long)]
    pub attachments: PathBuf,
    /// Deployment-owned hostname applied to every HTTP route.
    #[arg(long)]
    pub route_host: Option<String>,
    /// Exact `wamn.json` for every package in the release.
    #[arg(long = "package-manifest", value_name = "PATH", required = true)]
    pub package_manifests: Vec<PathBuf>,
}

fn parse_package(value: &str) -> Result<PackageCoordinate, String> {
    let (package_id, package_version) = value
        .rsplit_once('@')
        .ok_or_else(|| "expected PACKAGE_ID@PACKAGE_VERSION".to_owned())?;
    PackageCoordinate::new(package_id, package_version).map_err(|error| error.to_string())
}

impl PublishReleaseArgs {
    fn deployment_coordinate(&self, release: &ServingRelease) -> DeploymentCoordinate {
        DeploymentCoordinate::new(&self.org, &self.project, release)
    }

    fn verified_run_schema(&self) -> anyhow::Result<BareSchemaName> {
        BareSchemaName::new(self.run_schema.clone())
            .map_err(|error| anyhow::anyhow!("invalid --run-schema {:?}: {error}", self.run_schema))
    }
}

pub async fn run(args: PublishReleaseArgs) -> anyhow::Result<()> {
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
    let authored_attachments = read_document(&args.attachments, "attachments")?;
    let attachments =
        resolve_route_host_overlay(&authored_attachments, args.route_host.as_deref())?;
    let (package_manifests, package_manifest_hashes) =
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
    let request = MintReleaseManifest {
        tenant_id: &args.tenant,
        effective_release_id: release_id,
        environment: &args.environment,
        verified_publisher_principal: &args.verified_publisher_principal,
        packages: &packages,
        wirings: &wirings,
        attachments: &attachments,
    };
    let run_schema = args.verified_run_schema()?;

    let (mut client, connection) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect to the release project environment")?;
    let connection_task = tokio::spawn(connection);
    let minted = mint_in_transaction(
        &mut client,
        &request,
        &package_manifests,
        &package_manifest_hashes,
        &run_schema,
    )
    .await;
    match minted {
        Ok(minted) => {
            drop(client);
            connection_task
                .await
                .context("join the release mint connection")?
                .context("drive the release mint connection")?;
            let coordinate = args.deployment_coordinate(&minted.manifest.release);
            report_deployment_coordinate(&coordinate, &minted.digest);
            project_release_identity(&args.control_database_url, &coordinate).await?;
            println!("{}", minted.digest);
            Ok(())
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
    run_schema: &BareSchemaName,
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
    )
    .await?;
    let expected = read_expected_environment(&transaction, run_schema, request.tenant_id).await?;
    verify_provisioned_environment(expected.as_deref(), &minted.manifest.release, run_schema)?;
    transaction
        .commit()
        .await
        .context("commit the release mint")?;
    Ok(minted)
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
        return Err(MintManifestError::new(
            MintManifestErrorKind::EnvironmentPolicyAbsent,
            format!(
                "tenant {:?} has no row in {}.environment_policies; run reconcile-run-plane",
                release.tenant_id,
                run_schema.as_str()
            ),
        ));
    };
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

pub(crate) fn report_deployment_coordinate(
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

fn render_driver_failure(error: &tokio_postgres::Error) -> String {
    match error.as_db_error() {
        Some(db_error) => format!("{error}: {db_error}"),
        None => error.to_string(),
    }
}

async fn execute_claimed(
    control: &mut Client,
    tenant_id: &str,
    statement: &wamn_schema_control::SqlStatement,
) -> Result<(), tokio_postgres::Error> {
    let transaction = control.transaction().await?;
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&tenant_id])
        .await?;
    let params = crate::sql_params::as_postgres(&statement.params);
    transaction.execute(statement.sql.as_str(), &params).await?;
    transaction.commit().await
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
        execute_claimed(control, identity.tenant_id, &statement)
            .await
            .map_err(|error| {
                anyhow::Error::new(
                    wamn_schema_control::attestation::translate_projection_failure(
                        &identity,
                        error.code().map(tokio_postgres::error::SqlState::code),
                        &render_driver_failure(&error),
                    ),
                )
            })
    })
    .await
}

pub async fn attest_deployment(
    control_database_url: &str,
    coordinate: &DeploymentCoordinate,
    manifest_hash: &ManifestDigest,
) -> anyhow::Result<String> {
    let effective_release_id = i32::try_from(coordinate.effective_release_id)
        .context("effective-release-id exceeds PostgreSQL integer")?;
    on_control_plane(control_database_url, async |control| {
        let proposed_attested_at: String = control
            .query_one("SELECT clock_timestamp()::text", &[])
            .await
            .context("read the proposed control database attestation instant")?
            .get(0);
        let attestation = wamn_schema_control::attestation::Attestation {
            tenant_id: &coordinate.tenant_id,
            effective_release_id,
            org_id: &coordinate.triple.org,
            project_id: &coordinate.triple.project,
            environment: coordinate.triple.env.as_str(),
            deployed_manifest_hash: manifest_hash.as_str(),
            attested_at: &proposed_attested_at,
        };
        let statement = wamn_schema_control::attestation::register_attestation(&attestation);
        let recorded: Result<chrono::DateTime<chrono::Utc>, tokio_postgres::Error> = async {
            let transaction = control.transaction().await?;
            transaction
                .query_one(CLAIM_TENANT_SQL, &[&attestation.tenant_id])
                .await?;
            let params = crate::sql_params::as_postgres(&statement.params);
            let winner = transaction
                .query_one(statement.sql.as_str(), &params)
                .await?;
            let winner = winner.try_get(0)?;
            transaction.commit().await?;
            Ok(winner)
        }
        .await;
        let winner = recorded.map_err(|error| {
            anyhow::Error::new(wamn_schema_control::attestation::translate_failure(
                &attestation,
                error.code().map(tokio_postgres::error::SqlState::code),
                &render_driver_failure(&error),
            ))
        })?;
        Ok(winner.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
    })
    .await
}

fn read_document<T: DeserializeOwned>(path: &Path, field: &'static str) -> anyhow::Result<T> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read release {field} {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse release {field} {}", path.display()))
}

fn read_package_manifests(
    paths: &[PathBuf],
) -> Result<
    (
        BTreeMap<String, wamn_schema_generator::PackageManifest>,
        BTreeMap<String, String>,
    ),
    MintManifestError,
> {
    let mut manifests = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    for path in paths {
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
        wamn_schema_generator::validate_operation_vocabulary(&manifest).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::PackageManifest,
                format!("validate package manifest {}", path.display()),
                error,
            )
        })?;
        let root = path.parent().ok_or_else(|| {
            MintManifestError::new(
                MintManifestErrorKind::PackageWeld,
                format!(
                    "package manifest {} has no package directory; pass package-owned wamn.json",
                    path.display()
                ),
            )
        })?;
        let weld_path = root.join("generated/package-weld.json");
        let weld_bytes = std::fs::read(&weld_path).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::PackageWeld,
                format!(
                    "package {}@{} requires generated/package-weld.json at {}; regenerate the package evidence",
                    manifest.package.id,
                    manifest.package.version,
                    weld_path.display()
                ),
                error,
            )
        })?;
        let weld = wamn_schema_generator::PackageWeld::from_slice(&weld_bytes).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::PackageWeld,
                format!(
                    "package {}@{} carries an invalid generated/package-weld.json; regenerate the package evidence",
                    manifest.package.id, manifest.package.version
                ),
                error,
            )
        })?;
        validate_package_weld(&manifest, &weld)?;
        let package_id = manifest.package.id.clone();
        if manifests.insert(package_id.clone(), manifest).is_some() {
            return Err(MintManifestError::new(
                MintManifestErrorKind::PackageManifest,
                format!("more than one package manifest names {package_id:?}"),
            ));
        }
        hashes.insert(package_id, sha256(&bytes));
    }
    Ok((manifests, hashes))
}

fn validate_package_weld(
    manifest: &wamn_schema_generator::PackageManifest,
    weld: &wamn_schema_generator::PackageWeld,
) -> Result<(), MintManifestError> {
    let coordinate = format!("{}@{}", manifest.package.id, manifest.package.version);
    if weld.required_platform_policy_contract() != &manifest.required_platform_policy_contract {
        return Err(MintManifestError::new(
            MintManifestErrorKind::PackageWeld,
            format!(
                "package {coordinate} manifest and generated weld disagree on the required platform policy contract; regenerate the package evidence"
            ),
        ));
    }
    if !weld.promotion_eligible() {
        return Err(MintManifestError::new(
            MintManifestErrorKind::PolicyContractUnsatisfied,
            format!(
                "package {coordinate} requires platform policy contract {:?} in state unsatisfied; reconcile its generated policy and regenerate with state satisfied",
                manifest.required_platform_policy_contract.id
            ),
        ));
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
    )
}

pub async fn mint_promoted_release_manifest(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
    registrations: &BTreeMap<String, ServingRegistration>,
) -> Result<MintedReleaseManifest, MintManifestError> {
    let package_manifests = BTreeMap::new();
    mint_release_manifest_from_sources(
        transaction,
        request,
        &package_manifests,
        None,
        Some(registrations),
    )
    .await
}

async fn mint_release_manifest_with_package_manifests(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    package_manifest_hashes: &BTreeMap<String, String>,
) -> Result<MintedReleaseManifest, MintManifestError> {
    mint_release_manifest_from_sources(
        transaction,
        request,
        package_manifests,
        Some(package_manifest_hashes),
        None,
    )
    .await
}

async fn mint_release_manifest_from_sources(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
    package_manifests: &BTreeMap<String, wamn_schema_generator::PackageManifest>,
    package_manifest_hashes: Option<&BTreeMap<String, String>>,
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
        )
        .await?;
        entry_targets
            .entry(entry_operation)
            .or_default()
            .push(target.clone());
    }

    let registrations = if let Some(registrations) = promoted_registrations {
        registrations.clone()
    } else {
        derive_serving_registrations(package_manifests, &entry_targets)?
    };

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
                    "effective release {} does not project a deliverable format-3 manifest",
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
    if request.wirings.is_empty() {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Wiring,
            "a release with no wiring has no executable closure",
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

/// Require every attachment hash to identify its exact canonical definition.
///
/// The release mint is the production boundary that accepts the authored
/// attachment map. Comparing here prevents a caller from pairing an unchanged
/// identity claim with different route or contract bytes before either can
/// enter the immutable serving snapshot.
fn validate_attachment_definition_hashes(
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
fn resolve_route_host_overlay(
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
    stored: String,
    component: &str,
    field: &'static str,
) -> Result<T, MintManifestError> {
    serde_json::from_str(&stored).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Component,
            format!("component {component:?} stores unreadable {field}"),
            error,
        )
    })
}

fn resolve_wiring_components(
    document: &WiringDocument,
    owner: &ComponentPackageScope,
    component_facts: &BTreeMap<(String, String), Vec<AdmittedComponent>>,
    package_manifest: Option<&wamn_schema_generator::PackageManifest>,
) -> Result<BTreeMap<String, AdmittedComponent>, MintManifestError> {
    let mut resolved = BTreeMap::new();
    for (node_id, node) in &document.nodes {
        let (package_id, package_version, digest, registered_operation) = match &node
            .operation_dependency
        {
            None => (
                owner.package_id.as_str(),
                owner.package_version.as_str(),
                None,
                None,
            ),
            Some(dependency) => {
                let manifest = package_manifest.ok_or_else(|| {
                        MintManifestError::new(
                            MintManifestErrorKind::OperationDependency,
                            format!(
                                "wiring node {node_id:?} invokes dependency alias {:?}, but package {}@{} supplied no package manifest",
                                dependency.alias, owner.package_id, owner.package_version
                            ),
                        )
                    })?;
                if manifest.package.id != owner.package_id
                    || manifest.package.version != owner.package_version
                {
                    return Err(MintManifestError::new(
                        MintManifestErrorKind::OperationDependency,
                        format!(
                            "wiring node {node_id:?} dependency manifest coordinate differs from {}@{}",
                            owner.package_id, owner.package_version
                        ),
                    ));
                }
                let requirement = manifest
                    .base_dependencies
                    .get(&dependency.alias)
                    .ok_or_else(|| {
                        MintManifestError::new(
                            MintManifestErrorKind::OperationDependency,
                            format!(
                                "wiring node {node_id:?} names undeclared dependency alias {:?}",
                                dependency.alias
                            ),
                        )
                    })?;
                if !requirement.operations.contains(&dependency.operation) {
                    return Err(MintManifestError::new(
                        MintManifestErrorKind::OperationDependency,
                        format!(
                            "wiring node {node_id:?} operation {:?} is absent from dependency alias {:?}",
                            dependency.operation, dependency.alias
                        ),
                    ));
                }
                let dependency_package = wamn_schema_generator::PackageIdentity {
                    id: requirement.package.clone(),
                    version: requirement.version.clone(),
                    predecessor_version: None,
                };
                let registered_operation = wamn_schema_generator::canonical_operation_identity(
                    &dependency_package,
                    &dependency.operation,
                )
                .map_err(|error| {
                    MintManifestError::with_source(
                        MintManifestErrorKind::OperationDependency,
                        format!("wiring node {node_id:?} dependency operation is not canonical"),
                        error,
                    )
                })?;
                (
                    requirement.package.as_str(),
                    requirement.version.as_str(),
                    Some(requirement.digest.as_str()),
                    Some(registered_operation),
                )
            }
        };
        let facts = component_facts
            .get(&(package_id.to_owned(), package_version.to_owned()))
            .ok_or_else(|| {
                MintManifestError::new(
                    MintManifestErrorKind::OperationDependency,
                    format!(
                        "wiring node {node_id:?} target package {package_id}@{package_version} is absent from the effective release"
                    ),
                )
            })?;
        let mut matches = facts.iter().filter(|fact| {
            fact.component == node.component
                && fact.interface_version == node.interface_version
                && digest.is_none_or(|digest| fact.component_digest == digest)
                && fact.operation(&node.operation).is_some_and(|declared| {
                    registered_operation.as_deref().is_none_or(|operation| {
                        declared.registered_operation.as_deref() == Some(operation)
                    })
                })
        });
        let Some(component) = matches.next() else {
            let kind = if node.operation_dependency.is_some() {
                MintManifestErrorKind::OperationDependency
            } else {
                MintManifestErrorKind::Component
            };
            return Err(MintManifestError::new(
                kind,
                format!(
                    "wiring node {node_id:?} has no exact component tuple in {package_id}@{package_version}"
                ),
            ));
        };
        if matches.next().is_some() {
            return Err(MintManifestError::new(
                MintManifestErrorKind::Component,
                format!("wiring node {node_id:?} resolves more than one exact component fact"),
            ));
        }
        resolved.insert(node_id.clone(), component.clone());
    }
    Ok(resolved)
}

fn resolved_wiring_entry_operation(
    document: &WiringDocument,
    resolved: &BTreeMap<String, AdmittedComponent>,
) -> Result<String, MintManifestError> {
    let entry = &document.nodes[&document.entry];
    let component = resolved.get(&document.entry).ok_or_else(|| {
        MintManifestError::new(
            MintManifestErrorKind::Component,
            format!(
                "wiring entry {:?} has no resolved component",
                document.entry
            ),
        )
    })?;
    if component.operation(&entry.operation).is_none() {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Component,
            format!(
                "wiring entry {:?} has no resolved export {:?}",
                document.entry, entry.operation
            ),
        ));
    }
    Ok(entry.operation.clone())
}

type ComponentOperationKey = (String, String, String);

fn resolve_component_dependency_closure(
    roots: &BTreeMap<String, AdmittedComponent>,
    component_facts: &BTreeMap<(String, String), Vec<AdmittedComponent>>,
) -> Result<Vec<AdmittedComponent>, MintManifestError> {
    let mut pending = roots.values().cloned().collect::<Vec<_>>();
    let mut closure = BTreeMap::<(String, String, String), AdmittedComponent>::new();
    while let Some(component) = pending.pop() {
        let key = (
            component.scope.package_id.clone(),
            component.scope.package_version.clone(),
            component.component_digest.clone(),
        );
        if let Some(existing) = closure.get(&key) {
            if existing != &component {
                return Err(MintManifestError::new(
                    MintManifestErrorKind::OperationDependency,
                    format!("component dependency tuple {key:?} resolves more than one fact"),
                ));
            }
            continue;
        }
        for operation in component.operations.values() {
            for dependency in &operation.dependencies {
                pending.push(resolve_component_dependency(dependency, component_facts)?.clone());
            }
        }
        closure.insert(key, component);
    }
    validate_component_dependency_cycles(closure.values())?;
    Ok(closure.into_values().collect())
}

fn resolve_component_dependency<'a>(
    dependency: &wamn_catalog::ComponentOperationDependency,
    component_facts: &'a BTreeMap<(String, String), Vec<AdmittedComponent>>,
) -> Result<&'a AdmittedComponent, MintManifestError> {
    let Some(facts) =
        component_facts.get(&(dependency.package.clone(), dependency.version.clone()))
    else {
        return Err(MintManifestError::new(
            MintManifestErrorKind::OperationDependency,
            format!(
                "component dependency {}@{} is absent from the effective release",
                dependency.package, dependency.version
            ),
        ));
    };
    let mut matches = facts.iter().filter(|component| {
        component.component_digest == dependency.digest
            && component
                .operation(&dependency.operation)
                .is_some_and(|operation| {
                    operation.registered_operation.as_deref() == Some(dependency.operation.as_str())
                })
    });
    let Some(component) = matches.next() else {
        return Err(MintManifestError::new(
            MintManifestErrorKind::OperationDependency,
            format!(
                "component dependency {}@{} digest {} operation {:?} has no exact admitted fact",
                dependency.package, dependency.version, dependency.digest, dependency.operation
            ),
        ));
    };
    if matches.next().is_some() {
        return Err(MintManifestError::new(
            MintManifestErrorKind::OperationDependency,
            format!(
                "component dependency {}@{} digest {} operation {:?} resolves more than one admitted fact",
                dependency.package, dependency.version, dependency.digest, dependency.operation
            ),
        ));
    }
    Ok(component)
}

fn validate_component_dependency_cycles<'a>(
    components: impl Iterator<Item = &'a AdmittedComponent>,
) -> Result<(), MintManifestError> {
    let mut graph = BTreeMap::<ComponentOperationKey, Vec<ComponentOperationKey>>::new();
    for component in components {
        for (operation_name, operation) in &component.operations {
            let key = (
                component.scope.package_id.clone(),
                component.component_digest.clone(),
                operation_name.clone(),
            );
            let dependencies = operation
                .dependencies
                .iter()
                .map(|dependency| {
                    (
                        dependency.package.clone(),
                        dependency.digest.clone(),
                        dependency.operation.clone(),
                    )
                })
                .collect();
            if graph.insert(key.clone(), dependencies).is_some() {
                return Err(MintManifestError::new(
                    MintManifestErrorKind::OperationDependency,
                    format!("component operation tuple {key:?} occurs more than once"),
                ));
            }
        }
    }
    let mut incoming = BTreeMap::new();
    for dependencies in graph.values() {
        for dependency in dependencies {
            *incoming.entry(dependency.clone()).or_insert(0_usize) += 1;
        }
    }
    for operation in graph.keys() {
        incoming.entry(operation.clone()).or_insert(0);
    }
    let mut pending = incoming
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(operation, _)| operation.clone())
        .collect::<VecDeque<_>>();
    let mut visited = 0_usize;
    while let Some(operation) = pending.pop_front() {
        visited += 1;
        for dependency in &graph[&operation] {
            let count = incoming
                .get_mut(dependency)
                .expect("exact dependency resolution populated every target");
            *count -= 1;
            if *count == 0 {
                pending.push_back(dependency.clone());
            }
        }
    }
    if visited != graph.len() {
        return Err(MintManifestError::new(
            MintManifestErrorKind::OperationDependency,
            "component operation dependency closure contains a cycle",
        ));
    }
    Ok(())
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
    let resolved = resolve_wiring_components(
        &document,
        scope,
        component_facts,
        package_manifests.get(&target.package_id),
    )?;
    validate_resolved_wiring_compatibility(&document, &resolved).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Component,
            format!(
                "wiring {:?} version {} is incompatible with its resolved component facts",
                target.wiring_id, target.wiring_version
            ),
            error,
        )
    })?;
    let component_closure = resolve_component_dependency_closure(&resolved, component_facts)?;
    validate_anonymous_wiring_closure(request.attachments, target, &document, &resolved)?;
    let entry_operation = resolved_wiring_entry_operation(&document, &resolved)?;
    wirings.insert(ServingWiring {
        package_id: target.package_id.clone(),
        wiring_id: target.wiring_id.clone(),
        wiring_version: target.wiring_version,
        graph_hash: derived_hash,
    });
    for fact in component_closure {
        let digest = ArtifactHash::parse(fact.component_digest.clone()).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Component,
                format!(
                    "component {:?} stores a non-canonical digest",
                    fact.component
                ),
                error,
            )
        })?;
        components.insert(ServingComponent {
            package_id: fact.scope.package_id.clone(),
            component: fact.component.clone(),
            interface_version: fact.interface_version.clone(),
            digest,
            operations: fact
                .operations
                .iter()
                .map(|(name, operation)| {
                    (
                        name.clone(),
                        ServingComponentOperation {
                            registered_operation: operation.registered_operation.clone(),
                            dependencies: operation.dependencies.clone(),
                        },
                    )
                })
                .collect(),
        });
    }
    for (node_id, fact) in resolved {
        membership.insert(ReleaseComponentMembership {
            wiring_package_id: target.package_id.clone(),
            wiring_package_version: target.package_version.clone(),
            wiring_id: target.wiring_id.clone(),
            wiring_version: target.wiring_version,
            node_id,
            package_id: fact.scope.package_id.clone(),
            package_version: fact.scope.package_version.clone(),
            component_digest: fact.component_digest.clone(),
        });
    }
    Ok(entry_operation)
}

/// Refuse an anonymous attachment whose selected wiring can reach a registered
/// application operation.
///
/// The attachment itself cannot carry this fact for nested calls: reachability
/// exists only in the exact stored wiring plus its admitted component facts, so
/// release mint is the first boundary that can make the invalid composition
/// unrepresentable without adding another manifest field (`wamn-10yt.3.2`).
fn validate_anonymous_wiring_closure(
    attachments: &BTreeMap<String, ServingAttachment>,
    target: &ReleaseWiringTarget,
    document: &WiringDocument,
    component_facts: &BTreeMap<String, AdmittedComponent>,
) -> Result<(), MintManifestError> {
    let anonymous_attachments = attachments.iter().filter(|(_, attachment)| {
        attachment.package_id == target.package_id
            && attachment.wiring_id == target.wiring_id
            && attachment.wiring_version == target.wiring_version
            && attachment
                .auth_policy
                .get("mode")
                .and_then(serde_json::Value::as_str)
                == Some(wamn_catalog::NO_AUTHENTICATION_MODE)
    });
    let reachable = reachable_nodes(document);
    for (attachment_id, _) in anonymous_attachments {
        for node_id in document
            .nodes
            .keys()
            .filter(|node_id| reachable.contains(*node_id))
        {
            let fact = component_facts
                .get(node_id)
                .expect("wiring compatibility resolved every reachable node");
            let operation = fact
                .operation(&document.nodes[node_id].operation)
                .expect("wiring compatibility resolved every reachable operation");
            if let Some(operation) = operation.registered_operation.as_deref() {
                return Err(MintManifestError::new(
                    MintManifestErrorKind::UnauthenticatedRegisteredOperation,
                    format!(
                        "attachment {attachment_id:?} reaches registered operation \
                         {operation:?} at node {node_id:?}; set auth-policy mode = \
                         {mode:?}",
                        mode = wamn_catalog::PAT_AUTHENTICATION_MODE,
                    ),
                ));
            }
            if let Some(dependency) = operation.dependencies.first() {
                return Err(MintManifestError::new(
                    MintManifestErrorKind::UnauthenticatedRegisteredOperation,
                    format!(
                        "attachment {attachment_id:?} reaches registered operation \
                         {:?} through component dependency at node {node_id:?}; set \
                         auth-policy mode = {mode:?}",
                        dependency.operation,
                        mode = wamn_catalog::PAT_AUTHENTICATION_MODE,
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn reachable_nodes(document: &WiringDocument) -> BTreeSet<String> {
    let mut reachable = BTreeSet::new();
    let mut pending = VecDeque::from([document.entry.as_str()]);
    while let Some(node_id) = pending.pop_front() {
        if !reachable.insert(node_id.to_owned()) {
            continue;
        }
        pending.extend(
            document
                .edges
                .iter()
                .filter(|edge| edge.from == node_id)
                .map(|edge| edge.to.as_str()),
        );
    }
    reachable
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
            let version: i32 = row.get(3);
            Ok(ReleaseComponentMembership {
                wiring_package_id: row.get(0),
                wiring_package_version: row.get(1),
                wiring_id: row.get(2),
                wiring_version: positive_u32(version, "wiring-version")?,
                node_id: row.get(4),
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
        .map_err(|error| storage("read the frozen format-3 snapshot", error))?;

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
                "release membership and format-3 snapshot are partially frozen",
            ));
        }
    }

    for member in expected {
        let wiring_version = i32::try_from(member.wiring_version)
            .expect("resolved wiring version fits PostgreSQL integer");
        transaction
            .execute(
                INSERT_RELEASE_COMPONENT_SQL,
                &[
                    &request.tenant_id,
                    &request.effective_release_id,
                    &member.wiring_package_id,
                    &member.wiring_package_version,
                    &member.wiring_id,
                    &wiring_version,
                    &member.node_id,
                    &member.package_id,
                    &member.package_version,
                    &member.component_digest,
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
        .map_err(|error| storage("freeze the format-3 manifest", error))?;
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
        .map_err(|error| storage("read the frozen format-3 snapshot", error))
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

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn refresh_definition_hash(attachment: &mut ServingAttachment) {
        attachment.definition_hash = wamn_catalog::DefinitionHash::parse(
            wamn_execution_contract::canonical_json_sha256(&attachment.definition),
        )
        .expect("the canonicalizer emits a valid definition hash");
    }

    #[test]
    fn release_mint_binds_each_attachment_hash_to_its_definition() {
        let definition = serde_json::json!({
            "id": "receiving-http",
            "kind": "http",
            "route": {"path": "/receipt/get", "method": "POST"},
        });
        let definition_hash = wamn_execution_contract::canonical_json_sha256(&definition);
        let attachment = ServingAttachment {
            kind: wamn_catalog::AttachmentKind::Http,
            package_id: "wamn_receiving".to_owned(),
            wiring_id: "receipt_get".to_owned(),
            wiring_version: 1,
            definition_hash: wamn_catalog::DefinitionHash::parse(definition_hash)
                .expect("the canonicalizer emits a valid definition hash"),
            definition,
            auth_policy: serde_json::json!({"mode": "pat"}),
            registered_operation: Some("wamn-receiving:receipt/get@1.0.0".to_owned()),
        };
        let attachments = BTreeMap::from([("receiving-http".to_owned(), attachment.clone())]);
        validate_attachment_definition_hashes(&attachments)
            .expect("the exact canonical definition matches its authored hash");

        let mut changed = attachment;
        changed.definition["route"]["path"] = serde_json::json!("/receipt/query");
        let error = validate_attachment_definition_hashes(&BTreeMap::from([(
            "receiving-http".to_owned(),
            changed,
        )]))
        .expect_err("changed definition bytes cannot retain the old identity");
        assert_eq!(error.kind(), MintManifestErrorKind::Document);
    }

    #[test]
    fn release_mint_requires_and_applies_the_deployment_route_host() {
        let definition = serde_json::json!({
            "id": "receiving-http",
            "kind": "http",
            "route": {"path": "/receipt/get", "method": "POST"},
        });
        let authored_hash = wamn_execution_contract::canonical_json_sha256(&definition);
        let attachment = ServingAttachment {
            kind: wamn_catalog::AttachmentKind::Http,
            package_id: "wamn_receiving".to_owned(),
            wiring_id: "receipt_get".to_owned(),
            wiring_version: 1,
            definition_hash: wamn_catalog::DefinitionHash::parse(authored_hash.clone())
                .expect("the canonicalizer emits a valid definition hash"),
            definition,
            auth_policy: serde_json::json!({"mode": "pat"}),
            registered_operation: Some("wamn-receiving:receipt/get@1.0.0".to_owned()),
        };
        let authored = BTreeMap::from([("receiving-http".to_owned(), attachment)]);

        let missing = resolve_route_host_overlay(&authored, None)
            .expect_err("a routed release requires its deployment hostname");
        assert_eq!(missing.kind(), MintManifestErrorKind::RouteHostUnbound);
        assert_eq!(missing.kind().as_str(), "route-host-unbound");
        assert!(missing.detail().contains("receiving-http"));
        assert!(missing.detail().contains("--route-host"));

        let resolved = resolve_route_host_overlay(&authored, Some("Route.Example"))
            .expect("the deployment overlay resolves the route hostname");
        assert!(
            authored["receiving-http"].definition["route"]
                .get("host")
                .is_none()
        );
        assert_eq!(
            resolved["receiving-http"].definition["route"]["host"],
            "route.example"
        );
        assert_ne!(
            resolved["receiving-http"].definition_hash.as_str(),
            authored_hash
        );
        assert_eq!(
            resolved["receiving-http"].definition_hash.as_str(),
            wamn_execution_contract::canonical_json_sha256(&resolved["receiving-http"].definition)
        );

        let mut package_authored = authored.clone();
        package_authored
            .get_mut("receiving-http")
            .expect("the attachment exists")
            .definition["route"]["host"] = serde_json::json!("package.example");
        refresh_definition_hash(
            package_authored
                .get_mut("receiving-http")
                .expect("the attachment exists"),
        );
        let package_host = resolve_route_host_overlay(&package_authored, None)
            .expect_err("package content cannot author a deployment hostname");
        assert_eq!(package_host.kind(), MintManifestErrorKind::Document);
        assert!(package_host.detail().contains("remove it"));
        assert!(package_host.detail().contains("--route-host"));

        let mut non_routed = package_authored;
        non_routed
            .get_mut("receiving-http")
            .expect("the attachment exists")
            .kind = wamn_catalog::AttachmentKind::Internal;
        let package_host = resolve_route_host_overlay(&non_routed, None)
            .expect_err("every attachment kind refuses an authored route hostname");
        assert_eq!(package_host.kind(), MintManifestErrorKind::Document);

        let mut extra_route_field = authored.clone();
        extra_route_field
            .get_mut("receiving-http")
            .expect("the attachment exists")
            .definition["route"]["port"] = serde_json::json!(443);
        refresh_definition_hash(
            extra_route_field
                .get_mut("receiving-http")
                .expect("the attachment exists"),
        );
        let extra = resolve_route_host_overlay(&extra_route_field, Some("route.example"))
            .expect_err("package route schema admits only path and method");
        assert_eq!(extra.kind(), MintManifestErrorKind::Document);
        assert!(extra.detail().contains("exactly string path and method"));

        let mut colliding = authored;
        let first = colliding
            .get_mut("receiving-http")
            .expect("the attachment exists");
        first.definition["route"]["path"] = serde_json::json!("/receipt/{id}");
        refresh_definition_hash(first);
        let mut second = first.clone();
        second.definition["id"] = serde_json::json!("receiving-http-alias");
        second.definition["route"]["path"] = serde_json::json!("/receipt/{receipt_id}");
        refresh_definition_hash(&mut second);
        colliding.insert("receiving-http-alias".to_owned(), second);
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
                    registered_operation: registered_operation.map(str::to_owned),
                    dependencies: Vec::new(),
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                },
            )]),
            component_digest: DIGEST.to_owned(),
            imports: Vec::new(),
            imports_fingerprint: DIGEST.to_owned(),
            effects: Vec::new(),
        }
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
            "receiving",
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
                        operation: "base:purchase-order/get@1.0.0".to_owned(),
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
            wiring_id: "receiving".to_owned(),
            wiring_version: 1,
            definition_hash: wamn_catalog::DefinitionHash::parse(DIGEST)
                .expect("fixture definition hash is canonical"),
            definition: serde_json::json!({"route": {}}),
            auth_policy: serde_json::json!({"mode": mode}),
            registered_operation: None,
        }
    }

    fn dependency_manifest(digest: &str) -> wamn_schema_generator::PackageManifest {
        let mut document: serde_json::Value =
            serde_json::from_str(include_str!("../../../packages/receiving/wamn.json"))
                .expect("the repository package manifest parses as JSON");
        document["base_dependencies"] = serde_json::json!({
            "base": {
                "package": "base",
                "version": "1.0.0",
                "digest": digest,
                "operations": ["receiving.record_receipt"]
            }
        });
        serde_json::from_value(document).expect("the dependency fixture manifest parses")
    }

    #[test]
    fn release_mint_consumes_the_package_owned_weld_and_refuses_unsatisfied_policy() {
        let manifest_bytes = include_bytes!("../../../packages/receiving/wamn.json");
        let manifest = wamn_schema_generator::PackageManifest::from_slice(manifest_bytes)
            .expect("the Receiving manifest is valid");
        let weld_bytes = include_bytes!("../../../packages/receiving/generated/package-weld.json");
        let weld = wamn_schema_generator::PackageWeld::from_slice(weld_bytes)
            .expect("the Receiving weld is canonical");
        validate_package_weld(&manifest, &weld)
            .expect("the Receiving manifest and weld carry one satisfied policy fact");

        let mut unsatisfied_manifest: serde_json::Value =
            serde_json::from_slice(manifest_bytes).unwrap();
        unsatisfied_manifest["required_platform_policy_contract"]["state"] =
            serde_json::json!("unsatisfied");
        let unsatisfied_manifest = wamn_schema_generator::PackageManifest::from_slice(
            &serde_json::to_vec(&unsatisfied_manifest).unwrap(),
        )
        .unwrap();
        let mut unsatisfied_weld: serde_json::Value = serde_json::from_slice(weld_bytes).unwrap();
        unsatisfied_weld["required_platform_policy_contract"]["state"] =
            serde_json::json!("unsatisfied");
        unsatisfied_weld["promotion_state"] =
            serde_json::json!("blocked_unsatisfied_policy_contract");
        let unsatisfied_weld = wamn_schema_generator::PackageWeld::from_slice(
            &wamn_execution_contract::canonical_json_bytes(&unsatisfied_weld),
        )
        .unwrap();
        let refusal = validate_package_weld(&unsatisfied_manifest, &unsatisfied_weld)
            .expect_err("an unsatisfied generated weld cannot enter a release");
        assert_eq!(
            refusal.kind(),
            MintManifestErrorKind::PolicyContractUnsatisfied
        );
        assert!(refusal.detail().contains("receiving_data_access"));
        assert!(refusal.detail().contains("regenerate"));

        let mut mismatched_weld: serde_json::Value = serde_json::from_slice(weld_bytes).unwrap();
        mismatched_weld["required_platform_policy_contract"]["id"] =
            serde_json::json!("different_policy");
        let mismatched_weld = wamn_schema_generator::PackageWeld::from_slice(
            &wamn_execution_contract::canonical_json_bytes(&mismatched_weld),
        )
        .unwrap();
        let refusal = validate_package_weld(&manifest, &mismatched_weld)
            .expect_err("a weld cannot restate the manifest policy requirement");
        assert_eq!(refusal.kind(), MintManifestErrorKind::PackageWeld);
    }

    fn handler_manifest() -> wamn_schema_generator::PackageManifest {
        serde_json::from_str(include_str!(
            "../../../packages/client_acme_receiving/wamn.json"
        ))
        .expect("the repository handler manifest parses")
    }

    fn source_manifest() -> wamn_schema_generator::PackageManifest {
        serde_json::from_str(include_str!("../../../packages/receiving/wamn.json"))
            .expect("the repository source manifest parses")
    }

    fn handler_manifest_with_entity(entity: &str) -> wamn_schema_generator::PackageManifest {
        let mut document = serde_json::to_value(handler_manifest())
            .expect("the repository handler manifest serializes");
        document["custom_operations"]["quality.create_inspection"]["registration"]["entity"] =
            serde_json::Value::String(entity.to_owned());
        serde_json::from_value(document).expect("the mutated handler manifest parses")
    }

    fn resolve_repository_private_handler_entry() -> String {
        let mut declaration: serde_json::Value = serde_json::from_str(include_str!(
            "../../../packages/client_acme_receiving/publication/components/client_acme_receiving.json.in"
        ))
        .expect("the repository component declaration parses as JSON");
        declaration["scope"]["tenant-id"] = serde_json::json!("tenant-a");
        let declaration: wamn_catalog::ComponentDeclaration = serde_json::from_value(declaration)
            .expect("the repository component declaration is structurally valid");
        let document_value = serde_json::from_str(include_str!(
            "../../../packages/client_acme_receiving/publication/wirings/quality_create_inspection.json"
        ))
        .expect("the repository handler wiring parses as JSON");
        let document = WiringDocument::parse(&document_value)
            .expect("the repository handler wiring is structurally valid");
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
                    registered_operation: declared.registered_operation,
                    dependencies: declared.dependencies,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
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
            ("client_acme_receiving".to_owned(), manifest),
            ("wamn_receiving".to_owned(), source_manifest()),
        ]);
        let operation = resolve_repository_private_handler_entry();
        assert_eq!(
            operation,
            "client-acme-receiving:quality/create-inspection@3.0.0"
        );
        let target = ReleaseWiringTarget {
            package_id: "client_acme_receiving".to_owned(),
            package_version: "3.0.0".to_owned(),
            wiring_id: "quality_create_inspection".to_owned(),
            wiring_version: 1,
        };
        let targets = BTreeMap::from([(operation.clone(), vec![target.clone()])]);
        let registrations = derive_serving_registrations(&manifests, &targets)
            .expect("one entry wiring resolves the handler operation");
        let registration = &registrations["client_acme_receiving::quality.create_inspection"];
        assert_eq!(registration.package_id, "client_acme_receiving");
        assert_eq!(registration.source_package_id, "wamn_receiving");
        assert_eq!(registration.wiring_id, target.wiring_id);
        assert_eq!(registration.entity, "receipt");
        assert_eq!(registration.ops, BTreeSet::from(["insert".to_owned()]));

        for selected in [Vec::new(), vec![target.clone(), target]] {
            let targets = BTreeMap::from([(operation.clone(), selected)]);
            let error = derive_serving_registrations(&manifests, &targets)
                .expect_err("zero or multiple handler entry wirings were accepted");
            assert_eq!(error.kind(), MintManifestErrorKind::Registration);
        }

        let manifests = BTreeMap::from([
            (
                "client_acme_receiving".to_owned(),
                handler_manifest_with_entity("missing"),
            ),
            ("wamn_receiving".to_owned(), source_manifest()),
        ]);
        let error = derive_serving_registrations(&manifests, &targets)
            .expect_err("a registration source entity absent from its package was accepted");
        assert_eq!(error.kind(), MintManifestErrorKind::Registration);
        for fact in ["quality.create_inspection", "wamn_receiving", "missing"] {
            assert!(
                error.detail().contains(fact),
                "missing refusal fact {fact:?}"
            );
        }
    }

    fn dependency_document() -> WiringDocument {
        WiringDocument::new(
            "receiving",
            1,
            "registered",
            BTreeMap::from([(
                "registered".to_owned(),
                wamn_catalog::WiringNode {
                    component: "registered-component".to_owned(),
                    interface_version: "0.1.0".to_owned(),
                    operation: "base:receiving/record-receipt@1.0.0".to_owned(),
                    operation_dependency: Some(wamn_catalog::WiringOperationDependency {
                        alias: "base".to_owned(),
                        operation: "receiving.record_receipt".to_owned(),
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
            package_id: "wamn_receiving".to_owned(),
            package_version: "1.0.0".to_owned(),
        };
        let mut base = closure_component(
            "registered-component",
            Some("base:receiving/record-receipt@1.0.0"),
        );
        base.scope.package_id = "base".to_owned();
        let mut local_same_name = closure_component(
            "registered-component",
            Some("wamn-receiving:receiving/record-receipt@1.0.0"),
        );
        local_same_name.component_digest = format!("sha256:{}", "c".repeat(64));
        let facts = BTreeMap::from([
            (("base".to_owned(), "1.0.0".to_owned()), vec![base.clone()]),
            (
                ("wamn_receiving".to_owned(), "1.0.0".to_owned()),
                vec![local_same_name],
            ),
        ]);

        let resolved = resolve_wiring_components(
            &dependency_document(),
            &owner,
            &facts,
            Some(&dependency_manifest(&base.component_digest)),
        )
        .expect("the alias resolves its exact package, version, digest, and operation");

        assert_eq!(resolved["registered"], base);
        assert_eq!(resolved["registered"].scope.package_id, "base");
    }

    #[test]
    fn operation_dependency_refuses_digest_and_release_membership_drift() {
        let owner = ComponentPackageScope {
            tenant_id: "tenant-a".to_owned(),
            package_id: "wamn_receiving".to_owned(),
            package_version: "1.0.0".to_owned(),
        };
        let mut base = closure_component(
            "registered-component",
            Some("base:receiving/record-receipt@1.0.0"),
        );
        base.scope.package_id = "base".to_owned();
        let facts = BTreeMap::from([(("base".to_owned(), "1.0.0".to_owned()), vec![base])]);

        let digest_error = resolve_wiring_components(
            &dependency_document(),
            &owner,
            &facts,
            Some(&dependency_manifest(&format!("sha256:{}", "d".repeat(64)))),
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

    #[test]
    fn component_dependencies_expand_the_exact_release_closure_and_refuse_cycles() {
        let base_operation = "base:receiving/record-receipt@1.0.0";
        let overlay_operation = "overlay:receiving/record-receipt@3.0.0";
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

        let closure = resolve_component_dependency_closure(&roots, &facts)
            .expect("the exact dependency expands the release component closure");
        assert_eq!(closure.len(), 2);
        assert!(closure.contains(&base));
        assert!(closure.contains(&overlay));

        base.operations
            .get_mut(base_operation)
            .expect("the base operation exists")
            .dependencies = vec![wamn_catalog::ComponentOperationDependency {
            package: "overlay".to_owned(),
            version: "3.0.0".to_owned(),
            digest: overlay.component_digest.clone(),
            operation: overlay_operation.to_owned(),
        }];
        let cyclic = BTreeMap::from([
            (("base".to_owned(), "1.0.0".to_owned()), vec![base]),
            (("overlay".to_owned(), "3.0.0".to_owned()), vec![overlay]),
        ]);
        let error = resolve_component_dependency_closure(&roots, &cyclic)
            .expect_err("an exact component dependency cycle was accepted");
        assert_eq!(error.kind(), MintManifestErrorKind::OperationDependency);
        assert!(error.detail().contains("cycle"));
    }

    #[test]
    fn release_mint_refuses_an_anonymous_path_to_a_registered_operation() {
        let target = ReleaseWiringTarget {
            package_id: "base".to_owned(),
            package_version: "1.0.0".to_owned(),
            wiring_id: "receiving".to_owned(),
            wiring_version: 1,
        };
        let facts = BTreeMap::from([
            (
                "entry".to_owned(),
                closure_component("entry-component", None),
            ),
            (
                "registered".to_owned(),
                closure_component(
                    "registered-component",
                    Some("base:purchase-order/get@1.0.0"),
                ),
            ),
        ]);
        let anonymous = BTreeMap::from([(
            "receiving-http".to_owned(),
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
            "attachment \"receiving-http\" reaches registered operation \
             \"base:purchase-order/get@1.0.0\" at node \"registered\"; set \
             auth-policy mode = \"pat\""
        );

        let mut dependency_facts = facts.clone();
        dependency_facts
            .get_mut("entry")
            .unwrap()
            .operations
            .get_mut("run")
            .unwrap()
            .dependencies = vec![wamn_catalog::ComponentOperationDependency {
            package: "base".to_owned(),
            version: "1.0.0".to_owned(),
            digest: DIGEST.to_owned(),
            operation: "base:purchase-order/get@1.0.0".to_owned(),
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
        let protected = BTreeMap::from([(
            "receiving-http".to_owned(),
            closure_attachment(wamn_catalog::PAT_AUTHENTICATION_MODE),
        )]);
        assert!(
            validate_anonymous_wiring_closure(
                &protected,
                &target,
                &closure_document(true),
                &facts,
            )
            .is_ok(),
            "PAT mode is the declared remedy"
        );
    }

    #[test]
    fn package_and_wiring_coordinates_are_exact() {
        let package = parse_package("wamn_receiving@1.0.0").unwrap();
        assert_eq!(package.package_id(), "wamn_receiving");
        assert_eq!(package.package_version(), "1.0.0");

        let wiring = "wamn_receiving@1.0.0::receiving=2"
            .parse::<ReleaseWiringTarget>()
            .unwrap();
        assert_eq!(wiring.package_id, "wamn_receiving");
        assert_eq!(wiring.package_version, "1.0.0");
        assert_eq!(wiring.wiring_id, "receiving");
        assert_eq!(wiring.wiring_version, 2);
        assert!(
            "wamn_receiving::receiving=2"
                .parse::<ReleaseWiringTarget>()
                .is_err()
        );
    }

    #[test]
    fn environment_policy_refusals_remain_distinct() {
        let release = ServingRelease {
            tenant_id: "tenant-a".to_owned(),
            effective_release_id: EffectiveReleaseId::new(1).unwrap(),
            environment: "prod".to_owned(),
            packages: BTreeSet::from([PackageCoordinate::new("app", "1.0.0").unwrap()]),
        };
        let schema = BareSchemaName::new("wamn_run").unwrap();
        assert_eq!(
            verify_provisioned_environment(None, &release, &schema)
                .unwrap_err()
                .kind(),
            MintManifestErrorKind::EnvironmentPolicyAbsent
        );
        assert_eq!(
            verify_provisioned_environment(Some("dev"), &release, &schema)
                .unwrap_err()
                .kind(),
            MintManifestErrorKind::EnvironmentPolicyMismatch
        );
        verify_provisioned_environment(Some("prod"), &release, &schema).unwrap();
    }
}

//! Mint one immutable format-3 effective-release closure.
//!
//! A release is an independent integer identity plus exact package membership.
//! The publisher resolves every wiring and component from those package pairs,
//! freezes the relational closure and canonical manifest in one transaction,
//! then projects only the release identity to the control plane so a later
//! deployment attestation can reference it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use clap::Args;
use serde::de::DeserializeOwned;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentEffect, AdmittedComponentParameter, AdmittedComponentPort,
    ArtifactHash, ComponentPackageScope, EffectiveReleaseId, ManifestDigest, PackageCoordinate,
    SERVING_MANIFEST_FORMAT_VERSION, ServingAttachment, ServingComponent, ServingManifest,
    ServingRegistration, ServingRelease, ServingWiring, WiringDocument,
    validate_wiring_compatibility,
};
use wamn_control_registry::Triple;
use wamn_schema_control::BareSchemaName;

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
const SELECT_COMPONENT_FACTS_SQL: &str = "\
SELECT component, interface_version, operation, registered_operation, component_digest, \
       imports::text, imports_fingerprint, input_ports::text, output_ports::text, \
       parameters::text, effects::text \
  FROM catalog.component_library \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
 ORDER BY component COLLATE \"C\", interface_version COLLATE \"C\" FOR SHARE";
const SELECT_WIRING_SQL: &str = "\
SELECT wiring_hash, graph_json::text \
  FROM catalog.wirings \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
   AND wiring_id = $4 AND version = $5 FOR SHARE";
const SELECT_RELEASE_COMPONENTS_SQL: &str = "\
SELECT package_id, package_version, wiring_id, wiring_version, component_digest \
  FROM catalog.release_components \
 WHERE tenant_id = $1 AND effective_release_id = $2 \
 ORDER BY package_id COLLATE \"C\", package_version COLLATE \"C\", \
          wiring_id COLLATE \"C\", wiring_version, component_digest COLLATE \"C\" FOR SHARE";
const INSERT_RELEASE_COMPONENT_SQL: &str = "\
INSERT INTO catalog.release_components (\
       tenant_id, effective_release_id, package_id, package_version, \
       wiring_id, wiring_version, component_digest\
     ) VALUES ($1, $2, $3, $4, $5, $6, $7)";
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
    pub registrations: &'a BTreeMap<String, ServingRegistration>,
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
    Wiring,
    Component,
    ClosureConflict,
    Document,
    EnvironmentPolicyAbsent,
    EnvironmentPolicyMismatch,
}

impl MintManifestErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Release => "release",
            Self::Wiring => "wiring",
            Self::Component => "component",
            Self::ClosureConflict => "closure-conflict",
            Self::Document => "document",
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
    package_id: String,
    package_version: String,
    wiring_id: String,
    wiring_version: u32,
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
    #[arg(long)]
    pub registrations: PathBuf,
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
    let attachments = read_document(&args.attachments, "attachments")?;
    let registrations = read_document(&args.registrations, "registrations")?;
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
        registrations: &registrations,
    };
    let run_schema = args.verified_run_schema()?;

    let (mut client, connection) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect to the release project environment")?;
    let connection_task = tokio::spawn(connection);
    let minted = mint_in_transaction(&mut client, &request, &run_schema).await;
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
    run_schema: &BareSchemaName,
) -> anyhow::Result<MintedReleaseManifest> {
    let transaction = client
        .transaction()
        .await
        .context("begin the release mint")?;
    let minted = mint_release_manifest(&transaction, request).await?;
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

pub async fn mint_release_manifest(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
) -> Result<MintedReleaseManifest, MintManifestError> {
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&request.tenant_id])
        .await
        .map_err(|error| storage("claim the release tenant", error))?;
    validate_request(request)?;
    establish_release(transaction, request).await?;

    let mut components = BTreeSet::new();
    let mut wirings = BTreeSet::new();
    let mut membership = BTreeSet::new();
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
        let facts = load_component_facts(transaction, &scope).await?;
        resolve_wiring(
            transaction,
            request,
            target,
            &scope,
            &facts,
            &mut components,
            &mut wirings,
            &mut membership,
        )
        .await?;
    }

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
        registrations: request.registrations.clone(),
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
                operation: row.get(2),
                registered_operation: row.get(3),
                component_digest: row.get(4),
                imports: decode_json(row.get(5), &component, "imports")?,
                imports_fingerprint: row.get(6),
                input_ports: decode_json::<Vec<AdmittedComponentPort>>(
                    row.get(7),
                    &component,
                    "input-ports",
                )?,
                output_ports: decode_json::<Vec<AdmittedComponentPort>>(
                    row.get(8),
                    &component,
                    "output-ports",
                )?,
                parameters: decode_json::<Vec<AdmittedComponentParameter>>(
                    row.get(9),
                    &component,
                    "parameters",
                )?,
                effects: decode_json::<Vec<AdmittedComponentEffect>>(
                    row.get(10),
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

#[allow(clippy::too_many_arguments)]
async fn resolve_wiring(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
    target: &ReleaseWiringTarget,
    scope: &ComponentPackageScope,
    component_facts: &[AdmittedComponent],
    components: &mut BTreeSet<ServingComponent>,
    wirings: &mut BTreeSet<ServingWiring>,
    membership: &mut BTreeSet<ReleaseComponentMembership>,
) -> Result<(), MintManifestError> {
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
    validate_wiring_compatibility(&document, scope, component_facts).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Component,
            format!(
                "wiring {:?} version {} is incompatible with its package component facts",
                target.wiring_id, target.wiring_version
            ),
            error,
        )
    })?;
    wirings.insert(ServingWiring {
        package_id: target.package_id.clone(),
        wiring_id: target.wiring_id.clone(),
        wiring_version: target.wiring_version,
        graph_hash: derived_hash,
    });
    for node in document.nodes.values() {
        let fact = component_facts
            .iter()
            .find(|fact| {
                fact.component == node.component
                    && fact.interface_version == node.interface_version
                    && fact.operation == node.operation
            })
            .expect("the semantic gate resolved every wiring node");
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
            package_id: target.package_id.clone(),
            component: fact.component.clone(),
            interface_version: fact.interface_version.clone(),
            digest,
            registered_operation: fact.registered_operation.clone(),
        });
        membership.insert(ReleaseComponentMembership {
            package_id: target.package_id.clone(),
            package_version: target.package_version.clone(),
            wiring_id: target.wiring_id.clone(),
            wiring_version: target.wiring_version,
            component_digest: fact.component_digest.clone(),
        });
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
            let version: i32 = row.get(3);
            Ok(ReleaseComponentMembership {
                package_id: row.get(0),
                package_version: row.get(1),
                wiring_id: row.get(2),
                wiring_version: positive_u32(version, "wiring-version")?,
                component_digest: row.get(4),
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
                    &member.package_id,
                    &member.package_version,
                    &member.wiring_id,
                    &wiring_version,
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
mod tests {
    use super::*;

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

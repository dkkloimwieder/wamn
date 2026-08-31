//! Promote one immutable format-3 release without applying package migrations.
//!
//! `apply-package` is the sole applier. Promotion proves every source package
//! coordinate, raw manifest hash, and complete ordered migration ledger already
//! exists byte-exactly in the target before copying portable facts.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use clap::Args;
use tokio_postgres::{Client, GenericClient, IsolationLevel, NoTls, Row, Transaction};
use wamn_catalog::{
    AdmittedComponent, ComponentPackageScope, PackageCoordinate, ServingManifest, WiringDocument,
    validate_wiring_compatibility,
};
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_schema_control::BareSchemaName;
use wamn_schema_control::connections::ComponentConnectionRequirement;

use crate::publish_release::{
    DeploymentCoordinate, MintReleaseManifest, ReleaseWiringTarget, mint_release_manifest,
    project_release_identity, read_expected_environment, report_deployment_coordinate,
    verify_provisioned_environment,
};

const COMPONENT_FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";
const SELECT_SOURCE_SNAPSHOT_SQL: &str = "\
SELECT manifest_digest, canonical_bytes FROM catalog.release_manifest_v3_snapshots \
 WHERE tenant_id = $1 AND effective_release_id = $2";
const SELECT_RELEASE_PACKAGES_SQL: &str = "\
SELECT package_id, package_version FROM catalog.effective_release_packages \
 WHERE tenant_id = $1 AND effective_release_id = $2 ORDER BY package_id COLLATE \"C\"";
const SELECT_PACKAGE_SQL: &str = "\
SELECT manifest_sha256, predecessor_version FROM catalog.packages \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3";
const SELECT_MIGRATIONS_SQL: &str = "\
SELECT ordinal, relative_path, sha256 FROM catalog.package_migrations \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 ORDER BY ordinal";
const SELECT_WIRINGS_SQL: &str = "\
SELECT wiring_id, version, graph_json::text, wiring_hash FROM catalog.wirings \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
 ORDER BY wiring_id COLLATE \"C\", version";
const INSERT_WIRING_SQL: &str = "\
INSERT INTO catalog.wirings (tenant_id, package_id, package_version, wiring_id, version, graph_json, wiring_hash) \
VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7) ON CONFLICT DO NOTHING";
const EXACT_WIRING_SQL: &str = "\
SELECT EXISTS (SELECT 1 FROM catalog.wirings \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
   AND wiring_id = $4 AND version = $5 AND graph_json = $6::text::jsonb AND wiring_hash = $7)";
const SELECT_REQUIREMENTS_SQL: &str = "\
SELECT component_digest, store_alias, requirement_json::text, requirement_hash \
  FROM catalog.connection_requirements \
 WHERE tenant_id = $1 AND component_digest = ANY($2) \
 ORDER BY component_digest COLLATE \"C\", store_alias COLLATE \"C\"";
const SELECT_PROJECTION_HASHES_SQL: &str = "\
SELECT component_digest, projection_hash FROM catalog.component_library \
 WHERE tenant_id = $1 AND component_digest = ANY($2) \
 ORDER BY component_digest COLLATE \"C\"";
const INSERT_REQUIREMENT_SQL: &str = "\
INSERT INTO catalog.connection_requirements \
       (tenant_id, component_digest, store_alias, requirement_json, requirement_hash) \
VALUES ($1, $2, $3, $4::text::jsonb, $5) ON CONFLICT DO NOTHING";
const EXACT_REQUIREMENT_SQL: &str = "\
SELECT EXISTS (SELECT 1 FROM catalog.connection_requirements \
 WHERE tenant_id = $1 AND component_digest = $2 AND store_alias = $3 \
   AND requirement_json = $4::text::jsonb AND requirement_hash = $5)";
const UPSERT_HEAD_SQL: &str = "\
INSERT INTO catalog.effective_release_heads (tenant_id, environment, effective_release_id) \
VALUES ($1, $2, $3) ON CONFLICT (tenant_id, environment) DO UPDATE \
SET effective_release_id = EXCLUDED.effective_release_id, updated_at = now()";
const LOCK_ACTIVATION_SQL: &str = "\
SELECT confirmed_definition_hash, enabled FROM catalog.wiring_activation \
 WHERE tenant_id = $1 AND package_id = $2 AND environment = $3 AND wiring_id = $4 FOR UPDATE";
const UPSERT_ACTIVATION_SQL: &str = "\
INSERT INTO catalog.wiring_activation \
       (tenant_id, package_id, environment, wiring_id, confirmed_definition_hash, enabled) \
VALUES ($1, $2, $3, $4, $5, true) \
ON CONFLICT (tenant_id, package_id, environment, wiring_id) DO UPDATE \
SET confirmed_definition_hash = EXCLUDED.confirmed_definition_hash, enabled = true, changed_at = now()";
const INSERT_ACTIVATION_EVENT_SQL: &str = "\
INSERT INTO catalog.wiring_activation_events \
       (tenant_id, package_id, environment, wiring_id, enabled, confirmed_definition_hash, \
        source_environment, changed_by, reason) \
VALUES ($1, $2, $3, $4, true, $5, $6, $7, $8)";

pub const PROMOTION_REFUSAL: &str = "promotion-refused";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromotionErrorKind {
    PackageNotApplied,
    PackageManifestMismatch,
    PackageLedgerMismatch,
}

impl PromotionErrorKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PackageNotApplied => "package-not-applied",
            Self::PackageManifestMismatch => "package-manifest-mismatch",
            Self::PackageLedgerMismatch => "package-ledger-mismatch",
        }
    }
}

#[derive(Debug)]
pub struct PromotionError {
    kind: PromotionErrorKind,
    detail: String,
}

impl PromotionError {
    pub const fn kind(&self) -> PromotionErrorKind {
        self.kind
    }

    fn new(kind: PromotionErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for PromotionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{PROMOTION_REFUSAL} ({}): {}",
            self.kind.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for PromotionError {}

#[derive(Debug, Args)]
pub struct PromoteArgs {
    #[arg(long)]
    pub source_database_url: String,
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub target_database_url: String,
    #[arg(long)]
    pub control_database_url: String,
    #[arg(long)]
    pub org: String,
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub tenant: String,
    #[arg(long)]
    pub source_effective_release_id: u32,
    #[arg(long)]
    pub target_effective_release_id: u32,
    #[arg(long)]
    pub source_environment: String,
    #[arg(long)]
    pub target_environment: String,
    #[arg(long)]
    pub run_schema: String,
    #[arg(long)]
    pub artifact_base: String,
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE")]
    pub registry_auth_file: PathBuf,
    #[arg(long, default_value_t = false)]
    pub insecure_registry: bool,
    #[arg(long)]
    pub principal: String,
    #[arg(long, default_value = "promote-release")]
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MigrationRecord {
    ordinal: i32,
    relative_path: String,
    sha256: String,
}

#[derive(Clone, Debug)]
struct PackageProof {
    coordinate: PackageCoordinate,
    manifest_sha256: String,
    predecessor_version: Option<String>,
    migrations: Vec<MigrationRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromotionAction {
    CopyPortableFacts,
}

#[derive(Clone, Copy)]
struct TargetPackageProof<'a> {
    manifest_sha256: &'a str,
    predecessor_version: Option<&'a str>,
    migrations: &'a [MigrationRecord],
}

#[derive(Clone, Debug)]
struct PortableWiring {
    package_id: String,
    package_version: String,
    wiring_id: String,
    version: i32,
    graph_json: String,
    wiring_hash: String,
}

#[derive(Clone, Debug)]
struct PortableRequirement {
    component_digest: String,
    store_alias: String,
    requirement_json: String,
    requirement_hash: String,
}

#[derive(Debug)]
struct SourceRelease {
    manifest: ServingManifest,
    manifest_digest: String,
    packages: Vec<PackageProof>,
    components: Vec<AdmittedComponent>,
    projection_hashes: BTreeMap<String, String>,
    wirings: Vec<PortableWiring>,
    requirements: Vec<PortableRequirement>,
}

pub async fn run(args: PromoteArgs) -> anyhow::Result<()> {
    validate_args(&args)?;
    let run_schema = BareSchemaName::new(args.run_schema.clone())
        .with_context(|| format!("invalid --run-schema {:?}", args.run_schema))?;
    let source_release_id = pg_release_id(args.source_effective_release_id, "source")?;
    let target_release_id = pg_release_id(args.target_effective_release_id, "target")?;

    let (mut source, source_connection) = tokio_postgres::connect(&args.source_database_url, NoTls)
        .await
        .context("connect to source project environment")?;
    let source_task = tokio::spawn(source_connection);
    let release = load_source_release(&mut source, &args, source_release_id).await;
    let release = finish_connection(source, source_task, release).await?;

    let artifact_config = ComponentArtifactSourceConfig::new(
        &args.artifact_base,
        args.insecure_registry,
        COMPONENT_FETCH_TIMEOUT,
    )
    .context("configure component artifact source")?
    .with_registry_auth_file(&args.registry_auth_file)
    .context("load component registry pull credential")?;
    let artifact_source = ComponentArtifactSource::new(artifact_config);

    let (mut target, target_connection) = tokio_postgres::connect(&args.target_database_url, NoTls)
        .await
        .context("connect to target project environment")?;
    let target_task = tokio::spawn(target_connection);
    let promoted = promote_target(
        &mut target,
        &artifact_source,
        &release,
        &args,
        target_release_id,
        &run_schema,
    )
    .await;
    let (target_digest, activated) = finish_connection(target, target_task, promoted).await?;

    let target_release = wamn_catalog::ServingRelease {
        tenant_id: args.tenant.clone(),
        effective_release_id: wamn_catalog::EffectiveReleaseId::new(
            args.target_effective_release_id,
        )
        .expect("validate_args rejected zero"),
        environment: args.target_environment.clone(),
        packages: release.manifest.release.packages.clone(),
    };
    let coordinate = DeploymentCoordinate::new(&args.org, &args.project, &target_release);
    let digest = wamn_catalog::ManifestDigest::parse(target_digest.clone())
        .expect("the release mint returned a canonical digest");
    report_deployment_coordinate(&coordinate, &digest);
    project_release_identity(&args.control_database_url, &coordinate).await?;
    println!(
        "promoted {} from {}:{} to {}:{} as {} ({} component artifact(s) verified, {} pointer flip(s))",
        release.manifest_digest,
        args.source_environment,
        args.source_effective_release_id,
        args.target_environment,
        args.target_effective_release_id,
        target_digest,
        release.components.len(),
        activated,
    );
    Ok(())
}

async fn finish_connection<T>(
    client: Client,
    task: tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
    result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    drop(client);
    match result {
        Ok(value) => {
            task.await
                .context("join PostgreSQL connection")?
                .context("drive PostgreSQL connection")?;
            Ok(value)
        }
        Err(error) => {
            task.abort();
            Err(error)
        }
    }
}

fn validate_args(args: &PromoteArgs) -> anyhow::Result<()> {
    for (field, value) in [
        ("tenant", args.tenant.as_str()),
        ("source-environment", args.source_environment.as_str()),
        ("target-environment", args.target_environment.as_str()),
        ("org", args.org.as_str()),
        ("project", args.project.as_str()),
        ("principal", args.principal.as_str()),
        ("reason", args.reason.as_str()),
    ] {
        ensure!(!value.is_empty(), "promotion {field} must not be empty");
    }
    ensure!(
        args.source_effective_release_id > 0 && args.target_effective_release_id > 0,
        "effective release ids must be greater than zero"
    );
    ensure!(
        args.source_environment != args.target_environment,
        "source and target environments must differ"
    );
    Ok(())
}

fn pg_release_id(value: u32, side: &'static str) -> anyhow::Result<i32> {
    i32::try_from(value)
        .with_context(|| format!("{side} effective-release-id exceeds PostgreSQL integer"))
}

async fn load_source_release(
    client: &mut Client,
    args: &PromoteArgs,
    release_id: i32,
) -> anyhow::Result<SourceRelease> {
    let tx = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .start()
        .await
        .context("begin source release snapshot")?;
    tx.query_one(CLAIM_TENANT_SQL, &[&args.tenant])
        .await
        .context("claim source tenant")?;
    let row = tx
        .query_opt(SELECT_SOURCE_SNAPSHOT_SQL, &[&args.tenant, &release_id])
        .await
        .context("read source format-3 release snapshot")?
        .with_context(|| {
            format!(
                "source-release-missing: tenant {:?} effective release {}",
                args.tenant, args.source_effective_release_id
            )
        })?;
    let stored_digest: String = row.get(0);
    let canonical_bytes: Vec<u8> = row.get(1);
    let (manifest, derived_digest) = ServingManifest::from_canonical_bytes(&canonical_bytes)
        .context("source snapshot is not a canonical format-3 serving manifest")?;
    ensure!(
        stored_digest == derived_digest.as_str(),
        "source snapshot digest mismatch"
    );
    ensure!(
        manifest.release.tenant_id == args.tenant
            && manifest.release.effective_release_id.get() == args.source_effective_release_id
            && manifest.release.environment == args.source_environment,
        "source serving-manifest coordinate mismatch"
    );
    let release_packages = load_release_packages(&tx, &args.tenant, release_id).await?;
    ensure!(
        release_packages == manifest.release.packages,
        "source release membership differs from its frozen manifest"
    );

    let mut packages = Vec::with_capacity(release_packages.len());
    let mut components = Vec::new();
    let mut wirings = Vec::new();
    for coordinate in &release_packages {
        packages.push(load_package_proof(&tx, &args.tenant, coordinate).await?);
        let scope = ComponentPackageScope {
            tenant_id: args.tenant.clone(),
            package_id: coordinate.package_id().to_owned(),
            package_version: coordinate.package_version().to_owned(),
        };
        let package_components = crate::publish_release::load_component_facts(&tx, &scope)
            .await
            .context("read source package component facts")?;
        wirings.extend(load_wirings(&tx, &scope, &package_components).await?);
        components.extend(package_components);
    }
    let requirements = load_requirements(&tx, &args.tenant, &components).await?;
    let projection_hashes = load_projection_hashes(&tx, &args.tenant, &components).await?;
    tx.commit()
        .await
        .context("finish source release snapshot")?;
    Ok(SourceRelease {
        manifest,
        manifest_digest: stored_digest,
        packages,
        components,
        projection_hashes,
        wirings,
        requirements,
    })
}

async fn load_projection_hashes(
    tx: &Transaction<'_>,
    tenant: &str,
    components: &[AdmittedComponent],
) -> anyhow::Result<BTreeMap<String, String>> {
    let digests = components
        .iter()
        .map(|component| component.component_digest.clone())
        .collect::<Vec<_>>();
    if digests.is_empty() {
        return Ok(BTreeMap::new());
    }
    let hashes = tx
        .query(SELECT_PROJECTION_HASHES_SQL, &[&tenant, &digests])
        .await
        .context("read source component projection hashes")?
        .into_iter()
        .map(|row| (row.get::<_, String>(0), row.get::<_, String>(1)))
        .collect::<BTreeMap<_, _>>();
    ensure!(
        hashes.len() == components.len()
            && components
                .iter()
                .all(|component| hashes.contains_key(&component.component_digest)),
        "source component projection hash set is incomplete"
    );
    Ok(hashes)
}

async fn load_release_packages(
    client: &impl GenericClient,
    tenant: &str,
    release_id: i32,
) -> anyhow::Result<BTreeSet<PackageCoordinate>> {
    client
        .query(SELECT_RELEASE_PACKAGES_SQL, &[&tenant, &release_id])
        .await
        .context("read effective release package membership")?
        .into_iter()
        .map(|row| {
            PackageCoordinate::new(row.get::<_, String>(0), row.get::<_, String>(1))
                .context("stored package coordinate is invalid")
        })
        .collect()
}

async fn load_package_proof(
    client: &impl GenericClient,
    tenant: &str,
    coordinate: &PackageCoordinate,
) -> anyhow::Result<PackageProof> {
    let row = client
        .query_opt(
            SELECT_PACKAGE_SQL,
            &[
                &tenant,
                &coordinate.package_id(),
                &coordinate.package_version(),
            ],
        )
        .await
        .context("read package manifest identity")?
        .with_context(|| {
            format!(
                "source package {}@{} is missing",
                coordinate.package_id(),
                coordinate.package_version()
            )
        })?;
    let migrations = load_migrations(client, tenant, coordinate).await?;
    ensure!(
        !migrations.is_empty(),
        "source package {}@{} has no migration ordinal 1",
        coordinate.package_id(),
        coordinate.package_version()
    );
    for (index, migration) in migrations.iter().enumerate() {
        ensure!(
            migration.ordinal == i32::try_from(index + 1).expect("ledger length fits integer"),
            "source package {}@{} has a migration gap at ordinal {}",
            coordinate.package_id(),
            coordinate.package_version(),
            migration.ordinal
        );
    }
    Ok(PackageProof {
        coordinate: coordinate.clone(),
        manifest_sha256: row.get(0),
        predecessor_version: row.get(1),
        migrations,
    })
}

async fn load_migrations(
    client: &impl GenericClient,
    tenant: &str,
    coordinate: &PackageCoordinate,
) -> anyhow::Result<Vec<MigrationRecord>> {
    Ok(client
        .query(
            SELECT_MIGRATIONS_SQL,
            &[
                &tenant,
                &coordinate.package_id(),
                &coordinate.package_version(),
            ],
        )
        .await
        .context("read complete ordered package migration ledger")?
        .into_iter()
        .map(|row| MigrationRecord {
            ordinal: row.get(0),
            relative_path: row.get(1),
            sha256: row.get(2),
        })
        .collect())
}

async fn load_wirings(
    tx: &Transaction<'_>,
    scope: &ComponentPackageScope,
    components: &[AdmittedComponent],
) -> anyhow::Result<Vec<PortableWiring>> {
    tx.query(
        SELECT_WIRINGS_SQL,
        &[&scope.tenant_id, &scope.package_id, &scope.package_version],
    )
    .await
    .context("read source package wirings")?
    .into_iter()
    .map(|row| decode_wiring(row, scope, components))
    .collect()
}

fn decode_wiring(
    row: Row,
    scope: &ComponentPackageScope,
    components: &[AdmittedComponent],
) -> anyhow::Result<PortableWiring> {
    let wiring_id: String = row.get(0);
    let version: i32 = row.get(1);
    let graph_json: String = row.get(2);
    let wiring_hash: String = row.get(3);
    let value = serde_json::from_str(&graph_json)
        .with_context(|| format!("source wiring {wiring_id:?} stores unreadable JSON"))?;
    let document = WiringDocument::parse(&value)
        .with_context(|| format!("source wiring {wiring_id:?} stores an invalid document"))?;
    ensure!(
        document.wiring_id == wiring_id
            && i32::try_from(document.version).ok() == Some(version)
            && document.wiring_hash().as_str() == wiring_hash,
        "source wiring {wiring_id:?} row and document identities differ"
    );
    validate_wiring_compatibility(&document, scope, components)
        .with_context(|| format!("source wiring {wiring_id:?} is incompatible"))?;
    Ok(PortableWiring {
        package_id: scope.package_id.clone(),
        package_version: scope.package_version.clone(),
        wiring_id,
        version,
        graph_json,
        wiring_hash,
    })
}

async fn load_requirements(
    tx: &Transaction<'_>,
    tenant: &str,
    components: &[AdmittedComponent],
) -> anyhow::Result<Vec<PortableRequirement>> {
    let digests = components
        .iter()
        .map(|component| component.component_digest.clone())
        .collect::<Vec<_>>();
    if digests.is_empty() {
        return Ok(Vec::new());
    }
    tx.query(SELECT_REQUIREMENTS_SQL, &[&tenant, &digests])
        .await
        .context("read portable component requirements")?
        .into_iter()
        .map(decode_requirement)
        .collect()
}

fn decode_requirement(row: Row) -> anyhow::Result<PortableRequirement> {
    let component_digest: String = row.get(0);
    let store_alias: String = row.get(1);
    let requirement_json: String = row.get(2);
    let requirement_hash: String = row.get(3);
    let requirement: ComponentConnectionRequirement = serde_json::from_str(&requirement_json)
        .context("source component requirement stores unreadable JSON")?;
    ensure!(
        requirement.component_digest() == component_digest
            && requirement.store_alias() == store_alias
            && requirement.requirement_hash() == requirement_hash,
        "source component requirement identity or hash mismatch"
    );
    Ok(PortableRequirement {
        component_digest,
        store_alias,
        requirement_json,
        requirement_hash,
    })
}

async fn promote_target(
    client: &mut Client,
    artifact_source: &ComponentArtifactSource,
    source: &SourceRelease,
    args: &PromoteArgs,
    target_release_id: i32,
    run_schema: &BareSchemaName,
) -> anyhow::Result<(String, usize)> {
    let preflight = client
        .transaction()
        .await
        .context("begin target package-parity preflight")?;
    preflight
        .query_one(CLAIM_TENANT_SQL, &[&args.tenant])
        .await
        .context("claim target preflight tenant")?;
    verify_target_packages(&preflight, &args.tenant, &source.packages).await?;
    preflight
        .commit()
        .await
        .context("finish target package-parity preflight")?;
    for component in &source.components {
        artifact_source
            .pull_verified(component)
            .await
            .with_context(|| format!("verify promoted component {:?}", component.component))?;
    }

    let tx = client
        .build_transaction()
        .isolation_level(IsolationLevel::Serializable)
        .start()
        .await
        .context("begin target promotion")?;
    tx.query_one(CLAIM_TENANT_SQL, &[&args.tenant])
        .await
        .context("claim target promotion tenant")?;
    match verify_target_packages(&tx, &args.tenant, &source.packages).await? {
        PromotionAction::CopyPortableFacts => {
            copy_portable_facts(&tx, source, &args.tenant).await?
        }
    }

    let packages = source.manifest.release.packages.clone();
    let wirings = source
        .manifest
        .wirings
        .iter()
        .map(|wiring| {
            let package = packages
                .iter()
                .find(|package| package.package_id() == wiring.package_id)
                .expect("manifest validation proved package membership");
            ReleaseWiringTarget {
                package_id: wiring.package_id.clone(),
                package_version: package.package_version().to_owned(),
                wiring_id: wiring.wiring_id.clone(),
                wiring_version: wiring.wiring_version,
            }
        })
        .collect::<BTreeSet<_>>();
    let minted = mint_release_manifest(
        &tx,
        &MintReleaseManifest {
            tenant_id: &args.tenant,
            effective_release_id: target_release_id,
            environment: &args.target_environment,
            verified_publisher_principal: &args.principal,
            packages: &packages,
            wirings: &wirings,
            attachments: &source.manifest.attachments,
            registrations: &source.manifest.registrations,
        },
    )
    .await
    .context("mint target format-3 release snapshot")?;
    let expected = read_expected_environment(&tx, run_schema, &args.tenant).await?;
    verify_provisioned_environment(expected.as_deref(), &minted.manifest.release, run_schema)?;
    tx.execute(
        UPSERT_HEAD_SQL,
        &[&args.tenant, &args.target_environment, &target_release_id],
    )
    .await
    .context("advance target effective-release head")?;
    let mut activated = 0;
    for wiring in &minted.manifest.wirings {
        if activate_once(
            &tx,
            args,
            &wiring.package_id,
            &wiring.wiring_id,
            wiring.graph_hash.as_str(),
        )
        .await?
        {
            activated += 1;
        }
    }
    tx.commit().await.context("commit target promotion")?;
    Ok((minted.digest.as_str().to_owned(), activated))
}

async fn verify_target_packages(
    client: &impl GenericClient,
    tenant: &str,
    expected: &[PackageProof],
) -> anyhow::Result<PromotionAction> {
    for package in expected {
        let coordinate = &package.coordinate;
        let row = client
            .query_opt(
                SELECT_PACKAGE_SQL,
                &[
                    &tenant,
                    &coordinate.package_id(),
                    &coordinate.package_version(),
                ],
            )
            .await
            .context("read target package identity")?;
        let Some(row) = row else {
            return compare_target_package(package, None).map_err(Into::into);
        };
        let target_manifest: String = row.get(0);
        let target_predecessor: Option<String> = row.get(1);
        let target_migrations = load_migrations(client, tenant, coordinate).await?;
        compare_target_package(
            package,
            Some(TargetPackageProof {
                manifest_sha256: &target_manifest,
                predecessor_version: target_predecessor.as_deref(),
                migrations: &target_migrations,
            }),
        )?;
    }
    Ok(PromotionAction::CopyPortableFacts)
}

fn compare_target_package(
    expected: &PackageProof,
    target: Option<TargetPackageProof<'_>>,
) -> Result<PromotionAction, PromotionError> {
    let coordinate = &expected.coordinate;
    let Some(target) = target else {
        return Err(PromotionError::new(
            PromotionErrorKind::PackageNotApplied,
            format!(
                "target lacks {}@{}; run wamn-ctl apply-package for this exact package directory against the target",
                coordinate.package_id(),
                coordinate.package_version()
            ),
        ));
    };
    if target.manifest_sha256 != expected.manifest_sha256 {
        return Err(PromotionError::new(
            PromotionErrorKind::PackageManifestMismatch,
            format!(
                "{}@{} source manifest {} differs from target {}",
                coordinate.package_id(),
                coordinate.package_version(),
                expected.manifest_sha256,
                target.manifest_sha256
            ),
        ));
    }
    if target.predecessor_version != expected.predecessor_version.as_deref() {
        return Err(PromotionError::new(
            PromotionErrorKind::PackageManifestMismatch,
            format!(
                "{}@{} source predecessor {:?} differs from target {:?}",
                coordinate.package_id(),
                coordinate.package_version(),
                expected.predecessor_version,
                target.predecessor_version
            ),
        ));
    }
    if target.migrations != expected.migrations {
        return Err(PromotionError::new(
            PromotionErrorKind::PackageLedgerMismatch,
            format!(
                "{}@{} has a different complete ordered migration ledger; promote never applies migrations",
                coordinate.package_id(),
                coordinate.package_version()
            ),
        ));
    }
    Ok(PromotionAction::CopyPortableFacts)
}

async fn copy_portable_facts(
    tx: &Transaction<'_>,
    source: &SourceRelease,
    tenant: &str,
) -> anyhow::Result<()> {
    for component in &source.components {
        let projection_hash = source
            .projection_hashes
            .get(&component.component_digest)
            .expect("source snapshot loaded one projection hash per component");
        crate::push_component::append_or_verify_admitted_component(tx, component, projection_hash)
            .await?;
    }
    for requirement in &source.requirements {
        persist_requirement(tx, tenant, requirement).await?;
    }
    for wiring in &source.wirings {
        persist_wiring(tx, tenant, wiring).await?;
    }
    Ok(())
}

async fn persist_requirement(
    tx: &Transaction<'_>,
    tenant: &str,
    requirement: &PortableRequirement,
) -> anyhow::Result<()> {
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 5] = [
        &tenant,
        &requirement.component_digest,
        &requirement.store_alias,
        &requirement.requirement_json,
        &requirement.requirement_hash,
    ];
    tx.execute(INSERT_REQUIREMENT_SQL, &params)
        .await
        .context("append portable component requirement")?;
    let exact: bool = tx
        .query_one(EXACT_REQUIREMENT_SQL, &params)
        .await
        .context("verify portable component requirement")?
        .get(0);
    ensure!(exact, "component-connection-requirement-conflict");
    Ok(())
}

async fn persist_wiring(
    tx: &Transaction<'_>,
    tenant: &str,
    wiring: &PortableWiring,
) -> anyhow::Result<()> {
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 7] = [
        &tenant,
        &wiring.package_id,
        &wiring.package_version,
        &wiring.wiring_id,
        &wiring.version,
        &wiring.graph_json,
        &wiring.wiring_hash,
    ];
    tx.execute(INSERT_WIRING_SQL, &params)
        .await
        .context("append promoted wiring fact")?;
    let exact: bool = tx
        .query_one(EXACT_WIRING_SQL, &params)
        .await
        .context("verify promoted wiring fact")?
        .get(0);
    ensure!(exact, "promoted-wiring-conflict");
    Ok(())
}

async fn activate_once(
    tx: &Transaction<'_>,
    args: &PromoteArgs,
    package_id: &str,
    wiring_id: &str,
    graph_hash: &str,
) -> anyhow::Result<bool> {
    let current = tx
        .query_opt(
            LOCK_ACTIVATION_SQL,
            &[
                &args.tenant,
                &package_id,
                &args.target_environment,
                &wiring_id,
            ],
        )
        .await
        .context("lock target wiring activation")?;
    if current.is_some_and(|row| row.get::<_, String>(0) == graph_hash && row.get::<_, bool>(1)) {
        return Ok(false);
    }
    tx.execute(
        UPSERT_ACTIVATION_SQL,
        &[
            &args.tenant,
            &package_id,
            &args.target_environment,
            &wiring_id,
            &graph_hash,
        ],
    )
    .await
    .context("activate promoted wiring")?;
    tx.execute(
        INSERT_ACTIVATION_EVENT_SQL,
        &[
            &args.tenant,
            &package_id,
            &args.target_environment,
            &wiring_id,
            &graph_hash,
            &args.source_environment,
            &args.principal,
            &args.reason,
        ],
    )
    .await
    .context("record promoted wiring activation")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migration(ordinal: i32, name: &str, digest_byte: char) -> MigrationRecord {
        MigrationRecord {
            ordinal,
            relative_path: format!("migrations/{ordinal:04}_{name}.sql"),
            sha256: format!("sha256:{}", digest_byte.to_string().repeat(64)),
        }
    }

    fn package() -> PackageProof {
        PackageProof {
            coordinate: PackageCoordinate::new("wamn_receiving", "1.1.0")
                .expect("fixture coordinate is valid"),
            manifest_sha256: format!("sha256:{}", "a".repeat(64)),
            predecessor_version: Some("1.0.0".to_owned()),
            migrations: vec![migration(1, "initial", 'b'), migration(2, "upgrade", 'c')],
        }
    }

    fn target(package: &PackageProof) -> TargetPackageProof<'_> {
        TargetPackageProof {
            manifest_sha256: &package.manifest_sha256,
            predecessor_version: package.predecessor_version.as_deref(),
            migrations: &package.migrations,
        }
    }

    #[test]
    fn absent_target_package_refuses_with_apply_package_remedy() {
        let package = package();
        let error = compare_target_package(&package, None)
            .expect_err("promotion cannot create an absent package");
        assert_eq!(error.kind(), PromotionErrorKind::PackageNotApplied);
        let rendered = error.to_string();
        assert!(rendered.contains("wamn_receiving@1.1.0"));
        assert!(rendered.contains("run wamn-ctl apply-package"));
    }

    #[test]
    fn manifest_and_predecessor_identity_must_both_match() {
        let package = package();
        let wrong_manifest = format!("sha256:{}", "f".repeat(64));
        let manifest_error = compare_target_package(
            &package,
            Some(TargetPackageProof {
                manifest_sha256: &wrong_manifest,
                ..target(&package)
            }),
        )
        .expect_err("a different raw manifest identity must refuse");
        assert_eq!(
            manifest_error.kind(),
            PromotionErrorKind::PackageManifestMismatch
        );

        let predecessor_error = compare_target_package(
            &package,
            Some(TargetPackageProof {
                predecessor_version: Some("0.9.0"),
                ..target(&package)
            }),
        )
        .expect_err("a different predecessor identity must refuse");
        assert_eq!(
            predecessor_error.kind(),
            PromotionErrorKind::PackageManifestMismatch
        );
    }

    #[test]
    fn missing_extra_and_divergent_ordered_ledgers_refuse() {
        let package = package();
        let mut missing = package.migrations.clone();
        missing.pop();
        let mut extra = package.migrations.clone();
        extra.push(migration(3, "extra", 'd'));
        let mut divergent = package.migrations.clone();
        divergent[1].sha256 = format!("sha256:{}", "e".repeat(64));

        for migrations in [&missing, &extra, &divergent] {
            let error = compare_target_package(
                &package,
                Some(TargetPackageProof {
                    migrations,
                    ..target(&package)
                }),
            )
            .expect_err("the complete target ledger must be byte-exact");
            assert_eq!(error.kind(), PromotionErrorKind::PackageLedgerMismatch);
        }
    }

    #[test]
    fn exact_target_proof_unlocks_only_portable_fact_copy() {
        let package = package();
        assert_eq!(
            compare_target_package(&package, Some(target(&package)))
                .expect("an exact target package is promotable"),
            PromotionAction::CopyPortableFacts
        );
    }
}

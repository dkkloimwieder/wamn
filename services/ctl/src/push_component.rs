//! Publish exact component bytes, then project their admitted facts to both planes.
//!
//! This is the sole production caller of component byte admission. It validates
//! the local bytes and exact already-applied package before registry I/O,
//! publishes one digest-addressed OCI layer, pulls it back, then exact-replays
//! one admission-computed projection into the source-project and control stores.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use oci_client::client::{ClientConfig, ClientProtocol, Config, ImageLayer};
use oci_client::manifest::OciImageManifest;
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use tokio_postgres::{Client as PgClient, Config as PgConfig, NoTls, Transaction};
use wamn_catalog::ConnectionTypeDescriptor;
use wamn_catalog::{
    AdmittedComponent, ComponentConnection, ComponentConnectionType, ComponentDeclaration,
};
use wamn_runtime::component_admission::validate_component_admission;
use wamn_runtime::component_artifact::{
    component_artifact_config_bytes, component_artifact_layout, component_artifact_reference,
};
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_runtime::registry_credentials::{RegistryCredentials, read_registry_credentials};
use wamn_schema_control::connections::ComponentConnectionRequirement;
use wamn_schema_control::{PackageDirectory, PackageMigrationErrorKind, plan_package_migrations};

/// Bound each registry connect/read phase without adding a second deployment knob.
const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";
const LOCK_PROJECTION_SQL: &str =
    "SELECT pg_advisory_xact_lock(hashtextextended('wamn.component.projection:' || $1, 0))";

const REGISTER_PACKAGE_SQL: &str = "SELECT catalog.register_package($1, $2, $3, $4, $5)";
const SELECT_PACKAGE_SQL: &str = "SELECT manifest_sha256, predecessor_version FROM catalog.packages \
     WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3";

const INSERT_COMPONENT_SQL: &str = "INSERT INTO catalog.component_library (\
         tenant_id, package_id, package_version, component, interface_version, operation, \
         registered_operation, component_digest, projection_hash, imports, imports_fingerprint, effects, \
         input_ports, output_ports, parameters\
     ) VALUES (\
         $1, $2, $3, $4, $5, $6, $7, $8, $9, $10::text::jsonb, $11, $12::text::jsonb, \
         $13::text::jsonb, $14::text::jsonb, $15::text::jsonb\
     ) ON CONFLICT DO NOTHING RETURNING admitted_at";

const EXACT_COMPONENT_SQL: &str = "SELECT EXISTS (\
         SELECT 1 FROM catalog.component_library \
          WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
            AND component = $4 AND interface_version = $5 AND operation = $6 \
            AND registered_operation IS NOT DISTINCT FROM $7 \
            AND component_digest = $8 AND projection_hash = $9 \
            AND imports = $10::text::jsonb AND imports_fingerprint = $11 \
            AND effects = $12::text::jsonb AND input_ports = $13::text::jsonb \
            AND output_ports = $14::text::jsonb AND parameters = $15::text::jsonb\
     )";
const SELECT_COMPONENT_PROJECTION_HASH_SQL: &str = "SELECT projection_hash \
       FROM catalog.component_library \
      WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
        AND component = $4 AND interface_version = $5";
const SELECT_REQUIREMENT_INVENTORY_SQL: &str = "SELECT store_alias, requirement_hash \
       FROM catalog.connection_requirements \
      WHERE tenant_id = $1 AND component_digest = $2 \
      ORDER BY store_alias COLLATE \"C\"";

/// Stable prefix for package/component projection refusals.
pub const COMPONENT_PROJECTION_REFUSAL: &str = "component-projection-refused";

/// Remedy-distinct projector refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentProjectionErrorKind {
    PackageCoordinateMismatch,
    SourcePackageNotApplied,
    PackageManifestMismatch,
    SourcePackageMigrationMismatch,
    PlaneContentMismatch,
    ComponentFactConflict,
    ConnectionFactConflict,
}

impl ComponentProjectionErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PackageCoordinateMismatch => "package-coordinate-mismatch",
            Self::SourcePackageNotApplied => "source-package-not-applied",
            Self::PackageManifestMismatch => "package-manifest-mismatch",
            Self::SourcePackageMigrationMismatch => "source-package-migration-mismatch",
            Self::PlaneContentMismatch => "plane-content-mismatch",
            Self::ComponentFactConflict => "component-fact-conflict",
            Self::ConnectionFactConflict => "connection-fact-conflict",
        }
    }
}

/// Contextual refusal from the dual-plane projection boundary.
#[derive(Debug)]
pub struct ComponentProjectionError {
    kind: ComponentProjectionErrorKind,
    detail: String,
}

impl ComponentProjectionError {
    fn new(kind: ComponentProjectionErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    pub const fn kind(&self) -> ComponentProjectionErrorKind {
        self.kind
    }
}

impl fmt::Display for ComponentProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{COMPONENT_PROJECTION_REFUSAL} ({}): {}",
            self.kind.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for ComponentProjectionError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectionPlane {
    SourceProject,
    Control,
}

impl fmt::Display for ProjectionPlane {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SourceProject => "source-project",
            Self::Control => "control",
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ProjectionOutcome {
    package_inserted: bool,
    component_inserted: bool,
    requirements_inserted: usize,
    manifest_sha256: String,
    projection_hash: String,
}

impl ProjectionOutcome {
    fn is_noop(&self) -> bool {
        !self.package_inserted && !self.component_inserted && self.requirements_inserted == 0
    }
}

#[derive(Debug, PartialEq, Eq)]
struct DualPlaneProjectionOutcome {
    project: ProjectionOutcome,
    control: ProjectionOutcome,
}

impl DualPlaneProjectionOutcome {
    #[cfg(test)]
    fn is_noop(&self) -> bool {
        self.project.is_noop() && self.control.is_noop()
    }
}

#[derive(Debug, Args)]
pub struct PushComponentArgs {
    /// Package root whose strict wamn.json owns this component coordinate.
    #[arg(long)]
    pub package: PathBuf,

    /// Exact wasm component bytes to validate and publish.
    #[arg(long)]
    pub component_bytes: PathBuf,

    /// JSON declaration of catalog scope, component identity, operation, typed
    /// input/output ports, and parameters.
    #[arg(long)]
    pub declaration: PathBuf,

    /// Explicit `<registry>/<repository>` base. It must not include a tag or
    /// digest; the admitted component digest derives the immutable tag.
    #[arg(long)]
    pub artifact_base: String,

    /// Projected `.dockerconfigjson` file carrying the push credential.
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE")]
    pub registry_auth_file: PathBuf,

    /// Use plain HTTP for exactly the registry host in `--artifact-base`.
    #[arg(long, default_value_t = false)]
    pub insecure_registry: bool,

    /// Exact admitted `wamn:<package>` capability. Repeat for each package the
    /// closed platform registry grants this component.
    #[arg(long = "admit-platform-package")]
    pub admitted_platform_packages: Vec<String>,

    /// Owner URL to the already-applied source project database. Env
    /// `WAMN_PG_ADMIN_URL`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub project_database_url: String,

    /// Owner URL to the T1 control database. Env `WAMN_SYSTEM_ADMIN_URL`.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub control_database_url: String,
}

/// Validate, publish, verify, and finally record one admitted component.
pub async fn run(args: PushComponentArgs) -> anyhow::Result<()> {
    let directory = crate::apply_package::read_package_directory(&args.package)?;
    let package = plan_package_migrations(&directory, None)
        .context("validate strict package directory before component publication")?;
    let component_bytes = std::fs::read(&args.component_bytes)
        .with_context(|| format!("read component bytes {}", args.component_bytes.display()))?;
    let declaration_bytes = std::fs::read(&args.declaration)
        .with_context(|| format!("read component declaration {}", args.declaration.display()))?;
    let declaration: ComponentDeclaration = serde_json::from_slice(&declaration_bytes)
        .with_context(|| format!("parse component declaration {}", args.declaration.display()))?;

    let engine = wamn_runtime::build_engine(&[]).context("build component admission engine")?;
    let facts = validate_component_admission(
        &engine,
        &component_bytes,
        wamn_runtime::component_admission::ComponentAdmissionRequest {
            declaration,
            admitted_platform_packages: args
                .admitted_platform_packages
                .into_iter()
                .collect::<BTreeSet<_>>(),
        },
    )
    .context("validate exact component bytes")?;
    let admitted = facts.component;
    let requirements = facts
        .connections
        .iter()
        .map(|connection| portable_requirement(&admitted.component_digest, connection))
        .collect::<Vec<_>>();
    let projection_hash = admitted_projection_hash(&admitted, &requirements)?;
    ensure_component_package_matches(&admitted, &package.coordinate)?;
    let project_config: PgConfig = args
        .project_database_url
        .parse()
        .context("parse source-project database URL")?;
    let control_config: PgConfig = args
        .control_database_url
        .parse()
        .context("parse T1 control database URL")?;
    verify_source_package(&project_config, &admitted, &directory, &args.package).await?;
    let artifact = component_artifact_reference(&args.artifact_base, &admitted.component_digest)
        .context("derive component artifact reference")?;
    let reference = Reference::with_tag(
        artifact.registry().to_owned(),
        artifact.repository().to_owned(),
        artifact.tag().to_owned(),
    );
    let registry_credentials =
        read_registry_credentials(&args.registry_auth_file, artifact.registry())
            .context("load component registry push credential")?;
    let config_bytes = component_artifact_config_bytes(&admitted);

    publish_and_verify(
        &reference,
        &args.artifact_base,
        args.insecure_registry,
        &component_bytes,
        &config_bytes,
        &admitted,
        &registry_credentials,
    )
    .await?;

    let projected = project_component_facts(
        &project_config,
        &control_config,
        &admitted,
        &requirements,
        &package.manifest_sha256,
        &projection_hash,
        &directory,
        &args.package,
    )
    .await?;
    println!(
        "projected {} (source-project: {}; control: {})",
        admitted.component_digest,
        if projected.project.is_noop() {
            "already converged"
        } else {
            "changed"
        },
        if projected.control.is_noop() {
            "already converged"
        } else {
            "changed"
        }
    );
    println!("{}", admitted.component_digest);
    Ok(())
}

/// Translate one admitted connection into its platform-owned portable record.
///
/// This is the single place a declared alias becomes connection SEMANTICS. The
/// descriptor is minted from the platform's own constructor, never authored, so
/// field ownership and credential injection cannot be widened by a declaration.
fn portable_requirement(
    component_digest: &str,
    connection: &ComponentConnection,
) -> ComponentConnectionRequirement {
    let descriptor = match connection.requirement_type {
        ComponentConnectionType::Http => ConnectionTypeDescriptor::http_v1(),
    };
    ComponentConnectionRequirement::new(component_digest, &connection.store_alias, descriptor)
}

/// Hash the complete normalized component projection using the shared RFC-8785 spelling.
pub fn admitted_projection_hash(
    component: &AdmittedComponent,
    requirements: &[ComponentConnectionRequirement],
) -> anyhow::Result<String> {
    let mut ordered = requirements.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.store_alias().cmp(right.store_alias()));
    let requirement_documents = ordered
        .into_iter()
        .map(|requirement| {
            serde_json::from_slice::<serde_json::Value>(&requirement.canonical_bytes())
                .context("parse normalized connection requirement for projection hash")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let projection = serde_json::json!({
        "component": serde_json::to_value(component)
            .context("serialize normalized component for projection hash")?,
        "connection-requirements": requirement_documents,
    });
    Ok(wamn_execution_contract::canonical_json_sha256(&projection))
}

async fn publish_and_verify(
    reference: &Reference,
    artifact_base: &str,
    insecure: bool,
    component_bytes: &[u8],
    config_bytes: &[u8],
    component: &AdmittedComponent,
    credentials: &RegistryCredentials,
) -> anyhow::Result<()> {
    let protocol = if insecure {
        ClientProtocol::HttpsExcept(vec![reference.resolve_registry().to_owned()])
    } else {
        ClientProtocol::Https
    };
    // Built here rather than borrowed from `wash_runtime::oci`, which exposes no
    // client-construction seam. Its `push_component` mints the config blob
    // itself from `WasmConfig` and tags the layer `WASM_LAYER_MEDIA_TYPE`; this
    // artifact carries the platform's own `component_artifact_layout`, whose
    // config blob is the admitted-component fact `pull_verified` re-proves
    // below. Routing through that API would discard the proof. See standing
    // trigger 5 in `docs/architecture/native-alignment-ledger.md`
    // (`wamn-kdhw`).
    let client = OciClient::new(ClientConfig {
        protocol,
        read_timeout: Some(REGISTRY_IO_TIMEOUT),
        connect_timeout: Some(REGISTRY_IO_TIMEOUT),
        ..ClientConfig::default()
    });
    let auth = RegistryAuth::Basic(
        credentials.username().to_owned(),
        credentials.password().to_owned(),
    );
    let (layer, config, manifest) = artifact_layout(component_bytes, config_bytes);

    client
        .push(
            reference,
            std::slice::from_ref(&layer),
            config,
            &auth,
            Some(manifest),
        )
        .await
        .with_context(|| format!("push component artifact {reference}"))?;

    // Publication is not a successful catalog admission until the immutable
    // reference can be read back by the production puller and independently
    // proves the descriptor, config facts, and exact component body.
    let source_config =
        ComponentArtifactSourceConfig::new(artifact_base, insecure, REGISTRY_IO_TIMEOUT)
            .context("configure published component verification source")?
            .with_credentials(credentials.clone());
    ComponentArtifactSource::new(source_config)
        .pull_verified(component)
        .await
        .with_context(|| format!("verify published component artifact {reference}"))?;
    Ok(())
}

fn artifact_layout(
    component_bytes: &[u8],
    config_bytes: &[u8],
) -> (ImageLayer, Config, OciImageManifest) {
    let layout = component_artifact_layout(component_bytes, config_bytes);
    let layer = ImageLayer::new(
        layout.component_bytes().to_vec(),
        layout.layer_media_type().to_owned(),
        None,
    );
    let config = Config::new(
        layout.config_bytes().to_vec(),
        layout.config_media_type().to_owned(),
        None,
    );
    let manifest = OciImageManifest::build(std::slice::from_ref(&layer), &config, None);
    (layer, config, manifest)
}

fn ensure_component_package_matches(
    component: &AdmittedComponent,
    package: &wamn_catalog::PackageCoordinate,
) -> anyhow::Result<()> {
    if component.scope.package_id != package.package_id()
        || component.scope.package_version != package.package_version()
    {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorKind::PackageCoordinateMismatch,
            format!(
                "declaration names {}@{} but package directory names {}@{}",
                component.scope.package_id,
                component.scope.package_version,
                package.package_id(),
                package.package_version()
            ),
        )
        .into());
    }
    Ok(())
}

async fn verify_source_package(
    database_config: &PgConfig,
    component: &AdmittedComponent,
    directory: &PackageDirectory,
    package_path: &std::path::Path,
) -> anyhow::Result<()> {
    let (mut client, connection) = database_config
        .connect(NoTls)
        .await
        .context("connect to source-project database")?;
    let connection_task = tokio::spawn(connection);
    let verified =
        verify_source_package_with_client(&mut client, component, directory, package_path).await;
    drop(client);
    if verified.is_err() {
        connection_task.abort();
    } else {
        connection_task
            .await
            .context("join source-project database connection")?
            .context("drive source-project database connection")?;
    }
    verified.map(|_| ())
}

async fn verify_source_package_with_client(
    client: &mut PgClient,
    component: &AdmittedComponent,
    directory: &PackageDirectory,
    package_path: &std::path::Path,
) -> anyhow::Result<String> {
    let transaction = client
        .transaction()
        .await
        .context("begin source-package verification")?;
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&component.scope.tenant_id])
        .await
        .context("claim source-package tenant")?;
    let observed =
        require_exact_applied_package(&transaction, component, directory, package_path).await?;
    transaction
        .commit()
        .await
        .context("finish source-package verification")?;
    Ok(observed)
}

async fn project_component_facts(
    project_config: &PgConfig,
    control_config: &PgConfig,
    component: &AdmittedComponent,
    requirements: &[ComponentConnectionRequirement],
    manifest_sha256: &str,
    projection_hash: &str,
    directory: &PackageDirectory,
    package_path: &std::path::Path,
) -> anyhow::Result<DualPlaneProjectionOutcome> {
    // Source first. If the control write then fails, retry re-verifies this
    // immutable package and exact-replays the already committed source facts.
    let project = persist_plane(
        project_config,
        ProjectionPlane::SourceProject,
        component,
        requirements,
        manifest_sha256,
        projection_hash,
        directory,
        package_path,
    )
    .await?;
    let control = persist_plane(
        control_config,
        ProjectionPlane::Control,
        component,
        requirements,
        manifest_sha256,
        projection_hash,
        directory,
        package_path,
    )
    .await?;
    if project.manifest_sha256 != control.manifest_sha256
        || project.projection_hash != control.projection_hash
        || project.projection_hash != projection_hash
    {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorKind::PlaneContentMismatch,
            format!(
                "{}@{} project manifest/projection ({}, {}) disagrees with control ({}, {}) or admitted projection {}",
                component.scope.package_id,
                component.scope.package_version,
                project.manifest_sha256,
                project.projection_hash,
                control.manifest_sha256,
                control.projection_hash,
                projection_hash
            ),
        )
        .into());
    }
    Ok(DualPlaneProjectionOutcome { project, control })
}

async fn persist_plane(
    database_config: &PgConfig,
    plane: ProjectionPlane,
    component: &AdmittedComponent,
    requirements: &[ComponentConnectionRequirement],
    manifest_sha256: &str,
    projection_hash: &str,
    directory: &PackageDirectory,
    package_path: &std::path::Path,
) -> anyhow::Result<ProjectionOutcome> {
    let (mut client, connection) = database_config
        .connect(NoTls)
        .await
        .with_context(|| format!("connect to {plane} database"))?;
    let connection_task = tokio::spawn(connection);
    let stored = persist_with_client(
        &mut client,
        plane,
        component,
        requirements,
        manifest_sha256,
        projection_hash,
        directory,
        package_path,
    )
    .await;
    drop(client);
    if stored.is_err() {
        connection_task.abort();
    } else {
        connection_task
            .await
            .with_context(|| format!("join {plane} database connection"))?
            .with_context(|| format!("drive {plane} database connection"))?;
    }
    stored
}

async fn persist_with_client(
    client: &mut PgClient,
    plane: ProjectionPlane,
    component: &AdmittedComponent,
    requirements: &[ComponentConnectionRequirement],
    manifest_sha256: &str,
    projection_hash: &str,
    directory: &PackageDirectory,
    package_path: &std::path::Path,
) -> anyhow::Result<ProjectionOutcome> {
    let package = plan_package_migrations(directory, None)
        .context("validate package identity before component projection")?;
    ensure_component_package_matches(component, &package.coordinate)?;
    let transaction = client
        .transaction()
        .await
        .with_context(|| format!("begin {plane} component projection"))?;
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&component.scope.tenant_id])
        .await
        .with_context(|| format!("claim {plane} component tenant"))?;
    let coordinate = format!(
        "{}:{}@{}",
        component.scope.tenant_id, component.scope.package_id, component.scope.package_version
    );
    transaction
        .query_one(LOCK_PROJECTION_SQL, &[&coordinate])
        .await
        .with_context(|| format!("lock {plane} component projection coordinate"))?;
    let package_inserted = if plane == ProjectionPlane::Control {
        let existing = transaction
            .query_opt(
                SELECT_PACKAGE_SQL,
                &[
                    &component.scope.tenant_id,
                    &component.scope.package_id,
                    &component.scope.package_version,
                ],
            )
            .await
            .context("read control package root before projection")?;
        if let Some(existing) = existing {
            let recorded: String = existing.get(0);
            let recorded_predecessor: Option<String> = existing.get(1);
            if recorded != manifest_sha256
                || recorded_predecessor.as_deref() != package.predecessor_version.as_deref()
            {
                return Err(ComponentProjectionError::new(
                    ComponentProjectionErrorKind::PackageManifestMismatch,
                    format!(
                        "control {}@{} recorded-sha256={} presented-sha256={manifest_sha256} recorded-predecessor={recorded_predecessor:?} presented-predecessor={:?}",
                        component.scope.package_id,
                        component.scope.package_version,
                        recorded,
                        package.predecessor_version
                    ),
                )
                .into());
            }
            false
        } else {
            transaction
                .query_one(
                    REGISTER_PACKAGE_SQL,
                    &[
                        &component.scope.tenant_id,
                        &component.scope.package_id,
                        &component.scope.package_version,
                        &manifest_sha256,
                        &package.predecessor_version,
                    ],
                )
                .await
                .context("project immutable package root into control")?;
            true
        }
    } else {
        false
    };
    let observed_manifest = match plane {
        ProjectionPlane::SourceProject => {
            require_exact_applied_package(&transaction, component, directory, package_path).await?
        }
        ProjectionPlane::Control => {
            require_exact_package(
                &transaction,
                plane,
                component,
                manifest_sha256,
                package.predecessor_version.as_deref(),
                package_path,
            )
            .await?
        }
    };
    let (component_inserted, observed_projection_hash) =
        append_or_verify_admitted_component_count(&transaction, component, projection_hash).await?;
    let mut requirements_inserted = 0;
    // One transaction per plane: a library fact without its connection facts,
    // or the reverse, is never visible inside that plane.
    for requirement in requirements {
        requirements_inserted +=
            append_or_verify_requirement(&transaction, &component.scope.tenant_id, requirement)
                .await? as usize;
    }
    verify_requirement_inventory(
        &transaction,
        &component.scope.tenant_id,
        &component.component_digest,
        requirements,
    )
    .await?;
    transaction
        .commit()
        .await
        .with_context(|| format!("commit {plane} component projection"))?;
    Ok(ProjectionOutcome {
        package_inserted,
        component_inserted,
        requirements_inserted,
        manifest_sha256: observed_manifest,
        projection_hash: observed_projection_hash,
    })
}

async fn require_exact_applied_package(
    transaction: &Transaction<'_>,
    component: &AdmittedComponent,
    directory: &PackageDirectory,
    package_path: &std::path::Path,
) -> anyhow::Result<String> {
    let presented = plan_package_migrations(directory, None).map_err(|error| {
        ComponentProjectionError::new(
            ComponentProjectionErrorKind::SourcePackageMigrationMismatch,
            format!("package directory is invalid: {error}"),
        )
    })?;
    ensure_component_package_matches(component, &presented.coordinate)?;
    let Some(applied) = crate::apply_package::load_applied_package(
        transaction,
        &component.scope.tenant_id,
        &component.scope.package_id,
        &component.scope.package_version,
    )
    .await?
    else {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorKind::SourcePackageNotApplied,
            format!(
                "source-project lacks {}@{}; run wamn-ctl apply-package --package {} against the source project before push-component",
                component.scope.package_id,
                component.scope.package_version,
                package_path.display()
            ),
        )
        .into());
    };
    if applied.predecessor_version.as_deref() != presented.predecessor_version.as_deref() {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorKind::PackageManifestMismatch,
            format!(
                "source-project {}@{} recorded-predecessor={:?} presented-predecessor={:?}",
                component.scope.package_id,
                component.scope.package_version,
                applied.predecessor_version,
                presented.predecessor_version
            ),
        )
        .into());
    }
    let exact = plan_package_migrations(directory, Some(&applied)).map_err(|error| {
        let kind = if error.kind() == PackageMigrationErrorKind::ManifestDrift {
            ComponentProjectionErrorKind::PackageManifestMismatch
        } else {
            ComponentProjectionErrorKind::SourcePackageMigrationMismatch
        };
        ComponentProjectionError::new(kind, error.to_string())
    })?;
    if let Some(pending) = exact.pending.first() {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorKind::SourcePackageMigrationMismatch,
            format!(
                "source-project {}@{} is missing local migration {} (sha256={}); run apply-package before push-component",
                component.scope.package_id,
                component.scope.package_version,
                pending.relative_path,
                pending.sha256
            ),
        )
        .into());
    }
    Ok(exact.manifest_sha256)
}

async fn require_exact_package(
    transaction: &Transaction<'_>,
    plane: ProjectionPlane,
    component: &AdmittedComponent,
    manifest_sha256: &str,
    predecessor_version: Option<&str>,
    package_path: &std::path::Path,
) -> anyhow::Result<String> {
    let row = transaction
        .query_opt(
            SELECT_PACKAGE_SQL,
            &[
                &component.scope.tenant_id,
                &component.scope.package_id,
                &component.scope.package_version,
            ],
        )
        .await
        .with_context(|| format!("read {plane} package root"))?;
    let Some(row) = row else {
        let (kind, remedy) = if plane == ProjectionPlane::SourceProject {
            (
                ComponentProjectionErrorKind::SourcePackageNotApplied,
                format!(
                    "run wamn-ctl apply-package --package {} against the source project before push-component",
                    package_path.display()
                ),
            )
        } else {
            (
                ComponentProjectionErrorKind::PlaneContentMismatch,
                "control package projection did not persist its package root".to_owned(),
            )
        };
        return Err(ComponentProjectionError::new(
            kind,
            format!(
                "{plane} lacks {}@{}; {remedy}",
                component.scope.package_id, component.scope.package_version
            ),
        )
        .into());
    };
    let recorded: String = row.get(0);
    let recorded_predecessor: Option<String> = row.get(1);
    if recorded != manifest_sha256 || recorded_predecessor.as_deref() != predecessor_version {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorKind::PackageManifestMismatch,
            format!(
                "{plane} {}@{} recorded-sha256={} presented-sha256={manifest_sha256} recorded-predecessor={recorded_predecessor:?} presented-predecessor={predecessor_version:?}",
                component.scope.package_id, component.scope.package_version, recorded
            ),
        )
        .into());
    }
    Ok(recorded)
}

/// Append one portable connection requirement, or prove an exact retry.
async fn append_or_verify_requirement(
    transaction: &tokio_postgres::Transaction<'_>,
    tenant_id: &str,
    requirement: &ComponentConnectionRequirement,
) -> anyhow::Result<bool> {
    let canonical_json = String::from_utf8(requirement.canonical_bytes())
        .context("portable connection requirement is not UTF-8")?;
    let requirement_hash = requirement.requirement_hash();
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 5] = [
        &tenant_id,
        &requirement.component_digest(),
        &requirement.store_alias(),
        &canonical_json,
        &requirement_hash,
    ];
    let inserted = transaction
        .execute(
            wamn_schema_control::connections::insert_component_connection_requirement_sql(),
            &params,
        )
        .await
        .context("append portable component connection requirement")?
        == 1;
    let exact: bool = transaction
        .query_one(
            wamn_schema_control::connections::exact_component_connection_requirement_sql(),
            &params,
        )
        .await
        .context("verify portable component connection requirement")?
        .get(0);
    if !exact {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorKind::ConnectionFactConflict,
            format!(
                "component={} store-alias={} collides with different requirement bytes",
                requirement.component_digest(),
                requirement.store_alias()
            ),
        )
        .into());
    }
    Ok(inserted)
}

async fn verify_requirement_inventory(
    transaction: &Transaction<'_>,
    tenant_id: &str,
    component_digest: &str,
    requirements: &[ComponentConnectionRequirement],
) -> anyhow::Result<()> {
    let mut expected = BTreeMap::new();
    for requirement in requirements {
        let alias = requirement.store_alias().to_owned();
        let hash = requirement.requirement_hash();
        if expected.insert(alias.clone(), hash).is_some() {
            return Err(ComponentProjectionError::new(
                ComponentProjectionErrorKind::ConnectionFactConflict,
                format!(
                    "component={component_digest} repeats store-alias={alias} in its admitted projection"
                ),
            )
            .into());
        }
    }
    let observed = transaction
        .query(
            SELECT_REQUIREMENT_INVENTORY_SQL,
            &[&tenant_id, &component_digest],
        )
        .await
        .context("read exact component connection requirement inventory")?
        .into_iter()
        .map(|row| (row.get::<_, String>(0), row.get::<_, String>(1)))
        .collect::<BTreeMap<_, _>>();
    if observed != expected {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorKind::ConnectionFactConflict,
            format!(
                "component={component_digest} requirement inventory differs: expected={expected:?} observed={observed:?}"
            ),
        )
        .into());
    }
    Ok(())
}

/// Append one admitted component fact, or prove an exact retry.
///
/// The caller owns the transaction and tenant claim so release promotion can
/// combine this write with its target wiring and pointer cutover atomically.
pub(crate) async fn append_or_verify_admitted_component(
    transaction: &tokio_postgres::Transaction<'_>,
    component: &AdmittedComponent,
    projection_hash: &str,
) -> anyhow::Result<()> {
    append_or_verify_admitted_component_count(transaction, component, projection_hash)
        .await
        .map(|_| ())
}

async fn append_or_verify_admitted_component_count(
    transaction: &tokio_postgres::Transaction<'_>,
    component: &AdmittedComponent,
    projection_hash: &str,
) -> anyhow::Result<(bool, String)> {
    let imports =
        serde_json::to_string(&component.imports).context("serialize admitted imports")?;
    let effects =
        serde_json::to_string(&component.effects).context("serialize admitted effects")?;
    let input_ports =
        serde_json::to_string(&component.input_ports).context("serialize admitted input ports")?;
    let output_ports = serde_json::to_string(&component.output_ports)
        .context("serialize admitted output ports")?;
    let parameters =
        serde_json::to_string(&component.parameters).context("serialize admitted parameters")?;
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 15] = [
        &component.scope.tenant_id,
        &component.scope.package_id,
        &component.scope.package_version,
        &component.component,
        &component.interface_version,
        &component.operation,
        &component.registered_operation,
        &component.component_digest,
        &projection_hash,
        &imports,
        &component.imports_fingerprint,
        &effects,
        &input_ports,
        &output_ports,
        &parameters,
    ];

    let inserted = transaction
        .query_opt(INSERT_COMPONENT_SQL, &params)
        .await
        .context("append admitted component-library fact")?
        .is_some();
    if !inserted {
        let exact: bool = transaction
            .query_one(EXACT_COMPONENT_SQL, &params)
            .await
            .context("verify existing component-library fact")?
            .get(0);
        if !exact {
            return Err(ComponentProjectionError::new(
                ComponentProjectionErrorKind::ComponentFactConflict,
                format!(
                    "{}@{} component={} interface-version={} collides with different admitted facts",
                    component.scope.package_id,
                    component.scope.package_version,
                    component.component,
                    component.interface_version
                ),
            )
            .into());
        }
    }
    let observed_projection_hash: String = transaction
        .query_one(
            SELECT_COMPONENT_PROJECTION_HASH_SQL,
            &[
                &component.scope.tenant_id,
                &component.scope.package_id,
                &component.scope.package_version,
                &component.component,
                &component.interface_version,
            ],
        )
        .await
        .context("read persisted component projection hash")?
        .get(0);
    Ok((inserted, observed_projection_hash))
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::path::Path;

    use wamn_catalog::ComponentPackageScope;

    use super::*;

    fn projection_component() -> AdmittedComponent {
        AdmittedComponent {
            scope: ComponentPackageScope {
                tenant_id: "tenant-a".to_owned(),
                package_id: "wamn_receiving".to_owned(),
                package_version: "1.0.0".to_owned(),
            },
            component: "receiving_data".to_owned(),
            interface_version: "0.1.0".to_owned(),
            operation: "run".to_owned(),
            registered_operation: Some("wamn_receiving@1.0.0::purchase_order.get".to_owned()),
            component_digest: format!("sha256:{}", "a".repeat(64)),
            imports: Vec::new(),
            imports_fingerprint: format!("sha256:{}", "b".repeat(64)),
            effects: Vec::new(),
            input_ports: Vec::new(),
            output_ports: Vec::new(),
            parameters: Vec::new(),
        }
    }

    #[test]
    fn projection_hash_is_complete_and_requirement_order_independent() {
        let component = projection_component();
        let left = ComponentConnectionRequirement::new(
            &component.component_digest,
            "supplier",
            ConnectionTypeDescriptor::http_v1(),
        );
        let right = ComponentConnectionRequirement::new(
            &component.component_digest,
            "warehouse",
            ConnectionTypeDescriptor::http_v1(),
        );
        let requirements = [left, right];
        let forward =
            admitted_projection_hash(&component, &requirements).expect("projection hashes");
        let reversed = admitted_projection_hash(
            &component,
            &[requirements[1].clone(), requirements[0].clone()],
        )
        .expect("reordered projection hashes");
        assert_eq!(forward, reversed);

        let mut changed = component;
        changed.registered_operation =
            Some("wamn_receiving@1.0.0::purchase_order.query".to_owned());
        assert_ne!(
            forward,
            admitted_projection_hash(&changed, &requirements).expect("changed projection hashes")
        );
    }

    #[test]
    fn dual_plane_replay_is_noop_only_when_both_planes_are_noop() {
        let outcome = |component_inserted| ProjectionOutcome {
            package_inserted: false,
            component_inserted,
            requirements_inserted: 0,
            manifest_sha256: format!("sha256:{}", "a".repeat(64)),
            projection_hash: format!("sha256:{}", "b".repeat(64)),
        };
        let converged = DualPlaneProjectionOutcome {
            project: outcome(false),
            control: outcome(false),
        };
        assert!(converged.is_noop());
        assert!(
            !DualPlaneProjectionOutcome {
                project: outcome(false),
                control: outcome(true),
            }
            .is_noop()
        );
    }

    async fn connect(config: &PgConfig) -> PgClient {
        let (client, connection) = config
            .connect(NoTls)
            .await
            .expect("connect to live database");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    }

    fn database_config(base: &PgConfig, database: &str) -> PgConfig {
        let mut config = base.clone();
        config.dbname(database);
        config
    }

    async fn run_alone(client: &PgClient, sql: &str) {
        client
            .batch_execute(sql)
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    #[tokio::test]
    async fn dual_plane_persistence_reverifies_partial_retry_and_converges() {
        let Ok(url) = std::env::var("WAMN_CTL_PG_URL") else {
            eprintln!("skipping dual-plane projection proof; WAMN_CTL_PG_URL is unset");
            return;
        };
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(std::env::temp_dir().join("wamn-ctl-live-database.lock"))
            .expect("open shared ctl database lock");
        lock.lock()
            .expect("lock disposable PostgreSQL across tests");

        let base_config: PgConfig = url.parse().expect("parse WAMN_CTL_PG_URL");
        let base = connect(&base_config).await;
        let suffix = std::process::id();
        let project_database = format!("wamn_projection_project_{suffix}");
        let control_database = format!("wamn_projection_control_{suffix}");
        for database in [&project_database, &control_database] {
            run_alone(
                &base,
                &format!("DROP DATABASE IF EXISTS \"{database}\" WITH (FORCE)"),
            )
            .await;
        }
        base.batch_execute(
            "DO $roles$ DECLARE role_name text; BEGIN \
               FOREACH role_name IN ARRAY ARRAY[\
                 'wamn_system', 'wamn_control_author', 'wamn_app', 'wamn_scenario_author'\
               ] LOOP \
                 IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN \
                   EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                                   NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
                 END IF; \
               END LOOP; \
             END $roles$;",
        )
        .await
        .expect("ensure production schema prerequisite roles");
        for database in [&project_database, &control_database] {
            run_alone(&base, &format!("CREATE DATABASE \"{database}\"")).await;
        }
        run_alone(
            &base,
            &format!("GRANT CREATE ON DATABASE \"{control_database}\" TO wamn_system"),
        )
        .await;

        let project_config = database_config(&base_config, &project_database);
        let control_config = database_config(&base_config, &control_database);
        let mut project = connect(&project_config).await;
        let mut control = connect(&control_config).await;
        project
            .batch_execute(include_str!("../../../deploy/sql/catalog-schema.sql"))
            .await
            .expect("install production project catalog");
        control
            .batch_execute("SET ROLE wamn_system")
            .await
            .expect("assume production control owner");
        control
            .batch_execute(include_str!("../../../deploy/sql/system-schema.sql"))
            .await
            .expect("install production control system schema");
        control
            .batch_execute(include_str!(
                "../../../deploy/sql/control-portable-store.sql"
            ))
            .await
            .expect("install production portable control store");
        control
            .batch_execute("RESET ROLE")
            .await
            .expect("restore test administrator");

        let package_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/receiving");
        let directory = crate::apply_package::read_package_directory(&package_path)
            .expect("read real Receiving package");
        let package = plan_package_migrations(&directory, None).expect("plan real package");
        let component = projection_component();
        let requirements = vec![ComponentConnectionRequirement::new(
            &component.component_digest,
            "warehouse",
            ConnectionTypeDescriptor::http_v1(),
        )];
        let projection_hash = admitted_projection_hash(&component, &requirements)
            .expect("hash admitted projection once");

        let missing =
            verify_source_package_with_client(&mut project, &component, &directory, &package_path)
                .await
                .expect_err("an unapplied source package refuses");
        assert_eq!(
            missing
                .downcast_ref::<ComponentProjectionError>()
                .expect("missing package is a typed refusal")
                .kind(),
            ComponentProjectionErrorKind::SourcePackageNotApplied
        );
        project
            .query_one(
                REGISTER_PACKAGE_SQL,
                &[
                    &component.scope.tenant_id,
                    &component.scope.package_id,
                    &component.scope.package_version,
                    &package.manifest_sha256,
                    &package.predecessor_version,
                ],
            )
            .await
            .expect("seed the package root apply-package already proved");
        let incomplete =
            verify_source_package_with_client(&mut project, &component, &directory, &package_path)
                .await
                .expect_err("a package root without its exact migration ledger refuses");
        assert_eq!(
            incomplete
                .downcast_ref::<ComponentProjectionError>()
                .expect("incomplete migration ledger is a typed refusal")
                .kind(),
            ComponentProjectionErrorKind::SourcePackageMigrationMismatch
        );
        let mut manifest_drift = directory.clone();
        manifest_drift.manifest_bytes.push(b'\n');
        let drift = verify_source_package_with_client(
            &mut project,
            &component,
            &manifest_drift,
            &package_path,
        )
        .await
        .expect_err("a different raw manifest hash refuses");
        assert_eq!(
            drift
                .downcast_ref::<ComponentProjectionError>()
                .expect("manifest drift is a typed refusal")
                .kind(),
            ComponentProjectionErrorKind::PackageManifestMismatch
        );
        for migration in &package.pending {
            let ordinal = i32::try_from(migration.ordinal).expect("migration ordinal fits i32");
            project
                .execute(
                    "INSERT INTO catalog.package_migrations \
                     (tenant_id, package_id, package_version, ordinal, relative_path, sha256) \
                     VALUES ($1, $2, $3, $4, $5, $6)",
                    &[
                        &component.scope.tenant_id,
                        &component.scope.package_id,
                        &component.scope.package_version,
                        &ordinal,
                        &migration.relative_path,
                        &migration.sha256,
                    ],
                )
                .await
                .expect("seed exact apply-package migration ledger");
        }
        verify_source_package_with_client(&mut project, &component, &directory, &package_path)
            .await
            .expect("an exact local stream and applied ledger agree");
        let mut migration_drift = directory.clone();
        migration_drift.migrations[0].bytes.push(b'\n');
        let drift = verify_source_package_with_client(
            &mut project,
            &component,
            &migration_drift,
            &package_path,
        )
        .await
        .expect_err("different local migration bytes refuse");
        assert_eq!(
            drift
                .downcast_ref::<ComponentProjectionError>()
                .expect("migration drift is a typed refusal")
                .kind(),
            ComponentProjectionErrorKind::SourcePackageMigrationMismatch
        );

        let partial = persist_plane(
            &project_config,
            ProjectionPlane::SourceProject,
            &component,
            &requirements,
            &package.manifest_sha256,
            &projection_hash,
            &directory,
            &package_path,
        )
        .await
        .expect("commit source-project half before interruption");
        assert!(!partial.is_noop());
        let resumed = project_component_facts(
            &project_config,
            &control_config,
            &component,
            &requirements,
            &package.manifest_sha256,
            &projection_hash,
            &directory,
            &package_path,
        )
        .await
        .expect("partial retry re-verifies source and projects control");
        assert!(resumed.project.is_noop());
        assert!(!resumed.control.is_noop());
        let again = project_component_facts(
            &project_config,
            &control_config,
            &component,
            &requirements,
            &package.manifest_sha256,
            &projection_hash,
            &directory,
            &package_path,
        )
        .await
        .expect("exact dual-plane replay converges");
        assert!(again.project.is_noop());
        assert!(again.control.is_noop());
        assert!(again.is_noop());
        assert_eq!(again.project.projection_hash, projection_hash);
        assert_eq!(again.control.projection_hash, projection_hash);

        let unexpected = ComponentConnectionRequirement::new(
            &component.component_digest,
            "unexpected",
            ConnectionTypeDescriptor::http_v1(),
        );
        let transaction = control
            .transaction()
            .await
            .expect("begin control requirement-inventory mutation");
        transaction
            .query_one(CLAIM_TENANT_SQL, &[&component.scope.tenant_id])
            .await
            .expect("claim control tenant for requirement-inventory mutation");
        append_or_verify_requirement(&transaction, &component.scope.tenant_id, &unexpected)
            .await
            .expect("seed an extra control-plane requirement");
        transaction
            .commit()
            .await
            .expect("commit extra control-plane requirement");
        let extra = project_component_facts(
            &project_config,
            &control_config,
            &component,
            &requirements,
            &package.manifest_sha256,
            &projection_hash,
            &directory,
            &package_path,
        )
        .await
        .expect_err("a plane carrying an extra alias/hash requirement refuses");
        assert_eq!(
            extra
                .downcast_ref::<ComponentProjectionError>()
                .expect("extra requirement inventory is a typed refusal")
                .kind(),
            ComponentProjectionErrorKind::ConnectionFactConflict
        );

        drop(project);
        drop(control);
        for database in [&project_database, &control_database] {
            run_alone(&base, &format!("DROP DATABASE \"{database}\" WITH (FORCE)")).await;
        }
    }

    /// The exact bytes this publisher stores in `requirement_json`, frozen
    /// whole. `requirement_hash` is the SHA-256 of these same bytes, so an
    /// added, removed, or renamed field moves the persisted identity of every
    /// component connection and must not pass silently.
    #[test]
    fn the_minted_portable_requirement_document_is_frozen() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let requirement = portable_requirement(
            &digest,
            &ComponentConnection {
                store_alias: "erp".to_owned(),
                requirement_type: ComponentConnectionType::Http,
            },
        );

        assert_eq!(
            String::from_utf8(requirement.canonical_bytes()).expect("canonical bytes are UTF-8"),
            format!(
                r#"{{"component-digest":"{digest}","store-alias":"erp","requirement":{{"descriptor-version":"1","requirement-type":"http","contract":"wamn:connection/http@0.1.0","authority-model":"http-origin","field-ownership":[{{"field":"method","owner":"author"}},{{"field":"relative-target","owner":"author"}},{{"field":"headers","owner":"author"}},{{"field":"body","owner":"author"}},{{"field":"authority","owner":"environment"}},{{"field":"tls","owner":"environment"}},{{"field":"redirect","owner":"environment"}},{{"field":"proxy","owner":"environment"}},{{"field":"credential","owner":"environment"}}],"credential-injection":"environment-selected-http-header"}}}}"#
            )
        );
    }

    #[test]
    fn production_publisher_layout_matches_the_puller_contract() {
        let component_bytes = b"component-bytes";
        let config_bytes = b"config-bytes";
        let expected = component_artifact_layout(component_bytes, config_bytes);
        let (layer, config, manifest) = artifact_layout(component_bytes, config_bytes);
        let digest = wamn_runtime::component_admission::component_digest(component_bytes);
        let reference = component_artifact_reference("registry.example/wamn/components", &digest)
            .expect("shared digest reference derives");

        assert_eq!(
            expected.layer_media_type(),
            "application/vnd.wamn.component.v1+wasm"
        );
        assert_eq!(
            expected.config_media_type(),
            "application/vnd.wamn.component.config.v1+json"
        );
        assert_eq!(expected.manifest_schema_version(), 2);
        assert_eq!(expected.layer_count(), 1);
        assert_eq!(reference.tag(), digest.trim_start_matches("sha256:"));
        assert_eq!(
            reference.to_string(),
            format!("registry.example/wamn/components:{}", reference.tag())
        );
        assert_eq!(&layer.data[..], expected.component_bytes());
        assert_eq!(layer.media_type, expected.layer_media_type());
        assert_eq!(&config.data[..], expected.config_bytes());
        assert_eq!(config.media_type, expected.config_media_type());
        assert_eq!(manifest.schema_version, expected.manifest_schema_version());
        assert_eq!(manifest.layers.len(), expected.layer_count());
        assert_eq!(manifest.layers[0].media_type, expected.layer_media_type());
        assert_eq!(
            manifest.layers[0].digest,
            wamn_runtime::component_admission::component_digest(component_bytes)
        );
        assert_eq!(
            manifest.layers[0].size,
            i64::try_from(component_bytes.len()).expect("fixture size fits")
        );
        assert_eq!(manifest.config.media_type, expected.config_media_type());
        assert_eq!(
            manifest.config.digest,
            wamn_runtime::component_admission::component_digest(config_bytes)
        );
        assert_eq!(
            manifest.config.size,
            i64::try_from(config_bytes.len()).expect("fixture size fits")
        );
    }

    #[tokio::test]
    #[ignore = "requires a disposable registry in WAMN_COMPONENT_ARTIFACT_BASE"]
    async fn production_publisher_and_puller_round_trip_exact_bytes() {
        let artifact_base = std::env::var("WAMN_COMPONENT_ARTIFACT_BASE")
            .expect("set WAMN_COMPONENT_ARTIFACT_BASE to a disposable HTTP registry/repository");
        let registry_auth_file = std::env::var("WAMN_REGISTRY_AUTH_FILE")
            .expect("set WAMN_REGISTRY_AUTH_FILE to its Docker config credential");
        let component_bytes = b"\0asm\r\0\x01\0";
        let engine = wamn_runtime::build_engine(&[]).expect("component admission engine builds");
        let component = validate_component_admission(
            &engine,
            component_bytes,
            wamn_runtime::component_admission::ComponentAdmissionRequest {
                declaration: ComponentDeclaration {
                    scope: ComponentPackageScope {
                        tenant_id: "tenant-a".to_owned(),
                        package_id: "orders".to_owned(),
                        package_version: "1.0.0".to_owned(),
                    },
                    component: "round-trip".to_owned(),
                    interface_version: "0.1.0".to_owned(),
                    operation: "run".to_owned(),
                    registered_operation: None,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    connections: Vec::new(),
                },
                admitted_platform_packages: std::collections::BTreeSet::new(),
            },
        )
        .expect("fixture bytes admit")
        .component;
        let artifact = component_artifact_reference(&artifact_base, &component.component_digest)
            .expect("fixture artifact reference derives");
        let reference = Reference::with_tag(
            artifact.registry().to_owned(),
            artifact.repository().to_owned(),
            artifact.tag().to_owned(),
        );
        let config_bytes = component_artifact_config_bytes(&component);
        let credentials = read_registry_credentials(
            PathBuf::from(registry_auth_file).as_path(),
            artifact.registry(),
        )
        .expect("load live registry credential");

        publish_and_verify(
            &reference,
            &artifact_base,
            true,
            component_bytes,
            &config_bytes,
            &component,
            &credentials,
        )
        .await
        .expect("production publisher and puller agree");
    }
}

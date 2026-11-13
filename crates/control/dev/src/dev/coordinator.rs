//! Production stage coordinator for the canonical development loop.
//!
//! This module carries exact outputs between existing owners. It does not
//! reproduce migration, generation, admission, publication, release, or
//! activation semantics.

#[cfg(test)]
use wamn_scalar::SqlValueType;
pub(super) mod plan;
use plan::{
    base_digests_stage_error, declaration_stage_error, project_catalog_for_package,
    require_admitted_identity, require_command_success, require_wiring_store_alias,
};

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use serde::Deserialize;
use tokio::process::Command;
use tokio_postgres::NoTls;
use wamn_catalog::PackageCoordinate;
use wamn_compiler::manifest::PackageManifest;
use wamn_environment_control::reconcile_package_data_access;
use wamn_package_control::apply_package::{self, ApplyPackageRequest};
use wamn_package_control::component_admission::{
    AdmitComponentRequest, ComponentAdmission, admit_component,
};
use wamn_package_control::component_declaration::{
    ComponentDeclarationError, ComponentDeclarationErrorType, PACKAGE_MANIFEST,
    authored_base_digests, declared_platform_packages, render_declaration_document,
    render_palette_declaration,
};
use wamn_release_control::publish_release::{self, PublishReleaseRequest};
use wamn_release_control::release_composition::{
    CompositionError, CompositionErrorType, PackageInput, SelectedComponentArtifact,
    SelectedPaletteArtifact, select_component_artifacts, select_palette_artifacts,
};
use wamn_schema_control::BareSchemaName;
use wamn_schema_introspection::ir::CatalogIr;

use super::activation::{self, DevActivation, DevActivationRequest};
use super::config::{DevConfig, ResolvedDevPackages, VerifiedBaseComponentDigest};
use super::observations::DevObservationReaders;
use super::read::{DevReadHandle, DevReadPublisher, DevRuntimeEndpoint, dev_read_channel};
use super::target_database;
use super::watch::GitSource;
use super::{DevRunNotice, DevStage, DevStageFailure, DevStageRunner};
use wamn_release_control::release_carrier::ReleaseCarrier;

const RUN_SCHEMA: &str = "wamn_run";

/// Stable code for the notice a run emits when it built past an authored pin.
pub const BASE_PIN_STALE_NOTICE: &str = "pin stale";
pub(super) const PACKAGE_COMPONENTS: &str = "publication/components";
pub(super) const NODE_CAPABILITY: &str = "wamn:node";
pub(super) const POSTGRES_CAPABILITY: &str = "wamn:postgres";

static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Stable category of a concrete development-stage failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductionDevStageErrorType {
    InvalidState,
    StageOwner,
    AuthenticationUnavailable,
}

impl ProductionDevStageErrorType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidState => "dev-stage-state-invalid",
            Self::StageOwner => "dev-stage-owner-failed",
            Self::AuthenticationUnavailable => "authentication-unavailable",
        }
    }
}

/// Contextual failure translated once at the production orchestration boundary.
#[derive(Debug)]
pub struct ProductionDevStageError {
    type_: ProductionDevStageErrorType,
    operation: &'static str,
    endpoint: Option<Box<str>>,
    detail: Box<str>,
    source: Option<anyhow::Error>,
}

impl ProductionDevStageError {
    fn invalid(operation: &'static str, detail: impl Into<Box<str>>) -> Self {
        Self {
            type_: ProductionDevStageErrorType::InvalidState,
            operation,
            endpoint: None,
            detail: detail.into(),
            source: None,
        }
    }

    fn owner(operation: &'static str, source: anyhow::Error) -> Self {
        // The whole chain, top context first. Display alone prints only the
        // outermost context, which cost two fixture runs to recover the
        // refusal that actually stopped the stage (wamn-aij8).
        let detail = format!("{source:#}").into_boxed_str();
        Self {
            type_: ProductionDevStageErrorType::StageOwner,
            operation,
            endpoint: None,
            detail,
            source: Some(source),
        }
    }

    fn owner_at(
        operation: &'static str,
        endpoint: impl Into<Box<str>>,
        detail: impl Into<Box<str>>,
    ) -> Self {
        Self {
            type_: ProductionDevStageErrorType::StageOwner,
            operation,
            endpoint: Some(endpoint.into()),
            detail: detail.into(),
            source: None,
        }
    }

    fn authentication_unavailable(endpoint: impl Into<Box<str>>, source: anyhow::Error) -> Self {
        Self {
            type_: ProductionDevStageErrorType::AuthenticationUnavailable,
            operation: "re-authenticate publisher",
            endpoint: Some(endpoint.into()),
            detail: format!("the configured identity authority is unavailable: {source:#}")
                .into_boxed_str(),
            source: Some(source),
        }
    }

    /// Stable failure category.
    pub const fn error_type(&self) -> ProductionDevStageErrorType {
        self.type_
    }

    /// Credential-free endpoint involved in this failure, when applicable.
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }
}

impl fmt::Display for ProductionDevStageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} while {}",
            self.type_.as_str(),
            self.operation
        )?;
        if let Some(endpoint) = &self.endpoint {
            write!(formatter, " at {endpoint}")?;
        }
        write!(formatter, ": {}", self.detail)
    }
}

impl From<CompositionError> for ProductionDevStageError {
    fn from(error: CompositionError) -> Self {
        match (error.type_, error.source) {
            (CompositionErrorType::Owner, Some(source)) => Self::owner(error.operation, source),
            _ => Self::invalid(error.operation, error.detail),
        }
    }
}

impl Error for ProductionDevStageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_ref().map(AsRef::as_ref)
    }
}

#[derive(Debug)]
struct PreparedLocalGrants {
    target: target_database::PreparedConfiguration,
    data_access: reconcile_package_data_access::PreparedLocalDataAccess,
    inputs: Vec<String>,
}

/// Concrete runner carrying production-owner outputs through all ten stages.
pub struct ProductionDevStageRunner {
    config: DevConfig,
    pub(super) environment_file: PathBuf,
    environment: wamn_environment_control::environment::document::EnvironmentDocument,
    overlay_root: PathBuf,
    git: GitSource,
    packages: Option<ResolvedDevPackages>,
    catalogs: BTreeMap<String, CatalogIr>,
    artifacts: Vec<SelectedComponentArtifact>,
    palette_artifacts: Vec<SelectedPaletteArtifact>,
    verified_base_digests: Vec<VerifiedBaseComponentDigest>,
    admissions: Vec<ComponentAdmission>,
    target_instance: Option<String>,
    target_lease: Option<target_database::TargetLease>,
    release: Option<ReleaseCarrier>,
    local_bindings: Vec<wamn_runtime::local_application::LocalBindingFacts>,
    local_binding_inputs: Vec<PreparedLocalBinding>,
    local_grants: Option<PreparedLocalGrants>,
    activation: Option<DevActivation>,
    cdc_reader: Option<super::cdc_reader::CdcReaderProcess>,
    /// The generation digest of each package that this run reads, bases first.
    build_inputs: Option<Vec<String>>,
    /// The schema digest of each package that this run reads, bases first.
    schema_inputs: Option<Vec<String>>,
    /// The candidate schema inputs last compared with installed SQL.
    prepared_schema_inputs: Option<Vec<String>>,
    /// Packages whose generation still needs component compilation.
    pending_builds: BTreeSet<String>,
    composed_entry: Option<publish_release::ComposedApplication>,
    /// Inputs of each verifier's last successful SQLx preparation; `None` while unknown.
    sqlx_metadata_inputs: BTreeMap<String, Option<SqlxMetadataInputs>>,
    /// The build inputs whose grants Activate last applied.
    acl_inputs: Option<Vec<String>>,
    read_publisher: DevReadPublisher,
    read_handle: DevReadHandle,
    observation_readers: Option<DevObservationReaders>,
}

impl fmt::Debug for ProductionDevStageRunner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductionDevStageRunner")
            .field("config", &self.config)
            .field("overlay_root", &self.overlay_root)
            .field("repository_root", &self.git.repository_root())
            .field(
                "package_count",
                &self.package_inputs().map(|inputs| inputs.len()),
            )
            .field("catalog_count", &self.catalogs.len())
            .field("artifact_count", &self.artifacts.len())
            .field("palette_artifact_count", &self.palette_artifacts.len())
            .field(
                "verified_base_digest_count",
                &self.verified_base_digests.len(),
            )
            .field(
                "base_digests_moved_off_pin",
                &self.base_digests_moved_off_pin().collect::<Vec<_>>(),
            )
            .field("admission_count", &self.admissions.len())
            .field("release", &self.release)
            .field("activation", &self.activation)
            .field(
                "observation_readers_started",
                &self.observation_readers.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl ProductionDevStageRunner {
    /// Bind one strict request to its Git source.
    pub fn new(
        config: DevConfig,
        overlay_root: PathBuf,
        git: GitSource,
        environment_file: PathBuf,
        environment: wamn_environment_control::environment::document::EnvironmentDocument,
    ) -> Self {
        let (read_publisher, read_handle) = dev_read_channel();
        Self {
            config,
            environment_file,
            environment,
            overlay_root,
            git,
            packages: None,
            catalogs: BTreeMap::new(),
            artifacts: Vec::new(),
            palette_artifacts: Vec::new(),
            verified_base_digests: Vec::new(),
            admissions: Vec::new(),
            target_instance: None,
            target_lease: None,
            release: None,
            local_bindings: Vec::new(),
            local_binding_inputs: Vec::new(),
            local_grants: None,
            activation: None,
            cdc_reader: None,
            build_inputs: None,
            schema_inputs: None,
            prepared_schema_inputs: None,
            pending_builds: BTreeSet::new(),
            composed_entry: None,
            sqlx_metadata_inputs: BTreeMap::new(),
            acl_inputs: None,
            read_publisher,
            read_handle,
            observation_readers: None,
        }
    }

    /// Read-only state for terminal and future console clients.
    pub fn read_handle(&self) -> DevReadHandle {
        self.read_handle.clone()
    }

    /// Base coordinates this run built off their pinned digest, with both values.
    ///
    /// A disposable target accepts a moved digest instead of stopping the loop,
    /// so the drift is only visible if the run reports it. Nothing is written
    /// back to the authored manifest.
    pub fn base_digests_moved_off_pin(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.verified_base_digests.iter().filter_map(|verified| {
            verified
                .superseded_pin()
                .map(|pin| (verified.coordinate(), pin, verified.digest()))
        })
    }

    /// Base component digests THIS RUN built, by package coordinate.
    fn built_base_digests(&self) -> BTreeMap<Box<str>, Box<str>> {
        self.verified_base_digests
            .iter()
            .map(|verified| (verified.coordinate().into(), verified.digest().into()))
            .collect()
    }

    /// Start the two read-only environment observation sources once.
    pub async fn start_observations(&mut self) -> Result<(), ProductionDevStageError> {
        if self.observation_readers.is_some() {
            return Ok(());
        }
        self.observation_readers = Some(
            DevObservationReaders::start(&self.config, self.read_publisher.clone())
                .await
                .map_err(|source| {
                    ProductionDevStageError::owner(
                        "start development observation readers",
                        source.into(),
                    )
                })?,
        );
        Ok(())
    }

    /// Exact release carrier published and pushed by the Release stage.
    pub const fn release_carrier(&self) -> Option<&ReleaseCarrier> {
        self.release.as_ref()
    }

    /// Stop any active workload and local host owned by this runner.
    pub async fn shutdown(&mut self) -> Result<(), ProductionDevStageError> {
        let endpoint_file = self
            .config
            .local_artifacts()
            .directory
            .with_file_name("application-endpoint.json");
        if endpoint_file.exists() {
            std::fs::remove_file(endpoint_file).map_err(|source| {
                ProductionDevStageError::owner("remove the local web endpoint", source.into())
            })?;
        }
        self.read_publisher.clear_runtime_endpoint();
        // The reader stops first, so that a target replacement finds its
        // slot inactive.
        let reader_result = match self.cdc_reader.take() {
            Some(reader) => reader
                .stop()
                .await
                .map_err(|source| ProductionDevStageError::owner("stop the CDC reader", source)),
            None => Ok(()),
        };
        let Some(active) = self.activation.take() else {
            return reader_result;
        };
        let activation_result = active
            .shutdown()
            .await
            .map_err(|source| ProductionDevStageError::owner("clean up activation", source.into()));
        reader_result.and(activation_result)
    }

    async fn migrate(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Migrate);
        let packages = super::config::resolve_dev_packages(&self.config, &self.overlay_root)
            .map_err(|source| {
                ProductionDevStageError::owner("resolve package closure", source.into())
            })?;
        self.packages = Some(packages);
        let package_inputs = self.package_inputs()?;

        let run_schema = BareSchemaName::new(RUN_SCHEMA)
            .expect("the repository-owned run schema is a valid bare identifier");
        wamn_environment_control::verification_policy::project_environment_policy(
            self.config.system_database_url(),
            self.config.target_database_url(),
            &run_schema,
            &self.config.activation_identity().org,
            &self.config.activation_identity().project_env_key(),
            &self.config.activation_identity().environment,
        )
        .await
        .map_err(|source| ProductionDevStageError::owner("project environment policy", source))?;

        for package in package_inputs {
            let outcome = Box::pin(apply_package::apply_local_package(
                ApplyPackageRequest {
                    changes: wamn_broker::Publisher::new(
                        self.config.nats().connect().await.map_err(|source| {
                            ProductionDevStageError::owner("connect auth changes", source)
                        })?,
                    ),
                    package: apply_package::PackageRoot {
                        path: package.root,
                        artifact_digest: None,
                    },
                    database_url: self.config.target_database_url().to_owned(),
                    org: self.config.activation_identity().org.clone(),
                    project: self.config.activation_identity().project.clone(),
                    environment: self.config.activation_identity().environment.clone(),
                    component_source:
                        wamn_package_control::apply_package::component_source_from_env(None)
                            .map_err(|source| {
                                ProductionDevStageError::owner("component repository", source)
                            })?,
                },
                &self.config.activation_identity().environment,
            ))
            .await
            .map_err(|source| {
                ProductionDevStageError::owner("apply package to verification", source)
            })?;
            tracing::info!(package = %outcome.package_id, version = %outcome.package_version, migrations = outcome.migrations_applied, changed = outcome.changed, "applied development package");
        }
        self.grant_operator_admin_role().await?;
        self.prepared_schema_inputs.clone_from(&self.schema_inputs);
        Ok(())
    }

    /// Give the environment's operator service the `admin` role. Applying
    /// the package wrote the role above, and `reconcile-run-plane` wrote the
    /// service's `user` row when the environment was made.
    async fn grant_operator_admin_role(&self) -> Result<(), ProductionDevStageError> {
        let identity = self.config.activation_identity();
        super::environment::grant_operator_admin_role(
            self.config.system_database_url(),
            self.config.target_database_url(),
            &identity.org,
            &identity.project,
            &identity.environment,
            &identity.project_env_key(),
            self.config.platform_domain(),
        )
        .await
        .map_err(|source| ProductionDevStageError::owner("grant the operator role", source))
    }

    async fn introspect(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Introspect);
        let packages = self.package_inputs()?;
        for package in &packages {
            let catalog =
                wamn_compiler::introspect_package(self.config.target_database_url(), &package.root)
                    .await
                    .map_err(|source| {
                        ProductionDevStageError::owner("introspect package", source)
                    })?;
            let catalog = project_catalog_for_package(&catalog, &package.manifest, &packages)?;
            self.catalogs
                .insert(package.manifest.package.id.clone(), catalog);
        }
        // Recorded only after Introspect read every package, so a restarted
        // session never keeps a target whose Migrate or Introspect did not
        // finish.
        target_database::record_target_schema(
            &self.config,
            self.target_instance
                .as_deref()
                .expect("prepare_run assigned the target instance"),
            self.prepared_schema_inputs
                .as_deref()
                .expect("Migrate recorded the schema inputs"),
            &self.catalogs,
        )
        .map_err(|source| ProductionDevStageError::owner("record the target schema", source))?;
        Ok(())
    }

    async fn prepare_generation(&mut self) -> Result<(), ProductionDevStageError> {
        // `wamn build` of each package, bases first, without its components:
        // an overlay's build reads the build.json of each of its bases. Every
        // build describes its statements against its own fresh database on the
        // target's server. The Build stage compiles the components.
        let packages = self.package_inputs()?;
        let current = current_package_builds(&packages);
        self.pending_builds.extend(
            current
                .iter()
                .filter_map(|(id, build)| build.is_none().then_some(id.clone())),
        );
        let database_url = self.config.target_database_url();
        generate_changed_packages(packages, &current, |package| async move {
            let output_root = wamn_compiler::build::default_output_root(&package.root);
            wamn_compiler::build::generate_package(&wamn_compiler::build::BuildRequest::new(
                &package.root,
                &output_root,
                database_url,
            ))
            .await
            .map_err(|source| ProductionDevStageError::owner("build package", source))
        })
        .await?;
        Ok(())
    }

    async fn generate(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Generate);
        let selected = self
            .package_inputs()?
            .into_iter()
            .filter(|package| self.pending_builds.contains(&package.manifest.package.id))
            .collect::<Vec<_>>();
        if selected.iter().any(|package| {
            wamn_release_control::delivery::sqlx::requires_verifier(&package.manifest)
        }) {
            let output = super::execute_preparation(
                Command::new("cargo").args(["sqlx", "--version"]),
                super::INPUT_COMMAND_TIMEOUT,
            )
            .await
            .map_err(|source| ProductionDevStageError::owner("read SQLx CLI version", source))?;
            require_command_success("read SQLx CLI version", &output)?;
            wamn_release_control::delivery::sqlx::require_cli_version(&output.stdout).map_err(
                |source| ProductionDevStageError::owner("require pinned SQLx CLI", source),
            )?;
        }
        for package in selected {
            let package_id = &package.manifest.package.id;
            if wamn_release_control::delivery::sqlx::requires_verifier(&package.manifest) {
                let built =
                    wamn_compiler::build_record::read(&wamn_compiler::output_root(&package.root))
                        .map_err(|source| {
                        ProductionDevStageError::owner("read generated package", source)
                    })?;
                let current = SqlxMetadataInputs::of(&built);
                if sqlx_metadata_is_current(self.sqlx_metadata_inputs.get(package_id), &current) {
                    self.sqlx_metadata_inputs
                        .insert(package_id.clone(), Some(current));
                    continue;
                }
                // Until a preparation succeeds, the metadata on disk is unknown.
                self.sqlx_metadata_inputs.insert(package_id.clone(), None);
                wamn_release_control::delivery::sqlx::prepare(
                    &package.root,
                    self.config.target_database_url(),
                )
                .await
                .map_err(|source| {
                    ProductionDevStageError::owner("refresh selected SQLx metadata", source)
                })?;
                self.sqlx_metadata_inputs
                    .insert(package_id.clone(), Some(current));
            }
        }
        Ok(())
    }

    async fn build(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Build);
        self.packages
            .as_mut()
            .expect("Generate resolved the package closure")
            .read_compiled_manifests()
            .map_err(|source| {
                ProductionDevStageError::owner("read generated package manifests", source.into())
            })?;
        let packages = self.package_inputs()?;
        self.pending_builds.extend(
            package_builds(&packages, wamn_compiler::build::current_build)
                .into_iter()
                .filter_map(|(id, build)| build.is_none().then_some(id)),
        );
        // Build each changed package, bases first, including dependent overlays.
        for package in packages {
            if !self.pending_builds.contains(&package.manifest.package.id) {
                continue;
            }
            let output_root = wamn_compiler::build::default_output_root(&package.root);
            let mut request = wamn_compiler::build::BuildRequest::new(
                &package.root,
                &output_root,
                self.config.target_database_url(),
            );
            request.allow_stale_base_pins = true;
            wamn_compiler::build::build_components(&request, &package.manifest.package.id)
                .await
                .map_err(|source| ProductionDevStageError::owner("build components", source))?;
            if package.root.join("web/package.json").is_file() {
                wamn_package_control::web_artifact::build_web_artifact(&package.root)
                    .await
                    .map_err(|source| {
                        ProductionDevStageError::owner("build web artifact", source.into())
                    })?;
            }
            self.pending_builds.remove(&package.manifest.package.id);
        }
        Ok(())
    }

    fn virtualize(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Virtualize);
        // The Build stage virtualized each component; this stage selects the
        // components that each package's build.json records.
        self.artifacts = select_component_artifacts(&self.package_inputs()?)?;
        self.palette_artifacts = select_palette_artifacts(&self.package_inputs()?)?;

        let packages = self
            .packages
            .as_ref()
            .expect("package_inputs validated state");
        for base in packages.base_packages() {
            let artifact = self
                .artifacts
                .iter()
                .find(|artifact| {
                    artifact.package_id.as_ref() == base.manifest().package.id.as_str()
                })
                .expect("selected artifacts contain every resolved package");
            let verified = base.component_digest().verify(artifact.digest.clone());
            self.verified_base_digests.push(verified);
        }
        Ok(())
    }

    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "Admit is one stage of the `DevStageRunner` seam, and every stage the \
                  dispatcher awaits has the same shape; this one reaches no I/O today"
    )]
    async fn admit(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Admit);
        if self.artifacts.is_empty() {
            return Err(ProductionDevStageError::invalid(
                "admit components",
                "the Virtualize stage selected no package components",
            ));
        }
        for artifact in self.artifacts.clone() {
            let package = self.package_input(&artifact.package_id)?;
            let template = package
                .root
                .join(PACKAGE_COMPONENTS)
                .join(format!("{}.json.in", artifact.component));
            // ONE authored site (wamn-10yt.50): the manifest pin. The run then
            // layers the digest it actually BUILT over it (wamn-10yt.48).
            let mut base_digests =
                authored_base_digests(&package.root).map_err(base_digests_stage_error)?;
            base_digests.extend(self.built_base_digests());
            let declaration = render_component_declaration(
                &template,
                &self.config.activation_identity().project_env_key(),
            )?;
            let admission = admit_component(AdmitComponentRequest {
                package: package.root,
                component_bytes: artifact.path.clone(),
                declaration: declaration.path().to_owned(),
                admitted_platform_packages: vec![
                    NODE_CAPABILITY.to_owned(),
                    POSTGRES_CAPABILITY.to_owned(),
                ],
                base_digests: base_digests.values().map(ToString::to_string).collect(),
            })
            .map_err(|source| {
                ProductionDevStageError::owner("admit exact component bytes", source)
            })?;
            require_admitted_identity(&admission, &artifact)?;
            self.admissions.push(admission);
        }
        // Each palette component a wiring names, admitted into the target of
        // its package with the packages its own declaration states, as the
        // cluster path pushes it (wamn-hw3n).
        for palette in self.palette_artifacts.clone() {
            let artifact = palette.artifact;
            let package = self.package_input(&artifact.package_id)?;
            let target = wamn_catalog::ComponentPackageCoordinate {
                project_env_key: self.config.activation_identity().project_env_key(),
                package_id: artifact.package_id.to_string(),
                package_version: artifact.package_version.to_string(),
            };
            let store_alias = selected_store_alias(&self.config, &artifact)
                .map_err(|source| ProductionDevStageError::owner("read store alias", source))?;
            let components = wamn_package_control::package_artifact::wiring_components(
                &package.root,
            )
            .map_err(|source| ProductionDevStageError::owner("read connection aliases", source))?;
            require_wiring_store_alias(&components, &artifact, store_alias.as_deref())?;
            let document =
                render_palette_declaration(&palette.declaration, &target, store_alias.as_deref())
                    .map_err(declaration_stage_error)?;
            let admitted_platform_packages =
                declared_platform_packages(&palette.declaration, &document)
                    .map_err(declaration_stage_error)?;
            let rendered = serde_json::to_vec(&document).map_err(|source| {
                ProductionDevStageError::owner("serialize palette declaration", source.into())
            })?;
            let declaration = TemporaryFile::write(&rendered).map_err(|source| {
                ProductionDevStageError::owner("write rendered palette declaration", source)
            })?;
            let admission = admit_component(AdmitComponentRequest {
                package: package.root,
                component_bytes: artifact.path.clone(),
                declaration: declaration.path().to_owned(),
                admitted_platform_packages,
                base_digests: BTreeSet::new(),
            })
            .map_err(|source| {
                ProductionDevStageError::owner("admit exact palette component bytes", source)
            })?;
            require_admitted_identity(&admission, &artifact)?;
            self.admissions.push(admission);
        }
        Ok(())
    }

    fn condition(&mut self) -> impl Future<Output = Result<(), ProductionDevStageError>> {
        self.clear_after(DevStage::Condition);
        if self.admissions.len() != self.artifacts.len() + self.palette_artifacts.len() {
            return std::future::ready(Err(ProductionDevStageError::invalid(
                "condition package admissions",
                "every package and palette component must carry one exact admission before Condition",
            )));
        }
        std::future::ready(Ok(()))
    }

    async fn acl(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Acl);
        let packages = self
            .package_inputs()?
            .into_iter()
            .map(|package| package.root)
            .collect::<Vec<_>>();
        let inputs = build_inputs(&self.package_inputs()?)?;
        let prepared = PreparedLocalGrants {
            target: target_database::prepare_configuration(&self.config).map_err(|source| {
                ProductionDevStageError::owner("prepare local target privileges", source)
            })?,
            data_access: reconcile_package_data_access::prepare_local(&packages).map_err(
                |source| {
                    ProductionDevStageError::owner("prepare generated package data access", source)
                },
            )?,
            inputs,
        };
        self.reconcile_local_grants(&prepared, false).await?;
        if prepared.inputs != build_inputs(&self.package_inputs()?)? {
            return Err(ProductionDevStageError::invalid(
                "validate local grant inputs",
                "source inputs changed during grant validation; retry the candidate",
            ));
        }
        self.local_grants = Some(prepared);
        Ok(())
    }

    async fn reconcile_local_grants(
        &self,
        prepared: &PreparedLocalGrants,
        apply: bool,
    ) -> Result<(), ProductionDevStageError> {
        self.target_lease
            .as_ref()
            .expect("local target lease is held")
            .reconcile_configuration(&prepared.target, apply)
            .await
            .map_err(|source| {
                ProductionDevStageError::owner("reconcile local target privileges", source)
            })?;
        reconcile_package_data_access::reconcile_local(
            &prepared.data_access,
            self.config.target_database_url(),
            &self.config.activation_identity().project_env_key(),
            &self.config.activation_identity().environment,
            apply,
        )
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("reconcile generated package data access", source)
        })?;
        // A recreated local target loses its replica identities.
        let packages = self
            .package_inputs()?
            .into_iter()
            .map(|package| package.root)
            .collect::<Vec<_>>();
        reconcile_local_replica_identity(self.config.target_database_url(), &packages, apply)
            .await
            .map_err(|source| {
                ProductionDevStageError::owner("reconcile package replican identity", source)
            })?;
        Ok(())
    }

    async fn release(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Release);
        self.reauthenticate_publisher().await?;
        let packages = self.package_inputs()?;
        let package_coordinates = packages
            .iter()
            .map(|package| {
                PackageCoordinate::new(
                    &package.manifest.package.id,
                    &package.manifest.package.version,
                )
                .map_err(|source| {
                    ProductionDevStageError::owner(
                        "construct release package coordinate",
                        source.into(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let package_roots = packages
            .iter()
            .map(|package| package.root.clone())
            .collect::<Vec<_>>();
        let identity = self.config.activation_identity();
        let request = PublishReleaseRequest {
            applications: self.environment.membership(),
            application: None,
            org: identity.org.clone(),
            project: identity.project.clone(),
            environment: identity.environment.clone(),
            packages: package_coordinates,
        };
        let local = self.config.local_artifacts();
        let (mut published, mut local_facts) = publish_release::publish_local(
            request,
            publish_release::LocalReleaseTarget {
                database_url: self.config.target_database_url(),
                control_database_url: self.config.system_database_url(),
                run_schema: RUN_SCHEMA,
            },
            &package_roots,
            &self.admissions,
        )
        .await
        .map_err(|source| ProductionDevStageError::owner("assemble local application", source))?;
        fs::create_dir_all(&local.directory).map_err(|source| {
            ProductionDevStageError::owner("create local artifact directory", source.into())
        })?;
        let applications = self.environment.membership();
        let entry_changed = publish_release::refresh_composed_entry(
            &mut self.composed_entry,
            &published,
            &self
                .admissions
                .iter()
                .map(|admission| (admission.facts(), admission.bytes()))
                .collect::<Vec<_>>(),
            &fs::read(&local.flow_http_component).map_err(|source| {
                ProductionDevStageError::owner("read pinned route component", source.into())
            })?,
            &applications,
        )
        .map_err(|source| ProductionDevStageError::owner("compose released HTTP entry", source))?;
        let entry = self
            .composed_entry
            .as_ref()
            .expect("the entry was composed");
        fs::create_dir_all(&local.directory).map_err(|source| {
            ProductionDevStageError::owner("create local artifact directory", source.into())
        })?;
        let entry_path = wamn_engine::artifact_source::local_component_path(
            &local.directory.canonicalize().map_err(|source| {
                ProductionDevStageError::owner("name local artifacts", source.into())
            })?,
            entry.digest(),
        )
        .map_err(|source| ProductionDevStageError::owner("name local entry", source.into()))?;
        publish_release::record_composed_entry(
            &mut published,
            entry,
            &format!("file:{}", entry_path.display()),
        )
        .map_err(|source| ProductionDevStageError::owner("record local entry", source))?;
        // The entry record changes the manifest, so the facts name its new digest.
        local_facts.manifest_digest = published.digest.clone();
        if entry_changed || !entry_path.is_file() {
            fs::write(entry_path, entry.bytes()).map_err(|source| {
                ProductionDevStageError::owner("stage composed entry", source.into())
            })?;
        }
        // A run admitted under the candidate pins its digest, and the run-plane
        // foreign key needs the release row, which apply's stage writes.
        wamn_environment_control::environment::stage::write_release(
            self.config.target_database_url(),
            &identity.project_env_key(),
            published.digest.as_str(),
            &published.manifest,
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .await
        .map_err(|source| ProductionDevStageError::owner("install local release", source))?;
        let defaults = wamn_environment_control::environment::stage::sequence_defaults(
            &applications,
            self.package_inputs()?
                .iter()
                .map(|package| &package.manifest),
        )
        .map_err(|source| ProductionDevStageError::owner("read local sequence defaults", source))?;
        wamn_environment_control::environment::stage::create_number_sequences(
            self.config.target_database_url(),
            &defaults,
        )
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("create local number sequences", source)
        })?;
        let packages = self
            .package_inputs()?
            .iter()
            .map(|package| package.manifest.clone())
            .collect::<Vec<_>>();
        let binding_inputs = prepare_local_bindings(
            &self.config,
            &self.admissions,
            &self.environment,
            &packages,
            published.digest.as_str(),
        )
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("prepare local connection selections", source)
        })?;
        local_facts.bindings = resolve_local_bindings(
            &self.config,
            &self.environment,
            &packages,
            &binding_inputs,
            published.digest.as_str(),
            None,
        )
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("validate local connection selections", source)
        })?;
        wamn_runtime::local_application::validate_local_facts(
            &local_facts,
            &published.manifest,
            &identity.project_env_key(),
        )
        .map_err(|source| {
            ProductionDevStageError::owner("validate complete local application", source)
        })?;
        fs::create_dir_all(&local.directory).map_err(|source| {
            ProductionDevStageError::owner("create local artifact directory", source.into())
        })?;
        for artifact in self.artifacts.iter().chain(
            self.palette_artifacts
                .iter()
                .map(|palette| &palette.artifact),
        ) {
            let bytes = fs::read(&artifact.path).map_err(|source| {
                ProductionDevStageError::owner("read local component", source.into())
            })?;
            if wamn_digest::bytes_sha256(&bytes) != artifact.digest.as_ref() {
                return Err(ProductionDevStageError::invalid(
                    "stage local component",
                    "admitted component bytes changed",
                ));
            }
            let path = wamn_engine::artifact_source::local_component_path(
                &local.directory,
                &artifact.digest,
            )
            .map_err(|source| {
                ProductionDevStageError::owner("name local component", source.into())
            })?;
            fs::write(path, bytes).map_err(|source| {
                ProductionDevStageError::owner("stage local component", source.into())
            })?;
        }
        fs::write(
            local
                .directory
                .join(wamn_catalog::RELEASE_MANIFEST_FILE_NAME),
            &published.canonical_bytes,
        )
        .map_err(|source| {
            ProductionDevStageError::owner("stage local application manifest", source.into())
        })?;
        let admission_bytes = serde_json::to_vec(&local_facts).map_err(|source| {
            ProductionDevStageError::owner("encode local admissions", source.into())
        })?;
        fs::write(
            local
                .directory
                .join(wamn_runtime::local_application::LOCAL_FACTS_FILE),
            admission_bytes,
        )
        .map_err(|source| {
            ProductionDevStageError::owner("stage local admissions", source.into())
        })?;
        let serving = wamn_runtime::local_application::LocalRelease {
            manifest_digest: published.digest.clone(),
            applications,
        };
        let bytes = serde_json::to_vec(&serving).map_err(|source| {
            ProductionDevStageError::owner("encode local release facts", source.into())
        })?;
        fs::write(
            local
                .directory
                .join(wamn_runtime::local_application::LOCAL_RELEASE_FILE),
            bytes,
        )
        .map_err(|source| {
            ProductionDevStageError::owner("stage local release facts", source.into())
        })?;
        let carrier = ReleaseCarrier {
            artifact_base: format!("local:{}", local.directory.display()),
            manifest_digest: published.digest,
        };
        activation::prepare_local(&DevActivationRequest {
            config: &self.config,
            applications: &serving.applications,
            release: &carrier,
            identity: self.config.activation_identity(),
            host_binary: self.config.host_binary(),
            wasmtime_cache_dir: self.config.wasmtime_cache_dir(),
        })
        .map_err(|source| {
            ProductionDevStageError::owner(
                "validate local workload before replacement",
                source.into(),
            )
        })?;
        self.read_publisher
            .set_release(published.manifest, carrier.clone());
        self.release = Some(carrier);
        self.local_bindings = local_facts.bindings;
        self.local_binding_inputs = binding_inputs;
        Ok(())
    }

    async fn activate(&mut self) -> Result<(), ProductionDevStageError> {
        let packages = self
            .package_inputs()?
            .iter()
            .map(|package| package.manifest.clone())
            .collect::<Vec<_>>();
        let digest = self
            .release
            .as_ref()
            .ok_or_else(|| {
                ProductionDevStageError::invalid(
                    "read local release",
                    "the candidate has no release",
                )
            })?
            .manifest_digest
            .as_str()
            .to_owned();
        reserve_local_bindings(
            &self.config,
            &self.environment,
            &packages,
            &self.local_binding_inputs,
            &digest,
        )
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("reserve local connection bindings", source)
        })?;
        if self.activation.is_some() {
            self.shutdown().await?;
        }
        resolve_local_bindings(
            &self.config,
            &self.environment,
            &packages,
            &self.local_binding_inputs,
            &digest,
            Some(&self.local_bindings),
        )
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("apply exact local connection selections", source)
        })?;
        let grants = self.local_grants.as_ref().ok_or_else(|| {
            ProductionDevStageError::invalid(
                "apply local grants",
                "the candidate has no validated grant inputs",
            )
        })?;
        self.reconcile_local_grants(grants, true).await?;
        self.acl_inputs = Some(grants.inputs.clone());
        self.clear_after(DevStage::Activate);
        let release = self.release.as_ref().ok_or_else(|| {
            ProductionDevStageError::invalid(
                "activate local runtime",
                "the Release stage produced no carrier",
            )
        })?;
        let target_instance = self.target_instance.as_deref().ok_or_else(|| {
            ProductionDevStageError::invalid(
                "activate operator session",
                "the target creation is absent",
            )
        })?;
        let activation = activation::activate(DevActivationRequest {
            config: &self.config,
            applications: &self.environment.membership(),
            release,
            identity: self.config.activation_identity(),
            host_binary: self.config.host_binary(),
            wasmtime_cache_dir: self.config.wasmtime_cache_dir(),
        })
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("activate local host and flow-http", source.into())
        })?;
        let endpoint = DevRuntimeEndpoint::new(
            activation.http_base_url(),
            self.config.route_host(),
            target_instance,
        );
        let identity = self.config.activation_identity();
        let status_endpoint = serde_json::json!({
            "base_url": endpoint.base_url(), "route_host": endpoint.route_host(),
            "org": identity.org, "project": identity.project, "env": identity.environment,
            "digest": release.manifest_digest.as_str(), "host_id": activation.host_id(),
            "workloads": activation.workload_ids(),
        });
        self.activation = Some(activation);
        if let Some(reader) = self.config.cdc_reader() {
            // After the package migrations, as the cluster runs the verb after
            // apply-package, so the schema keeps its owner.
            super::cdc_reader::apply_cdc_sql(&self.config, reader)
                .await
                .map_err(|source| {
                    ProductionDevStageError::owner("enable CDC on the local target", source)
                })?;
            let log = self
                .config
                .local_artifacts()
                .directory
                .with_file_name("cdc-reader.log");
            self.cdc_reader = Some(
                super::cdc_reader::CdcReaderProcess::start(&self.config, reader, &log)
                    .await
                    .map_err(|source| {
                        ProductionDevStageError::owner("start the local CDC reader", source)
                    })?,
            );
        }
        wamn_environment_control::provision_project_env::write_secret_json(
            &self
                .config
                .local_artifacts()
                .directory
                .with_file_name("application-endpoint.json"),
            &status_endpoint,
        )
        .map_err(|source| {
            ProductionDevStageError::owner("publish the local web endpoint", source)
        })?;
        self.read_publisher.set_runtime_endpoint(endpoint);
        Ok(())
    }

    fn package_inputs(&self) -> Result<Vec<PackageInput>, ProductionDevStageError> {
        let packages = self.packages.as_ref().ok_or_else(|| {
            ProductionDevStageError::invalid(
                "read package closure",
                "the Migrate stage has not resolved the package closure",
            )
        })?;
        let mut inputs = packages
            .base_packages()
            .iter()
            .map(|package| PackageInput {
                root: package.root().to_owned(),
                manifest: package.manifest().clone(),
            })
            .collect::<Vec<_>>();
        inputs.push(PackageInput {
            root: packages.overlay_root().to_owned(),
            manifest: packages.overlay_manifest().clone(),
        });
        Ok(inputs)
    }

    fn package_input(&self, package_id: &str) -> Result<PackageInput, ProductionDevStageError> {
        self.package_inputs()?
            .into_iter()
            .find(|package| package.manifest.package.id == package_id)
            .ok_or_else(|| {
                ProductionDevStageError::invalid(
                    "resolve selected component package",
                    format!("selected artifact names unknown package {package_id}"),
                )
            })
    }

    async fn reauthenticate_publisher(&self) -> Result<(), ProductionDevStageError> {
        let endpoint = self.config.identity_database_endpoint().to_owned();
        let (client, connection) =
            tokio_postgres::connect(self.config.identity_database_url(), NoTls)
                .await
                .map_err(|source| {
                    ProductionDevStageError::authentication_unavailable(
                        endpoint.clone(),
                        source.into(),
                    )
                })?;
        let connection_task = tokio::spawn(connection);
        let authenticated =
            wamn_platform_identity::authenticate_pat(&client, self.config.condition_bearer_token())
                .await;
        drop(client);
        let authenticated = match authenticated {
            Ok(authenticated) => authenticated,
            Err(source) => {
                connection_task.abort();
                return Err(ProductionDevStageError::authentication_unavailable(
                    endpoint,
                    source.into(),
                ));
            }
        };
        connection_task
            .await
            .map_err(|source| {
                ProductionDevStageError::authentication_unavailable(endpoint.clone(), source.into())
            })?
            .map_err(|source| {
                ProductionDevStageError::authentication_unavailable(endpoint.clone(), source.into())
            })?;
        authenticated.ok_or_else(|| {
            ProductionDevStageError::owner_at(
                "re-authenticate publisher",
                endpoint,
                "the configured PAT was refused; supply a valid credential",
            )
        })?;
        Ok(())
    }

    fn clear_after(&mut self, stage: DevStage) {
        match stage {
            DevStage::Migrate | DevStage::Introspect => {
                self.catalogs.clear();
                self.artifacts.clear();
                self.palette_artifacts.clear();
                self.verified_base_digests.clear();
                self.admissions.clear();
                self.release = None;
            }
            DevStage::Generate | DevStage::Build | DevStage::Virtualize => {
                self.artifacts.clear();
                self.palette_artifacts.clear();
                self.verified_base_digests.clear();
                self.admissions.clear();
                self.release = None;
            }
            DevStage::Admit => {
                self.admissions.clear();
                self.release = None;
            }
            DevStage::Condition | DevStage::Acl | DevStage::Release => {
                self.release = None;
            }
            DevStage::Activate => {}
        }
    }
}

/// The generation digest of each package, in order, from the build record.
fn build_inputs(packages: &[PackageInput]) -> Result<Vec<String>, ProductionDevStageError> {
    packages
        .iter()
        .map(|package| {
            wamn_compiler::build_record::generation_digest(&package.root)
                .map_err(|source| ProductionDevStageError::owner("read the build inputs", source))
        })
        .collect()
}

/// The physical SQL fingerprint of each package for migration and target reuse.
fn schema_inputs(packages: &[PackageInput]) -> Result<Vec<String>, ProductionDevStageError> {
    packages
        .iter()
        .map(|package| {
            wamn_compiler::build_record::physical_schema_fingerprint(&wamn_compiler::output_root(
                &package.root,
            ))
            .map_err(|source| ProductionDevStageError::owner("read the schema inputs", source))
        })
        .collect()
}

/// Reuse generation digests and output completeness rules, bases first.
/// An unreadable build selects rebuilding, including each dependent overlay.
fn current_package_builds(
    packages: &[PackageInput],
) -> BTreeMap<String, Option<wamn_compiler::build_record::BuildRecord>> {
    package_builds(packages, wamn_compiler::build::current_generation)
}

fn package_builds(
    packages: &[PackageInput],
    mut current: impl FnMut(
        &Path,
        &Path,
    ) -> anyhow::Result<Option<wamn_compiler::build_record::BuildRecord>>,
) -> BTreeMap<String, Option<wamn_compiler::build_record::BuildRecord>> {
    let mut builds: BTreeMap<String, Option<wamn_compiler::build_record::BuildRecord>> =
        BTreeMap::new();
    for package in packages {
        let base_changed = package
            .manifest
            .base_dependencies
            .values()
            .any(|base| builds.get(&base.package).is_some_and(Option::is_none));
        let build = if base_changed {
            None
        } else {
            current(
                &package.root,
                &wamn_compiler::build::default_output_root(&package.root),
            )
            .ok()
            .flatten()
        };
        builds.insert(package.manifest.package.id.clone(), build);
    }
    builds
}

async fn generate_changed_packages<F, Fut>(
    packages: Vec<PackageInput>,
    current: &BTreeMap<String, Option<wamn_compiler::build_record::BuildRecord>>,
    mut generate: F,
) -> Result<BTreeMap<String, wamn_compiler::build_record::BuildRecord>, ProductionDevStageError>
where
    F: FnMut(PackageInput) -> Fut,
    Fut: std::future::Future<
            Output = Result<wamn_compiler::build_record::BuildRecord, ProductionDevStageError>,
        >,
{
    let mut built = BTreeMap::new();
    for package in packages {
        let id = package.manifest.package.id.clone();
        if current[&id].is_none() {
            built.insert(id, generate(package).await?);
        }
    }
    Ok(built)
}

/// What a verifier's SQLx metadata was prepared from: the generation digest of the
/// package's build.json and the digests of its base packages.
///
/// The verifier's statements and the schema they run against both come from
/// these inputs, so equal inputs give equal metadata. A component Rust edit
/// does not reach here, because Generate does not run for it.
#[derive(Debug, PartialEq, Eq)]
struct SqlxMetadataInputs {
    inputs: String,
    dependencies: Vec<wamn_compiler::build_record::BuildDependency>,
}

impl SqlxMetadataInputs {
    fn of(build_record: &wamn_compiler::build_record::BuildRecord) -> Self {
        Self {
            inputs: build_record.inputs.generation_digest.clone(),
            dependencies: build_record.dependencies.clone(),
        }
    }
}

/// Whether the verifier's SQLx metadata was prepared for `current`.
///
/// The last preparation in this session answers, and an unfinished one never
/// matches. A new session prepares once, because the metadata lies in build
/// output that no commit records.
fn sqlx_metadata_is_current(
    prepared: Option<&Option<SqlxMetadataInputs>>,
    current: &SqlxMetadataInputs,
) -> bool {
    prepared.is_some_and(|prepared| prepared.as_ref() == Some(current))
}

impl DevStageRunner for ProductionDevStageRunner {
    type Error = ProductionDevStageError;

    fn reset(&mut self, from: DevStage) {
        self.read_publisher.reset(from);
    }

    fn stage_started(&mut self, stage: DevStage) {
        self.read_publisher.stage_started(stage);
    }

    fn stage_completed(&mut self, stage: DevStage) {
        self.read_publisher.stage_completed(stage);
    }

    fn stage_skipped(&mut self, stage: DevStage) {
        self.read_publisher.stage_skipped(stage);
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the `DevStageRunner` seam declares `async fn`; this stage condition \
                  reads build.json synchronously"
    )]
    async fn stage_is_unchanged(&mut self, stage: DevStage) -> Result<bool, Self::Error> {
        match stage {
            DevStage::Migrate => {
                // Generation can change package registration without changing physical SQL.
                return Ok(self.pending_builds.is_empty()
                    && self.prepared_schema_inputs.is_some()
                    && self.prepared_schema_inputs == self.schema_inputs);
            }
            DevStage::Introspect => {
                return Ok(self.catalogs.len() == self.package_inputs()?.len());
            }
            DevStage::Acl => {
                return Ok(self.acl_inputs.is_some() && self.acl_inputs == self.build_inputs);
            }
            _ => {}
        }
        if stage != DevStage::Generate {
            return Ok(false);
        }
        Ok(self.pending_builds.is_empty()
            && current_package_builds(&self.package_inputs()?)
                .values()
                .all(Option::is_some))
    }

    fn stage_failed(&mut self, stage: DevStage, failure: DevStageFailure) {
        self.read_publisher.stage_failed(stage, failure);
    }

    fn classify_error(&self, error: &Self::Error) -> DevStageFailure {
        DevStageFailure::new(error.type_.as_str(), error.to_string(), None)
    }

    fn run_notices(&self) -> Vec<DevRunNotice> {
        // A durable publish from this same source WILL refuse until the pin is
        // reminted, so the run says it here rather than leaving it to be found
        // at promotion (wamn-10yt.48).
        self.base_digests_moved_off_pin()
            .map(|(coordinate, pin, built)| {
                DevRunNotice::new(
                    BASE_PIN_STALE_NOTICE,
                    format!("{coordinate} {PACKAGE_MANIFEST} names {pin}, built {built}"),
                )
            })
            .collect()
    }

    async fn prepare_run(&mut self) -> Result<(), Self::Error> {
        self.environment = super::session::read_environment(&self.environment_file)
            .map_err(|source| ProductionDevStageError::owner("read local environment", source))?;
        self.packages = Some(
            super::config::resolve_dev_packages(&self.config, &self.overlay_root).map_err(
                |source| {
                    ProductionDevStageError::owner("resolve local package closure", source.into())
                },
            )?,
        );
        let packages = self.package_inputs()?;
        let applications = self.environment.membership();
        let refusals = wamn_environment_control::environment::analyse::application_refusals(
            &applications,
            packages
                .iter()
                .map(|package| package.manifest.package.id.as_str()),
        );
        if !refusals.is_empty() {
            return Err(ProductionDevStageError::invalid(
                "read local applications",
                refusals.join("; "),
            ));
        }
        wamn_environment_control::environment::stage::sequence_defaults(
            &applications,
            packages.iter().map(|package| &package.manifest),
        )
        .map_err(|source| ProductionDevStageError::owner("read local sequence defaults", source))?;
        let inputs = build_inputs(&packages)?;
        if self.target_lease.is_none() {
            self.target_lease = Some(target_database::acquire(&self.config).await.map_err(
                |source| {
                    ProductionDevStageError::owner("acquire local session lease", source.into())
                },
            )?);
        }
        self.target_lease
            .as_ref()
            .expect("lease acquired")
            .require_current_lease()
            .await
            .map_err(|source| {
                ProductionDevStageError::owner("require local session lease", source.into())
            })?;
        // Build in disposable databases before comparing the selected physical SQL.
        self.prepare_generation().await?;
        let schema_inputs = schema_inputs(&packages)?;
        let roots = packages
            .iter()
            .map(|package| package.root.clone())
            .collect::<Vec<_>>();
        let diffs = target_database::schema_changes(&self.config, &self.environment, &roots)
            .await
            .map_err(|source| ProductionDevStageError::owner("plan local schema", source))?;
        for diff in &diffs {
            for (package, change) in &diff.packages {
                if change.installed.as_ref() != Some(&change.candidate) {
                    tracing::warn!(%package, mode = ?diff.mode, pending = ?change.pending,
                        edited = ?change.edited_applied,
                        "local schema differs; use wamn schema apply with the source directory and --env for explicit SQL");
                }
            }
        }
        // A configuration-only watch event is a reapply, not a schema-bearing deploy.
        let schema_deploy = self.schema_inputs.as_ref() != Some(&schema_inputs);
        let reset = schema_deploy
            && diffs
                .iter()
                .any(wamn_environment_control::environment::sql::schema::Diff::requires_reset);
        let matches_installed = diffs.iter().all(|diff| {
            diff.packages
                .values()
                .all(|package| package.installed.as_ref() == Some(&package.candidate))
        });
        let instance = self
            .target_lease
            .as_ref()
            .expect("lease acquired")
            .instance(&self.config)
            .await
            .map_err(|source| {
                ProductionDevStageError::owner("read the owned local target", source)
            })?;
        if reset {
            target_database::validate_schema_reset(&self.environment, &roots, &diffs).map_err(
                |source| ProductionDevStageError::owner("validate local reset inputs", source),
            )?;
            let reset_inputs = self
                .target_lease
                .as_ref()
                .expect("lease acquired")
                .prepare_reset(&self.config)
                .await
                .map_err(|source| {
                    ProductionDevStageError::owner("prepare local template reset", source.into())
                })?;
            // Compile the candidate before interrupting the last working application.
            self.build().await?;
            self.shutdown().await?;
            tracing::warn!(affected = ?diffs.iter().flat_map(|diff| &diff.affected_applications).collect::<BTreeSet<_>>(),
                "reset development database; all application data is discarded");
            let instance = self
                .target_lease
                .as_ref()
                .expect("lease acquired")
                .recreate_prepared(&self.config, &reset_inputs)
                .await
                .map_err(|source| {
                    ProductionDevStageError::owner("recreate local schema target", source.into())
                })?;
            self.target_instance = Some(instance);
            self.prepared_schema_inputs = None;
            self.catalogs.clear();
        } else {
            if self.target_instance.as_ref() != Some(&instance)
                || self.prepared_schema_inputs.as_ref() != Some(&schema_inputs)
                || !matches_installed
            {
                self.prepared_schema_inputs = None;
                self.catalogs.clear();
                if matches_installed
                    && let Some((_, recorded, catalogs)) = self
                        .target_lease
                        .as_ref()
                        .expect("lease acquired")
                        .retained_target(&self.config)
                        .await
                    && recorded == schema_inputs
                {
                    self.prepared_schema_inputs = Some(schema_inputs.clone());
                    self.catalogs = catalogs;
                }
            }
            self.target_instance = Some(instance);
        }
        self.build_inputs = Some(inputs);
        self.schema_inputs = Some(schema_inputs);
        Ok(())
    }

    async fn run(&mut self, stage: DevStage) -> Result<(), Self::Error> {
        match stage {
            DevStage::Migrate => self.migrate().await,
            DevStage::Introspect => self.introspect().await,
            DevStage::Generate => self.generate().await,
            DevStage::Build => self.build().await,
            DevStage::Virtualize => self.virtualize(),
            DevStage::Admit => self.admit().await,
            DevStage::Condition => self.condition().await,
            DevStage::Acl => self.acl().await,
            DevStage::Release => self.release().await,
            DevStage::Activate => self.activate().await,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct LocalBindingSelection {
    package_id: String,
    component: String,
    store_alias: String,
}

#[derive(Debug)]
struct PreparedLocalBinding {
    package_id: String,
    read_only: bool,
    requirement: wamn_catalog::ComponentConnectionRequirement,
    instance_id: String,
}

fn read_local_binding_selections(config: &DevConfig) -> anyhow::Result<Vec<LocalBindingSelection>> {
    Ok(config
        .local_artifacts()
        .bindings
        .as_ref()
        .map(|path| -> anyhow::Result<_> {
            serde_json::from_slice(&fs::read(path).context("read local connection selections")?)
                .context("parse strict local connection selections")
        })
        .transpose()?
        .unwrap_or_default())
}

async fn prepare_local_bindings(
    config: &DevConfig,
    admissions: &[ComponentAdmission],
    environment: &wamn_environment_control::environment::document::EnvironmentDocument,
    packages: &[wamn_compiler::manifest::PackageManifest],
    manifest_digest: &str,
) -> anyhow::Result<Vec<PreparedLocalBinding>> {
    let system = wamn_package_control::database::connect(config.system_database_url()).await?;
    system.batch_execute("SET ROLE wamn_db_owner").await?;
    let mut document = environment.clone();
    // DEP2-03 replaces the shared candidate with application publication.
    for application in document.applications.values_mut() {
        application.current =
            wamn_environment_control::environment::document::DeclaredRelease::Digest(
                manifest_digest.into(),
            );
    }
    let bindings =
        wamn_environment_control::environment::bindings::analyse(&system, &document, packages)
            .await?;
    let drops = wamn_environment_control::environment::bindings::project_authority(
        config.system_database_url(),
        config.target_database_url(),
        environment,
    )
    .await?;
    wamn_broker::Publisher::new(config.nats().connect().await?)
        .publish(drops)
        .await;

    let mut prepared = Vec::new();
    for admission in admissions {
        let application = environment
            .application_for_package(admission.package_id())
            .context("local component has no application")?;
        for requirement in admission.requirements() {
            let binding = bindings
                .iter()
                .find(|binding| {
                    binding.application == application && binding.alias == requirement.store_alias()
                })
                .with_context(|| {
                    format!(
                        "connection_unbound: {application}.{}",
                        requirement.store_alias()
                    )
                })?;
            let resolved = binding
                .resolved
                .as_ref()
                .context("local binding requires a host integration")?;
            anyhow::ensure!(
                &resolved.requirement_type.descriptor() == requirement.requirement(),
                "local instance type differs from the declared requirement"
            );
            prepared.push(PreparedLocalBinding {
                package_id: admission.package_id().to_owned(),
                read_only: binding.mode == wamn_compiler::manifest::ConnectionMode::Read,
                requirement: requirement.clone(),
                instance_id: binding.connection_id.clone(),
            });
        }
    }
    let mut project = wamn_package_control::database::connect(config.target_database_url()).await?;
    let tx = project.transaction().await?;
    for input in &prepared {
        let admission = admissions
            .iter()
            .find(|admission| {
                admission.package_id() == input.package_id
                    && admission.facts().component_digest == input.requirement.component_digest()
            })
            .context("local admitted requirement missing")?;
        let declaration = packages
            .iter()
            .find(|package| package.package.id == input.package_id)
            .and_then(|package| package.connections.get(input.requirement.store_alias()))
            .context("local connection declaration missing")?;
        let interface = wamn_package_control::connection_requirement::require_imports(
            admission.bytes(),
            input.requirement.store_alias(),
            &declaration.interface,
        )?;
        let mode = if input.read_only { "read" } else { "write" };
        tx.execute("INSERT INTO catalog.connection_requirement(project_env_key,component_digest,package_id,name,interface,mode) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING", &[&config.activation_identity().project_env_key(),&input.requirement.component_digest(),&input.package_id,&input.requirement.store_alias(),&interface,&mode]).await?;
    }
    tx.commit().await?;
    Ok(prepared)
}

async fn reserve_local_bindings(
    config: &DevConfig,
    environment: &wamn_environment_control::environment::document::EnvironmentDocument,
    packages: &[wamn_compiler::manifest::PackageManifest],
    prepared: &[PreparedLocalBinding],
    digest: &str,
) -> anyhow::Result<()> {
    let mut system = wamn_package_control::database::connect(config.system_database_url()).await?;
    let tx = system.transaction().await?;
    tx.batch_execute(&format!(
        "SET LOCAL ROLE wamn_db_owner; {}",
        wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ),
    ))
    .await?;
    let mut document = environment.clone();
    // DEP2-03 replaces the shared candidate with application publication.
    for application in document.applications.values_mut() {
        application.current =
            wamn_environment_control::environment::document::DeclaredRelease::Digest(digest.into());
    }
    let bindings =
        wamn_environment_control::environment::bindings::analyse(&tx, &document, packages).await?;
    for input in prepared {
        let application = environment
            .application_for_package(&input.package_id)
            .context("local component has no application")?;
        anyhow::ensure!(
            bindings.iter().any(|binding| {
                binding.application == application
                    && input.requirement.store_alias() == binding.alias
                    && input.instance_id == binding.connection_id
                    && input.read_only
                        == (binding.mode == wamn_compiler::manifest::ConnectionMode::Read)
            }),
            "local org connection authority changed after candidate validation"
        );
    }
    tx.commit().await?;
    Ok(())
}

async fn resolve_local_bindings(
    config: &DevConfig,
    environment: &wamn_environment_control::environment::document::EnvironmentDocument,
    packages: &[wamn_compiler::manifest::PackageManifest],
    prepared: &[PreparedLocalBinding],
    digest: &str,
    apply: Option<&[wamn_runtime::local_application::LocalBindingFacts]>,
) -> anyhow::Result<Vec<wamn_runtime::local_application::LocalBindingFacts>> {
    wamn_runtime::local_application::require_local_target(
        config.target_database_url(),
        &config.activation_identity().project_env_key(),
        &config.activation_identity().environment,
    )
    .await?;
    let (client, connection) = tokio_postgres::connect(config.target_database_url(), NoTls).await?;
    let driver = tokio::spawn(connection);
    let mut system = wamn_package_control::database::connect(config.system_database_url()).await?;
    let authority = system.transaction().await?;
    authority
        .batch_execute(&format!(
            "SET LOCAL ROLE wamn_db_owner; {}",
            wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ),
        ))
        .await?;
    let mut document = environment.clone();
    // DEP2-03 replaces the shared candidate with application publication.
    for application in document.applications.values_mut() {
        application.current =
            wamn_environment_control::environment::document::DeclaredRelease::Digest(digest.into());
    }
    let resolved =
        wamn_environment_control::environment::bindings::analyse(&authority, &document, packages)
            .await?;
    let result = async {
        client.batch_execute("BEGIN").await?;
        client
            .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ))
            .await?;
        client
            .query_one(
                "SELECT set_config('app.project_env_key', $1, true)",
                &[&config.activation_identity().project_env_key()],
            )
            .await?;
        let mut bindings = Vec::new();
        for input in prepared {
            let binding = resolved
                .iter()
                .find(|binding| {
                    binding.connection_id == input.instance_id
                        && binding.alias == input.requirement.store_alias()
                })
                .context("local connection authority changed")?;
            let definition = binding
                .resolved
                .as_ref()
                .context("local binding needs a host integration")?;
            let handle = if binding.integration_type == "gcs" {
                None
            } else {
                Some(format!(
                    "org:{}:{}@{}",
                    document.org,
                    definition
                        .org_connection_name
                        .as_deref()
                        .context("local integration name missing")?,
                    binding.generation
                ))
            };
            let selection = wamn_runtime::local_application::selection(
                &input.requirement,
                &binding.connection_id,
                binding.generation,
                &definition.definition,
                handle,
                binding.mode == wamn_compiler::manifest::ConnectionMode::Read,
            );
            bindings.push(wamn_runtime::local_application::LocalBindingFacts {
                package_id: input.package_id.clone(),
                requirement: input.requirement.clone(),
                selection,
            });
        }
        if let Some(expected) = apply {
            anyhow::ensure!(
                bindings == expected,
                "local live authority changed after candidate validation"
            );
        }
        client.batch_execute("ROLLBACK").await?;
        if apply.is_some() {
            let mut project =
                wamn_package_control::database::connect(config.target_database_url()).await?;
            let tx = project.transaction().await?;
            let mut drops = wamn_broker::connection_changes::ConnectionDrops::default();
            wamn_environment_control::environment::bindings::write(
                &tx,
                &config.activation_identity().project_env_key(),
                digest,
                &document,
                &resolved,
                &mut drops,
            )
            .await?;
            tx.commit().await?;
            wamn_broker::Publisher::new(config.nats().connect().await?)
                .publish(drops)
                .await;
        }
        Ok::<_, anyhow::Error>(bindings)
    }
    .await;
    drop(client);
    driver.abort();
    authority.rollback().await?;
    result
}

async fn reconcile_local_replica_identity(
    database_url: &str,
    packages: &[PathBuf],
    apply: bool,
) -> anyhow::Result<()> {
    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect to the local target for replican identity")?;
    let driver = tokio::spawn(connection);
    let result = async {
        for root in packages {
            let directory = apply_package::read_package_directory(root)?;
            let package = wamn_schema_control::plan_package_migrations(&directory, None, &[])
                .context("derive package model mapping")?;
            let package_id = package.coordinate.package_id();
            let plan = wamn_environment_control::reconcile_replica_identity::reconcile(
                &client,
                &package.models,
                apply,
            )
            .await?;
            tracing::info!(package = %package_id, flips = plan.flips.len(), apply, "reconciled package replican identity");
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    drop(client);
    driver.abort();
    result
}

fn render_component_declaration(
    template: &Path,
    project_env_key: &str,
) -> Result<TemporaryFile, ProductionDevStageError> {
    let document =
        render_declaration_document(template, project_env_key).map_err(declaration_stage_error)?;
    let rendered = serde_json::to_vec(&document).map_err(|source| {
        ProductionDevStageError::owner("serialize component declaration", source.into())
    })?;
    TemporaryFile::write(&rendered).map_err(|source| {
        ProductionDevStageError::owner("write rendered component declaration", source)
    })
}

#[derive(Debug)]
pub(super) struct TemporaryFile(PathBuf);

impl TemporaryFile {
    pub(super) fn write(bytes: &[u8]) -> anyhow::Result<Self> {
        let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("wamn-dev-{}-{sequence}.json", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("create {}", path.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("write {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", path.display()))?;
        Ok(Self(path))
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// The store alias that the binding selections give a component: the one name
/// the operator passes as the environment document in the cluster.
fn selected_store_alias(
    config: &DevConfig,
    artifact: &SelectedComponentArtifact,
) -> anyhow::Result<Option<String>> {
    let selections = read_local_binding_selections(config)?;
    plan::selected_store_alias(&selections, artifact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_schema_introspection::ir::{
        Column, ColumnDefault, Constraint, Exclusion, ExclusionAccessMethod, ExclusionElement,
        ExclusionKey, Table,
    };

    fn package(id: &str, version: &str, component: &str) -> PackageInput {
        let manifest = serde_json::from_value(serde_json::json!({
            "package": {"id": id, "version": version},
            "required_platform_policy_contract": {"id": "fixture_data_access", "state": "satisfied"},
            "models": {},
            "connections": {},
            "components": {(component): {"connections": ["postgres"]}}
        }))
        .expect("fixture package manifest");
        PackageInput {
            root: PathBuf::from(format!("/packages/{id}")),
            manifest,
        }
    }

    struct PackageBuildFixture {
        directory: PathBuf,
        packages: Vec<PackageInput>,
    }

    impl PackageBuildFixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "wamn-dev-package-builds-{}-{}",
                std::process::id(),
                TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let mut packages = ["base", "other", "overlay"].map(|id| {
                let mut package = package(id, "1.0.0", "application");
                package.root = directory.join(id);
                package
            });
            packages[2].manifest.base_dependencies.insert(
                "base".to_owned(),
                wamn_compiler::manifest::BaseDependencyRequirement {
                    package: "base".to_owned(),
                    version: "1.0.0".to_owned(),
                    digest: format!("sha256:{}", "a".repeat(64)),
                    operations: Vec::new(),
                },
            );
            for package in &packages {
                fs::create_dir_all(&package.root).unwrap();
                fs::write(
                    package.root.join("wamn.json"),
                    serde_json::to_vec(&package.manifest).unwrap(),
                )
                .unwrap();
                fs::write(package.root.join("input.txt"), "original").unwrap();
                fs::create_dir_all(package.root.join("component/src")).unwrap();
                fs::write(
                    package.root.join("component/src/lib.rs"),
                    "pub fn original() {}",
                )
                .unwrap();
                write_package_build(package);
            }
            Self {
                directory,
                packages: packages.into(),
            }
        }
    }

    impl Drop for PackageBuildFixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    fn write_package_build(package: &PackageInput) -> wamn_compiler::build_record::BuildRecord {
        use wamn_compiler::build_record;

        let output = wamn_compiler::output_root(&package.root);
        fs::create_dir_all(&output).unwrap();
        let manifest = fs::read(package.root.join("wamn.json")).unwrap();
        let inputs = build_record::input_digest(&package.root).unwrap();
        fs::write(output.join("wamn.json"), &manifest).unwrap();
        fs::write(output.join("generated.txt"), &inputs).unwrap();
        let dependencies = package
            .manifest
            .base_dependencies
            .values()
            .map(|base| {
                let built =
                    build_record::read(&output.parent().unwrap().join(&base.package)).unwrap();
                build_record::BuildDependency {
                    id: base.package.clone(),
                    version: base.version.clone(),
                    digest: built.outputs.manifest,
                }
            })
            .collect::<Vec<_>>();
        let built: build_record::BuildRecord = serde_json::from_value(serde_json::json!({
            "schema": build_record::BUILD_RECORD_SCHEMA,
            "package": {"id": package.manifest.package.id, "version": "1.0.0"},
            "inputs": {"digest": inputs, "generation_digest": build_record::generation_digest(&package.root).unwrap(), "files": {}},
            "dependencies": dependencies,
            "generator": {"id": "generator", "toolchain": "rust", "sqlx": "0.9.0"},
            "outputs": {
                "manifest": wamn_digest::bytes_sha256(&manifest),
                "files": {"wamn.json": "manifest", "generated.txt": "generated"},
                "components": []
            },
        }))
        .unwrap();
        fs::write(
            output.join(build_record::BUILD_RECORD_FILE),
            built.to_bytes(),
        )
        .unwrap();
        built
    }

    fn package_output_snapshot(
        package: &PackageInput,
    ) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
        fs::read_dir(wamn_compiler::output_root(&package.root))
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (
                    path.clone(),
                    (
                        fs::read(&path).unwrap(),
                        fs::metadata(path).unwrap().modified().unwrap(),
                    ),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn a_save_preserves_other_package_build_records_and_outputs() {
        let fixture = PackageBuildFixture::new();
        let before = [
            package_output_snapshot(&fixture.packages[0]),
            package_output_snapshot(&fixture.packages[2]),
        ];
        let other_before = package_output_snapshot(&fixture.packages[1]);
        fs::write(fixture.packages[1].root.join("input.txt"), "saved").unwrap();
        let current = current_package_builds(&fixture.packages);
        let built =
            generate_changed_packages(fixture.packages.clone(), &current, |package| async move {
                Ok(write_package_build(&package))
            })
            .await
            .unwrap();
        assert_eq!(
            built.keys().map(String::as_str).collect::<Vec<_>>(),
            ["other"]
        );
        assert_eq!(
            before,
            [
                package_output_snapshot(&fixture.packages[0]),
                package_output_snapshot(&fixture.packages[2])
            ]
        );
        assert_ne!(other_before, package_output_snapshot(&fixture.packages[1]));
    }

    #[tokio::test]
    async fn source_edits_skip_generation_and_build_only_changed_packages_and_overlays() {
        use wamn_compiler::{build, build_record};

        for changed in ["other", "base"] {
            let fixture = PackageBuildFixture::new();
            let before = fixture
                .packages
                .iter()
                .map(package_output_snapshot)
                .collect::<Vec<_>>();
            let generation_inputs = build_inputs(&fixture.packages).unwrap();
            let package = fixture
                .packages
                .iter()
                .find(|package| package.manifest.package.id == changed)
                .unwrap();
            fs::write(
                package.root.join("component/src/lib.rs"),
                "pub fn changed() {}",
            )
            .unwrap();
            assert!(build_record::require_current(&package.root).is_err());
            assert_eq!(generation_inputs, build_inputs(&fixture.packages).unwrap());
            let current = current_package_builds(&fixture.packages);
            assert!(current.values().all(Option::is_some));
            let generated =
                generate_changed_packages(fixture.packages.clone(), &current, |_| async {
                    panic!("source edits must keep generated output")
                })
                .await
                .unwrap();
            assert!(generated.is_empty());
            let pending = package_builds(&fixture.packages, build::current_build);
            let selected = pending
                .iter()
                .filter_map(|(id, build)| build.is_none().then_some(id.as_str()))
                .collect::<Vec<_>>();
            assert_eq!(
                selected,
                if changed == "base" {
                    vec!["base", "overlay"]
                } else {
                    vec!["other"]
                }
            );
            for package in &fixture.packages {
                if pending[&package.manifest.package.id].is_none() {
                    build::build_components(
                        &build::BuildRequest::new(
                            &package.root,
                            &build::default_output_root(&package.root),
                            "",
                        ),
                        &package.manifest.package.id,
                    )
                    .await
                    .unwrap();
                    build_record::require_current(&package.root).unwrap();
                }
            }
            for (index, package) in fixture.packages.iter().enumerate() {
                if pending[&package.manifest.package.id].is_some() {
                    assert_eq!(before[index], package_output_snapshot(package));
                }
            }
            let rebuilt = build_record::read_current(
                &package.root,
                &wamn_compiler::output_root(&package.root),
            )
            .unwrap();
            assert_eq!(
                rebuilt.inputs.digest,
                build_record::input_digest(&package.root).unwrap()
            );
            assert!(
                package_builds(&fixture.packages, build::current_build)
                    .values()
                    .all(Option::is_some)
            );
        }
    }

    #[tokio::test]
    async fn unchanged_packages_do_no_generation_or_output_writes() {
        let fixture = PackageBuildFixture::new();
        for package in &fixture.packages {
            for path in package_output_snapshot(package).keys() {
                fs::File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
                    .unwrap();
            }
        }
        let before = fixture
            .packages
            .iter()
            .map(package_output_snapshot)
            .collect::<Vec<_>>();
        let current = current_package_builds(&fixture.packages);
        let built = generate_changed_packages(fixture.packages.clone(), &current, |_| async {
            panic!("unchanged packages must not generate")
        })
        .await
        .unwrap();
        assert!(built.is_empty());
        assert_eq!(
            before,
            fixture
                .packages
                .iter()
                .map(package_output_snapshot)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn base_edits_rebuild_overlays_and_overlay_edits_keep_bases() {
        let fixture = PackageBuildFixture::new();
        fs::write(fixture.packages[2].root.join("input.txt"), "overlay saved").unwrap();
        let current = current_package_builds(&fixture.packages);
        let built =
            generate_changed_packages(fixture.packages.clone(), &current, |package| async move {
                Ok(write_package_build(&package))
            })
            .await
            .unwrap();
        assert_eq!(
            built.keys().map(String::as_str).collect::<Vec<_>>(),
            ["overlay"]
        );
        fs::write(fixture.packages[0].root.join("input.txt"), "base saved").unwrap();
        let current = current_package_builds(&fixture.packages);
        let built =
            generate_changed_packages(fixture.packages.clone(), &current, |package| async move {
                Ok(write_package_build(&package))
            })
            .await
            .unwrap();
        assert_eq!(
            built.keys().map(String::as_str).collect::<Vec<_>>(),
            ["base", "overlay"]
        );
    }

    #[tokio::test]
    async fn stale_absent_and_invalid_build_records_select_rebuilding() {
        let fixture = PackageBuildFixture::new();
        let package = &fixture.packages[1];
        let path = wamn_compiler::output_root(&package.root).join("build.json");
        for state in ["stale", "absent", "invalid"] {
            match state {
                "stale" => fs::write(package.root.join("input.txt"), "changed").unwrap(),
                "absent" => fs::remove_file(&path).unwrap(),
                _ => fs::write(&path, "invalid JSON").unwrap(),
            }
            let current = current_package_builds(&fixture.packages);
            let built = generate_changed_packages(
                fixture.packages.clone(),
                &current,
                |package| async move { Ok(write_package_build(&package)) },
            )
            .await
            .unwrap();
            assert_eq!(
                built.keys().map(String::as_str).collect::<Vec<_>>(),
                ["other"],
                "{state}"
            );
            assert!(
                current_package_builds(&fixture.packages)
                    .values()
                    .all(Option::is_some)
            );
        }
    }

    /// A failed stage carries the refusal that stopped it, under its context.
    ///
    /// The live run of wamn-ri4b printed "dev-stage-owner-failed while
    /// introspect package: introspect package schemas" and dropped the
    /// unsupported-acl refusal underneath it. Both the watch output and the
    /// read model take this one string, so recovering that cause cost two
    /// fixture runs (wamn-aij8).
    #[test]
    fn a_failed_stage_names_its_whole_cause_chain_after_the_top_context() {
        let source = anyhow::Error::msg("schema has an explicit ACL")
            .context("PostgreSQL introspection refused (unsupported-acl) in schema inventory")
            .context("introspect package schemas");

        let error = ProductionDevStageError::owner("introspect package", source);

        assert_eq!(
            error.to_string(),
            "dev-stage-owner-failed while introspect package: introspect package schemas: \
             PostgreSQL introspection refused (unsupported-acl) in schema inventory: \
             schema has an explicit ACL"
        );
        assert_eq!(
            ProductionDevStageError::owner("introspect package", anyhow::Error::msg("one line"))
                .to_string(),
            "dev-stage-owner-failed while introspect package: one line",
            "a cause with no context keeps the line it always printed"
        );
    }

    #[test]
    fn sqlx_preparation_is_keyed_on_the_generation_digest() {
        let build_record = |inputs: &str, base: &str| {
            let build_record: wamn_compiler::build_record::BuildRecord =
                serde_json::from_value(serde_json::json!({
                    "schema": wamn_compiler::build_record::BUILD_RECORD_SCHEMA,
                    "package": {"id": "demo", "version": "1.0.0"},
                    "inputs": {"digest": "full", "generation_digest": inputs, "files": {}},
                    "dependencies": [{"id": "base", "version": "1.0.0", "digest": base}],
                    "generator": {"id": "generator", "toolchain": "rust", "sqlx": "0.9.0"},
                    "outputs": {"manifest": "m", "files": {}, "components": []},
                }))
                .unwrap();
            let expected = SqlxMetadataInputs::of(&build_record);
            let mut source_edit = build_record;
            source_edit.inputs.digest.push_str("-source-edit");
            assert_eq!(expected, SqlxMetadataInputs::of(&source_edit));
            expected
        };
        let prepared = build_record("sha256:a", "sha256:base");
        assert!(
            !sqlx_metadata_is_current(None, &prepared),
            "a new session prepares once"
        );
        assert!(
            sqlx_metadata_is_current(
                Some(&Some(prepared)),
                &build_record("sha256:a", "sha256:base")
            ),
            "an unchanged build input digest keeps the prepared SQLx metadata"
        );
        let prepared = build_record("sha256:a", "sha256:base");
        assert!(
            !sqlx_metadata_is_current(
                Some(&Some(prepared)),
                &build_record("sha256:b", "sha256:base")
            ),
            "a changed build input digest prepares"
        );
        let prepared = build_record("sha256:a", "sha256:base");
        assert!(
            !sqlx_metadata_is_current(
                Some(&Some(prepared)),
                &build_record("sha256:a", "sha256:other")
            ),
            "a changed base package prepares"
        );
        assert!(
            !sqlx_metadata_is_current(Some(&None), &build_record("sha256:a", "sha256:base")),
            "an unfinished preparation runs again"
        );
    }

    #[test]
    fn package_catalog_projection_keeps_base_contract_additive() {
        let base = PackageInput {
            root: PathBuf::from("/fixtures/catalog_base"),
            manifest: PackageManifest::from_slice(include_bytes!(
                "../../../environment/tests/support/catalog_base/wamn.json"
            ))
            .expect("parse base fixture manifest"),
        };
        let mut overlay = PackageInput {
            root: PathBuf::from("/fixtures/catalog_overlay"),
            manifest: PackageManifest::from_slice(include_bytes!(
                "../../../environment/tests/support/catalog_overlay/wamn.json"
            ))
            .expect("parse overlay fixture manifest"),
        };
        overlay
            .manifest
            .models
            .get_mut("record")
            .expect("overlay extends the base record")
            .constraint_owners
            .insert(
                "record_review_state_excl".to_owned(),
                "platform_catalog_overlay".to_owned(),
            );
        let base_exclusion = Exclusion::new(
            "record_maker_id_excl",
            ExclusionAccessMethod::Gist,
            vec![ExclusionKey::new(ExclusionElement::column("maker_id"), "=")],
            ["maker_id"],
        )
        .expect("base exclusion");
        let overlay_exclusion = Exclusion::new(
            "record_review_state_excl",
            ExclusionAccessMethod::Gist,
            vec![ExclusionKey::new(
                ExclusionElement::column("review_state"),
                "=",
            )],
            ["review_state"],
        )
        .expect("overlay exclusion");
        let catalog = CatalogIr::new(vec![
            Table::new(
                "platform_fixture",
                "record",
                vec![
                    Column::new("id", SqlValueType::Uuid, false, None, None),
                    Column::new("maker_id", SqlValueType::Uuid, false, None, None),
                    Column::new("created_at", SqlValueType::Timestamptz, false, None, None),
                    Column::new("created_by", SqlValueType::Uuid, false, None, None),
                    Column::new("updated_at", SqlValueType::Timestamptz, false, None, None),
                    Column::new("updated_by", SqlValueType::Uuid, false, None, None),
                    Column::new(
                        "review_required",
                        SqlValueType::Boolean,
                        false,
                        Some(ColumnDefault::boolean(false)),
                        None,
                    ),
                    Column::new(
                        "review_state",
                        SqlValueType::Text,
                        false,
                        Some(ColumnDefault::text("not_required")),
                        None,
                    ),
                ],
                vec![
                    Constraint::primary_key("record_id_pkey", ["id"])
                        .expect("base primary key"),
                    Constraint::check(
                        "record_review_state_check",
                        "review_state = ANY (ARRAY['not_required'::text, 'pending'::text, 'approved'::text])",
                    )
                    .expect("overlay quality constraint"),
                ],
                Vec::new(),
            )
            .with_exclusions(vec![base_exclusion.clone(), overlay_exclusion]),
            Table::new(
                "platform_fixture",
                "inspection",
                vec![Column::new(
                    "record_id",
                    SqlValueType::Uuid,
                    false,
                    None,
                    None,
                )],
                vec![
                    Constraint::primary_key("inspection_record_id_pkey", ["record_id"])
                        .expect("overlay primary key"),
                ],
                Vec::new(),
            ),
        ]);
        let installed = [base.clone(), overlay.clone()];

        let base_catalog = project_catalog_for_package(&catalog, &base.manifest, &installed)
            .expect("project base");
        assert_eq!(base_catalog.tables().len(), 1);
        let base_record = &base_catalog.tables()[0];
        assert_eq!(base_record.name(), "record");
        assert_eq!(
            base_record
                .columns()
                .iter()
                .map(wamn_schema_introspection::ir::Column::name)
                .collect::<Vec<_>>(),
            [
                "created_at",
                "created_by",
                "id",
                "maker_id",
                "updated_at",
                "updated_by"
            ]
        );
        assert_eq!(
            base_record
                .constraints()
                .iter()
                .map(wamn_schema_introspection::ir::Constraint::name)
                .collect::<Vec<_>>(),
            ["record_id_pkey"]
        );
        assert_eq!(
            base_record
                .exclusions()
                .iter()
                .map(wamn_schema_introspection::ir::Exclusion::name)
                .collect::<Vec<_>>(),
            ["record_maker_id_excl"]
        );

        let overlay_catalog = project_catalog_for_package(&catalog, &overlay.manifest, &installed)
            .expect("project overlay");
        assert_eq!(overlay_catalog.tables().len(), 2);
        let overlay_record = overlay_catalog
            .tables()
            .iter()
            .find(|table| table.name() == "record")
            .expect("overlay includes the extended base relation");
        assert_eq!(overlay_record.columns().len(), 8);
        assert_eq!(overlay_record.constraints().len(), 2);
        assert_eq!(overlay_record.exclusions().len(), 2);
        let clean_base = CatalogIr::new(vec![
            Table::new(
                "platform_fixture",
                "record",
                vec![
                    Column::new("id", SqlValueType::Uuid, false, None, None),
                    Column::new("maker_id", SqlValueType::Uuid, false, None, None),
                    Column::new("created_at", SqlValueType::Timestamptz, false, None, None),
                    Column::new("created_by", SqlValueType::Uuid, false, None, None),
                    Column::new("updated_at", SqlValueType::Timestamptz, false, None, None),
                    Column::new("updated_by", SqlValueType::Uuid, false, None, None),
                ],
                vec![Constraint::primary_key("record_id_pkey", ["id"]).unwrap()],
                Vec::new(),
            )
            .with_exclusions(vec![base_exclusion]),
        ]);
        let mut manifest = base.manifest;
        manifest.models.retain(|name, _| name == "record");
        manifest.custom_operations.clear();
        manifest.internal_relations.clear();
        let model = manifest.models.get_mut("record").unwrap();
        model.server_owned_fields.clear();
        model.enum_fields.clear();
        model
            .operations
            .retain(|action, _| *action == wamn_compiler::manifest::CrudAction::Get);
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let transactional = wamn_compiler::StatementTransactionality::unclassified();
        let generate = |catalog| {
            wamn_compiler::generate(&wamn_compiler::GenerationInput::new(
                catalog,
                &bytes,
                &[],
                &transactional,
            ))
            .unwrap()
        };
        let clean = generate(&clean_base);
        let retained = generate(&base_catalog);
        assert!(!clean.files().is_empty());
        assert_eq!(clean.files(), retained.files());
    }

    #[test]
    fn each_package_selects_the_component_its_build_records() {
        use wamn_compiler::build_record;
        let directory = std::env::temp_dir().join(format!(
            "wamn-dev-selection-{}-{}",
            std::process::id(),
            TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let packages = [
            ("platform_inventory", "1.0.0", "inventory"),
            ("client_north_inventory", "3.0.0", "client_north_inventory"),
        ]
        .map(|(id, version, component)| {
            let mut input = package(id, version, component);
            input.root = directory.join(id);
            // An unpacked layout: the build output is `generated/`.
            let output = input.root.join("generated");
            fs::create_dir_all(output.join("components")).expect("create fixture output");
            let path = format!("components/{component}.wasm");
            fs::write(output.join(&path), component).expect("write fixture component");
            let built = build_record::BuildRecord {
                schema: build_record::BUILD_RECORD_SCHEMA.to_owned(),
                package: build_record::BuildPackage {
                    id: id.to_owned(),
                    version: version.to_owned(),
                },
                inputs: build_record::BuildInputs {
                    digest: "sha256:inputs".to_owned(),
                    generation_digest: "sha256:generation".to_owned(),
                    files: BTreeMap::new(),
                },
                dependencies: Vec::new(),
                generator: build_record::BuildGenerator {
                    id: "generator".to_owned(),
                    toolchain: "toolchain".to_owned(),
                    sqlx: "0.9.0".to_owned(),
                },
                outputs: build_record::BuildOutputs {
                    manifest: "sha256:manifest".to_owned(),
                    files: BTreeMap::new(),
                    components: vec![build_record::BuildComponent {
                        name: component.to_owned(),
                        path,
                        sha256: wamn_digest::bytes_sha256(component.as_bytes()),
                    }],
                },
            };
            fs::write(
                output.join(build_record::BUILD_RECORD_FILE),
                built.to_bytes(),
            )
            .expect("write fixture build record");
            input
        });
        let selected = select_component_artifacts(&packages).expect("select both artifacts");
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].component.as_ref(), "inventory");
        assert_eq!(selected[1].component.as_ref(), "client_north_inventory");

        fs::write(
            packages[0].root.join("generated/components/inventory.wasm"),
            b"changed",
        )
        .expect("change a component");
        let refusal = select_component_artifacts(&packages)
            .expect_err("a changed component is refused")
            .to_string();
        assert!(refusal.contains("build.json records"), "{refusal}");
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    /// The binding store alias must be the one the wiring node names
    /// (wamn-qj68).
    #[test]
    fn a_binding_store_alias_that_differs_from_the_wiring_node_is_refused() {
        let components = std::collections::BTreeMap::from([(
            "blob-put".to_owned(),
            BTreeSet::from(["labels".to_owned()]),
        )]);
        let artifact = SelectedComponentArtifact {
            package_id: "wamn_wms".into(),
            package_version: "0.1.0".into(),
            component: "blob-put".into(),
            path: PathBuf::new(),
            digest: "sha256:fixture".into(),
        };

        require_wiring_store_alias(&components, &artifact, Some("labels"))
            .expect("the wiring alias is accepted");
        let refusal = require_wiring_store_alias(&components, &artifact, Some("archive"))
            .expect_err("another alias is refused")
            .to_string();
        assert!(
            refusal.contains("\"archive\"") && refusal.contains("\"labels\""),
            "the refusal names both aliases: {refusal}"
        );
    }
}

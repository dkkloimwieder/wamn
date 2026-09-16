//! Production stage coordinator for the canonical Receiving development loop.
//!
//! This module carries exact outputs between existing owners. It does not
//! reproduce migration, generation, admission, publication, release, or
//! activation semantics.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, anyhow};
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;
use tokio_postgres::NoTls;
use wamn_authoring_model::GateResult;
use wamn_catalog::{PackageCoordinate, WiringDocument};
use wamn_control::apply_package::{self, ApplyPackageRequest};
use wamn_control::component_declaration::{
    ComponentDeclarationError, ComponentDeclarationErrorKind, PACKAGE_MANIFEST,
    authored_base_digests, render_declaration_document,
};
use wamn_control::publish_release::{self, PublishReleaseRequest, ReleaseWiringTarget};
use wamn_control::push_component::{AdmitComponentRequest, ComponentAdmission, admit_component};
use wamn_control::reconcile_package_data_access;
use wamn_schema_control::BareSchemaName;
use wamn_schema_generator::{MaterializeMode, PackageManifest};
use wamn_schema_introspection::ir::{CatalogIr, Table};

use super::activation::{self, DevActivation, DevActivationRequest};
use super::config::{DevConfig, ResolvedDevPackages, VerifiedBaseComponentDigest};
use super::observations::DevObservationReaders;
use super::read::{
    DevGateOutcome, DevGateVerdict, DevReadHandle, DevReadPublisher, DevRuntimeEndpoint,
    dev_read_channel,
};
use super::target_database;
use super::watch::GitSource;
use super::{DevRunNotice, DevStage, DevStageFailure, DevStageRunner};
use wamn_control::print_release_env::ReleaseCarrier;

const BUILD_TOOL: &str = "tools/build-components";
const PACKAGE_WELD: &str = "generated/package-weld.json";
const RECORD_HISTORY_SQL: &str = "deploy/sql/record-history.sql";
const RUN_SCHEMA: &str = "wamn_run";

/// Stable code for the notice a run emits when it built past an authored pin.
pub const BASE_PIN_STALE_NOTICE: &str = "pin stale";
const PACKAGE_ATTACHMENTS: &str = "publication/attachments.json";
const PACKAGE_COMPONENTS: &str = "publication/components";
const PACKAGE_WIRINGS: &str = "publication/wirings";
const NODE_CAPABILITY: &str = "wamn:node";
const POSTGRES_CAPABILITY: &str = "wamn:postgres";

static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Stable category of a concrete development-stage failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductionDevStageErrorKind {
    InvalidState,
    StageOwner,
    AuthenticationUnavailable,
}

impl ProductionDevStageErrorKind {
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
    kind: ProductionDevStageErrorKind,
    operation: &'static str,
    endpoint: Option<Box<str>>,
    detail: Box<str>,
    source: Option<anyhow::Error>,
}

impl ProductionDevStageError {
    fn invalid(operation: &'static str, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind: ProductionDevStageErrorKind::InvalidState,
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
            kind: ProductionDevStageErrorKind::StageOwner,
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
            kind: ProductionDevStageErrorKind::StageOwner,
            operation,
            endpoint: Some(endpoint.into()),
            detail: detail.into(),
            source: None,
        }
    }

    fn authentication_unavailable(endpoint: impl Into<Box<str>>, source: anyhow::Error) -> Self {
        Self {
            kind: ProductionDevStageErrorKind::AuthenticationUnavailable,
            operation: "re-authenticate publisher",
            endpoint: Some(endpoint.into()),
            detail: format!("the configured identity authority is unavailable: {source:#}")
                .into_boxed_str(),
            source: Some(source),
        }
    }

    /// Stable failure category.
    pub const fn kind(&self) -> ProductionDevStageErrorKind {
        self.kind
    }

    /// Credential-free endpoint involved in this failure, when applicable.
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }
}

impl fmt::Display for ProductionDevStageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} while {}", self.kind.as_str(), self.operation)?;
        if let Some(endpoint) = &self.endpoint {
            write!(formatter, " at {endpoint}")?;
        }
        write!(formatter, ": {}", self.detail)
    }
}

impl Error for ProductionDevStageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_ref().map(AsRef::as_ref)
    }
}

#[derive(Debug, Deserialize)]
struct ComponentBuildPlan {
    virtualization: ComponentVirtualizationPlan,
}

#[derive(Debug, Deserialize)]
struct ComponentVirtualizationPlan {
    artifacts: Vec<ComponentArtifactPlan>,
}

#[derive(Clone, Debug, Deserialize)]
struct ComponentArtifactPlan {
    package: String,
    output: PathBuf,
}

#[derive(Debug)]
struct BuildStageOutput {
    bytes: Box<[u8]>,
    plan: ComponentBuildPlan,
}

#[derive(Clone, Debug)]
struct SelectedComponentArtifact {
    package_id: Box<str>,
    package_version: Box<str>,
    component: Box<str>,
    path: PathBuf,
    digest: Box<str>,
}

#[derive(Clone, Debug)]
struct PackageInput {
    root: PathBuf,
    manifest: PackageManifest,
}

fn project_catalog_for_package(
    catalog: &CatalogIr,
    target: &PackageManifest,
    installed: &[PackageInput],
) -> Result<CatalogIr, ProductionDevStageError> {
    let mut relation_owners = BTreeMap::<(String, String), String>::new();
    let mut field_owners = BTreeMap::<(String, String, String), String>::new();
    let mut constraint_owners = BTreeMap::<(String, String, String), String>::new();
    for package in installed {
        for model in package.manifest.models.values() {
            relation_owners
                .entry((model.schema.clone(), model.table.clone()))
                .or_insert_with(|| model.owner.clone());
            for (field, owner) in &model.field_owners {
                field_owners.insert(
                    (model.schema.clone(), model.table.clone(), field.clone()),
                    owner.clone(),
                );
            }
            for (constraint, owner) in &model.constraint_owners {
                constraint_owners.insert(
                    (
                        model.schema.clone(),
                        model.table.clone(),
                        constraint.clone(),
                    ),
                    owner.clone(),
                );
            }
        }
        for relation in package.manifest.internal_relations.values() {
            relation_owners.insert(
                (relation.schema.clone(), relation.table.clone()),
                package.manifest.package.id.clone(),
            );
        }
    }

    let admitted_owners = std::iter::once(target.package.id.as_str())
        .chain(
            target
                .base_dependencies
                .values()
                .map(|dependency| dependency.package.as_str()),
        )
        .collect::<BTreeSet<_>>();
    let mut tables = Vec::new();
    for table in catalog.tables() {
        let coordinate = (table.schema().to_owned(), table.name().to_owned());
        let relation_owner = relation_owners.get(&coordinate).ok_or_else(|| {
            ProductionDevStageError::invalid(
                "project package catalog",
                format!(
                    "{}.{} has no installed package definition owner",
                    table.schema(),
                    table.name()
                ),
            )
        })?;
        if !admitted_owners.contains(relation_owner.as_str()) {
            continue;
        }

        let columns = table
            .columns()
            .iter()
            .filter(|column| {
                let owner = field_owners
                    .get(&(
                        table.schema().to_owned(),
                        table.name().to_owned(),
                        column.name().to_owned(),
                    ))
                    .unwrap_or(relation_owner);
                admitted_owners.contains(owner.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        let column_names = columns
            .iter()
            .map(wamn_schema_introspection::ir::Column::name)
            .collect::<BTreeSet<_>>();
        let constraints = table
            .constraints()
            .iter()
            .filter(|constraint| {
                let owner = constraint_owners
                    .get(&(
                        table.schema().to_owned(),
                        table.name().to_owned(),
                        constraint.name().to_owned(),
                    ))
                    .unwrap_or(relation_owner);
                admitted_owners.contains(owner.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        let indexes = table
            .indexes()
            .iter()
            .filter(|index| {
                index
                    .columns()
                    .iter()
                    .all(|column| column_names.contains(column.name()))
            })
            .cloned()
            .collect::<Vec<_>>();
        let exclusions = table
            .exclusions()
            .iter()
            .filter(|exclusion| {
                let owner = constraint_owners
                    .get(&(
                        table.schema().to_owned(),
                        table.name().to_owned(),
                        exclusion.name().to_owned(),
                    ))
                    .unwrap_or(relation_owner);
                admitted_owners.contains(owner.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        tables.push(
            Table::new(table.schema(), table.name(), columns, constraints, indexes)
                .with_exclusions(exclusions),
        );
    }
    Ok(CatalogIr::new(tables))
}

#[derive(Clone, Debug)]
struct WiringInput {
    package_id: Box<str>,
    package_version: Box<str>,
    document: Value,
    wiring: WiringDocument,
}

#[derive(Debug)]
struct PreparedLocalGrants {
    target: target_database::PreparedConfiguration,
    data_access: reconcile_package_data_access::PreparedLocalDataAccess,
    input_digest: String,
}

/// Concrete runner carrying production-owner outputs through all ten stages.
pub struct ProductionDevStageRunner {
    config: DevConfig,
    overlay_root: PathBuf,
    git: GitSource,
    packages: Option<ResolvedDevPackages>,
    catalogs: BTreeMap<String, CatalogIr>,
    build: Option<BuildStageOutput>,
    artifacts: Vec<SelectedComponentArtifact>,
    verified_base_digests: Vec<VerifiedBaseComponentDigest>,
    admissions: Vec<ComponentAdmission>,
    gated_wirings: Vec<WiringInput>,
    target_instance: Option<String>,
    target_lease: Option<target_database::TargetLease>,
    release: Option<ReleaseCarrier>,
    local_bindings: Vec<wamn_runtime::local_application::LocalBindingFacts>,
    local_binding_inputs: Vec<PreparedLocalBinding>,
    local_grants: Option<PreparedLocalGrants>,
    local_admission_digest: Option<String>,
    activation: Option<DevActivation>,
    operator: Option<(
        super::native_tui::NativePackage,
        super::operator::OperatorControl,
    )>,
    native_binaries: BTreeMap<String, PathBuf>,
    generated_native_outputs: Option<super::watch::GeneratedNativeOutputs>,
    generate_input_digest: Option<String>,
    generate_input_candidate: Option<String>,
    generated_output_digest: Option<String>,
    schema_input_digest: Option<String>,
    schema_input_candidate: Option<String>,
    /// Structure of the packages that the current target instance took.
    target_structure_digest: Option<String>,
    /// Inputs of each verifier's last successful SQLx preparation; `None` while unknown.
    sqlx_metadata_inputs: BTreeMap<String, Option<SqlxMetadataInputs>>,
    acl_input_digest: Option<String>,
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
            .field(
                "verified_base_digest_count",
                &self.verified_base_digests.len(),
            )
            .field(
                "base_digests_moved_off_pin",
                &self.base_digests_moved_off_pin().collect::<Vec<_>>(),
            )
            .field("admission_count", &self.admissions.len())
            .field("gated_wiring_count", &self.gated_wirings.len())
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
    pub fn new(config: DevConfig, overlay_root: PathBuf, git: GitSource) -> Self {
        let (read_publisher, read_handle) = dev_read_channel();
        Self {
            config,
            overlay_root,
            git,
            packages: None,
            catalogs: BTreeMap::new(),
            build: None,
            artifacts: Vec::new(),
            verified_base_digests: Vec::new(),
            admissions: Vec::new(),
            gated_wirings: Vec::new(),
            target_instance: None,
            target_lease: None,
            release: None,
            local_bindings: Vec::new(),
            local_binding_inputs: Vec::new(),
            local_grants: None,
            local_admission_digest: None,
            activation: None,
            operator: None,
            native_binaries: BTreeMap::new(),
            generated_native_outputs: None,
            generate_input_digest: None,
            generate_input_candidate: None,
            generated_output_digest: None,
            schema_input_digest: None,
            schema_input_candidate: None,
            target_structure_digest: None,
            sqlx_metadata_inputs: BTreeMap::new(),
            acl_input_digest: None,
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

    /// Exact release carrier minted and pushed by the Release stage.
    pub const fn release_carrier(&self) -> Option<&ReleaseCarrier> {
        self.release.as_ref()
    }

    /// Select one operator package before this runner starts its first run.
    pub(super) fn configure_operator(
        &mut self,
        package: super::native_tui::NativePackage,
        control: super::operator::OperatorControl,
    ) {
        self.operator = Some((package, control));
    }

    /// Share each successful package emission with the filesystem watcher.
    pub(super) fn configure_generated_native_outputs(
        &mut self,
        outputs: super::watch::GeneratedNativeOutputs,
    ) {
        self.generated_native_outputs = Some(outputs);
    }

    /// Stop any active workload and local host owned by this runner.
    pub async fn shutdown(&mut self) -> Result<(), ProductionDevStageError> {
        self.read_publisher.clear_runtime_endpoint();
        let operator_result = if let Some((_, control)) = &self.operator {
            match control.stop("the target is unavailable").await {
                Ok(()) => Ok(()),
                Err(source) if source.process_stopped() => Err(ProductionDevStageError::owner(
                    "operator terminal exited",
                    source.into(),
                )),
                Err(source) => {
                    return Err(ProductionDevStageError::owner(
                        "stop the operator terminal before target shutdown",
                        source.into(),
                    ));
                }
            }
        } else {
            Ok(())
        };
        let Some(active) = self.activation.take() else {
            return operator_result;
        };
        let activation_result = active
            .shutdown()
            .await
            .map_err(|source| ProductionDevStageError::owner("clean up activation", source.into()));
        operator_result.and(activation_result)
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
        wamn_control::verification_policy::project_environment_policy(
            self.config.system_database_url(),
            self.config.target_database_url(),
            &run_schema,
            &self.config.activation_identity().org,
            &self.config.activation_identity().tenant,
            &self.config.activation_identity().environment,
        )
        .await
        .map_err(|source| ProductionDevStageError::owner("project environment policy", source))?;

        for package in package_inputs {
            let outcome = apply_package::apply_local_package(
                ApplyPackageRequest {
                    package: package.root,
                    database_url: self.config.target_database_url().to_owned(),
                    tenant: self.config.activation_identity().tenant.clone(),
                },
                &self.config.activation_identity().environment,
            )
            .await
            .map_err(|source| {
                ProductionDevStageError::owner("apply package to verification", source)
            })?;
            crate::package_verbs::print_applied(&outcome);
        }
        self.schema_input_digest
            .clone_from(&self.schema_input_candidate);
        Ok(())
    }

    async fn introspect(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Introspect);
        let packages = self.package_inputs()?;
        for package in &packages {
            let catalog = wamn_schema_generator::introspect_package(
                self.config.target_database_url(),
                &package.root,
            )
            .await
            .map_err(|source| ProductionDevStageError::owner("introspect package", source))?;
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
                .expect("prepare_run claimed the target instance"),
            self.schema_input_digest
                .as_deref()
                .expect("Migrate recorded the schema inputs"),
            self.target_structure_digest
                .as_deref()
                .expect("prepare_run recorded the target structure"),
            &self.catalogs,
        )
        .map_err(|source| ProductionDevStageError::owner("record the target schema", source))?;
        Ok(())
    }

    async fn generate(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Generate);
        let packages = self.package_inputs()?;
        for package in packages {
            let catalog = self
                .catalogs
                .get(&package.manifest.package.id)
                .ok_or_else(|| {
                    ProductionDevStageError::invalid(
                        "generate package",
                        format!(
                            "no introspection exists for {}@{}",
                            package.manifest.package.id, package.manifest.package.version
                        ),
                    )
                })?;
            // The SERVER decides which statements need a transaction, by
            // planning each one against the migrated database. The unclassified
            // entry point writes `transactional: false` for every statement,
            // which rewrites the committed contracts (wamn-10yt.10.33). The
            // narrowed catalog above is passed through, because every package
            // here shares one target database and re-introspecting would hand
            // this package the relations and fields its neighbours own.
            wamn_schema_generator::materialize_package_verified_with_catalog(
                MaterializeMode::Write,
                catalog,
                self.config.target_database_url(),
                &package.root,
            )
            .await
            .map_err(|source| {
                ProductionDevStageError::owner("materialize generated package", source)
            })?;
            // Acknowledge this emission now: accepting snapshots after Build
            // would hide external edits made while the later stages were running.
            if let Some(outputs) = &self.generated_native_outputs {
                outputs.acknowledge(&package.root).map_err(|source| {
                    ProductionDevStageError::owner(
                        "acknowledge generated native outputs",
                        source.into(),
                    )
                })?;
            }
        }
        let selected = self.package_inputs()?;
        if selected.iter().any(|package| {
            wamn_control::delivery::sqlx::verifier_for(&package.manifest.package.id).is_some()
        }) {
            let output = super::execute_preparation(
                Command::new("cargo").args(["sqlx", "--version"]),
                super::INPUT_COMMAND_TIMEOUT,
            )
            .await
            .map_err(|source| ProductionDevStageError::owner("read SQLx CLI version", source))?;
            require_command_success("read SQLx CLI version", &output)?;
            wamn_control::delivery::sqlx::require_cli_version(&output.stdout).map_err(
                |source| ProductionDevStageError::owner("require pinned SQLx CLI", source),
            )?;
        }
        for package in selected {
            let package_id = &package.manifest.package.id;
            if let Some(verifier) = wamn_control::delivery::sqlx::verifier_for(package_id) {
                let current =
                    sqlx_metadata_inputs_on_disk(self.git.repository_root(), &package.root)
                        .map_err(|source| {
                            ProductionDevStageError::owner("read SQLx metadata inputs", source)
                        })?;
                if sqlx_metadata_is_current(
                    self.sqlx_metadata_inputs.get(package_id),
                    self.git.repository_root(),
                    &package.root,
                    &current,
                )
                .await?
                {
                    self.sqlx_metadata_inputs
                        .insert(package_id.clone(), Some(current));
                    continue;
                }
                // Until a preparation succeeds, the metadata on disk is unknown.
                self.sqlx_metadata_inputs.insert(package_id.clone(), None);
                let database_url = wamn_control::delivery::sqlx::package_database_url(
                    self.config.target_database_url(),
                    &package.manifest,
                )
                .map_err(|source| {
                    ProductionDevStageError::owner("scope SQLx to the package schemas", source)
                })?;
                let mut command = wamn_control::delivery::sqlx::prepare_command(
                    &package.root.join("tests"),
                    &database_url,
                    verifier,
                    false,
                );
                let output = super::execute_preparation(&mut command, super::PREPARATION_TIMEOUT)
                    .await
                    .map_err(|source| {
                        ProductionDevStageError::owner("refresh selected SQLx metadata", source)
                    })?;
                require_command_success("prepare selected SQLx verifier", &output)?;
                self.sqlx_metadata_inputs
                    .insert(package_id.clone(), Some(current));
            }
        }
        let outputs = generated_outputs_digest(&self.package_inputs()?)?;
        if outputs == "missing" {
            return Err(ProductionDevStageError::invalid(
                "verify generated outputs",
                "generation or SQLx preparation left required outputs missing",
            ));
        }
        self.generated_output_digest = Some(outputs);
        Ok(())
    }

    async fn build(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Build);
        let roots = self
            .package_inputs()?
            .into_iter()
            .map(|package| package.root)
            .collect::<Vec<_>>();
        let tool = self.git.repository_root().join(BUILD_TOOL);
        let output = super::execute_preparation(
            Command::new(&tool).args(["build-only", "app"]).args(&roots),
            super::PREPARATION_TIMEOUT,
        )
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("start the production component build", source)
        })?;
        require_command_success("build production components", &output)?;
        let plan = serde_json::from_slice(&output.stdout).map_err(|source| {
            ProductionDevStageError::owner(
                "decode the production build artifact plan",
                source.into(),
            )
        })?;
        self.build = Some(BuildStageOutput {
            bytes: output.stdout.into_boxed_slice(),
            plan,
        });
        self.native_binaries = super::native_tui::build(&roots).await.map_err(|source| {
            ProductionDevStageError::owner("build native operator terminals", source.into())
        })?;
        Ok(())
    }

    async fn virtualize(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Virtualize);
        let build = self.build.as_ref().ok_or_else(|| {
            ProductionDevStageError::invalid(
                "virtualize components",
                "the Build stage produced no artifact plan",
            )
        })?;
        let plan_file = TemporaryFile::write(&build.bytes).map_err(|source| {
            ProductionDevStageError::owner("write the build artifact-plan handoff", source)
        })?;
        let tool = self.git.repository_root().join(BUILD_TOOL);
        let output = super::execute_preparation(
            Command::new(&tool)
                .arg("virtualize-only")
                .arg(plan_file.path()),
            super::PREPARATION_TIMEOUT,
        )
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("start the production component virtualizer", source)
        })?;
        require_command_success("virtualize production components", &output)?;
        self.artifacts = select_component_artifacts(
            &self.package_inputs()?,
            &build.plan.virtualization.artifacts,
        )?;

        let packages = self
            .packages
            .as_ref()
            .expect("package_inputs checked state");
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
                &self.config.activation_identity().tenant,
                &base_digests,
            )?;
            let admission = admit_component(AdmitComponentRequest {
                package: package.root,
                component_bytes: artifact.path,
                declaration: declaration.path().to_owned(),
                admitted_platform_packages: vec![
                    NODE_CAPABILITY.to_owned(),
                    POSTGRES_CAPABILITY.to_owned(),
                ],
            })
            .map_err(|source| {
                ProductionDevStageError::owner("admit exact component bytes", source)
            })?;
            if admission.package_id() != artifact.package_id.as_ref()
                || admission.package_version() != artifact.package_version.as_ref()
                || admission.component() != artifact.component.as_ref()
                || admission.component_digest() != artifact.digest.as_ref()
            {
                return Err(ProductionDevStageError::invalid(
                    "carry admitted component identity",
                    format!(
                        "{}@{}::{} digest {} differs from selected {}@{}::{} digest {}",
                        admission.package_id(),
                        admission.package_version(),
                        admission.component(),
                        admission.component_digest(),
                        artifact.package_id,
                        artifact.package_version,
                        artifact.component,
                        artifact.digest
                    ),
                ));
            }
            self.admissions.push(admission);
        }
        Ok(())
    }

    async fn gate(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Gate);
        let mut read_outcomes = Vec::new();
        if self.admissions.len() != self.package_inputs()?.len() {
            return Err(ProductionDevStageError::invalid(
                "gate package wirings",
                "every package must carry one exact admission before Gate",
            ));
        }
        for input in load_wirings(&self.package_inputs()?)? {
            let command_id = authoring_command_id(
                "gate",
                &input.package_id,
                &input.package_version,
                &input.document,
                self.target_instance.as_deref(),
            );
            self.reauthenticate_publisher().await?;
            let scope = wamn_catalog::ComponentPackageScope {
                tenant_id: self.config.activation_identity().tenant.clone(),
                package_id: input.package_id.to_string(),
                package_version: input.package_version.to_string(),
            };
            let facts = self
                .admissions
                .iter()
                .filter(|admission| admission.facts().scope == scope)
                .map(|admission| admission.facts().clone())
                .collect::<Vec<_>>();
            let outcome = wamn_authoring_model::gate::judge_gate_document(
                &input.wiring,
                &scope,
                &facts,
            )
            .map(|()| GateResult {
                report_id: format!("local:{command_id}"),
                validated_draft: wamn_authoring_model::ValidatedDraftRef {
                    validated_draft_id: format!("local:{}", input.wiring.wiring_hash().as_str()),
                },
            });
            match outcome {
                Ok(result) => {
                    read_outcomes.push(DevGateOutcome {
                        package_id: input.package_id.to_string(),
                        package_version: input.package_version.to_string(),
                        wiring_id: input.wiring.wiring_id.clone(),
                        wiring_version: input.wiring.version,
                        verdict: DevGateVerdict::Accepted(result),
                    });
                    self.read_publisher.set_gate_outcomes(read_outcomes.clone());
                    self.gated_wirings.push(input);
                }
                Err(refusal) => {
                    read_outcomes.push(DevGateOutcome {
                        package_id: input.package_id.to_string(),
                        package_version: input.package_version.to_string(),
                        wiring_id: input.wiring.wiring_id.clone(),
                        wiring_version: input.wiring.version,
                        verdict: DevGateVerdict::Refused(refusal.clone()),
                    });
                    self.read_publisher.set_gate_outcomes(read_outcomes);
                    return Err(ProductionDevStageError::invalid(
                        "submit production Gate",
                        format!(
                            "{}@{}::{} was refused: {refusal:?}",
                            input.package_id, input.package_version, input.wiring.wiring_id
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    async fn acl(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Acl);
        let packages = self
            .package_inputs()?
            .into_iter()
            .map(|package| package.root)
            .collect::<Vec<_>>();
        let input_digest = self.generate_inputs_digest()?;
        let prepared = PreparedLocalGrants {
            target: target_database::prepare_configuration(&self.config).map_err(|source| {
                ProductionDevStageError::owner("prepare local target privileges", source)
            })?,
            data_access: reconcile_package_data_access::prepare_local(&packages).map_err(
                |source| {
                    ProductionDevStageError::owner("prepare generated package data access", source)
                },
            )?,
            input_digest,
        };
        self.reconcile_local_grants(&prepared, false).await?;
        if prepared.input_digest != self.generate_inputs_digest()? {
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
            &self.config.activation_identity().tenant,
            &self.config.activation_identity().environment,
            apply,
        )
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("reconcile generated package data access", source)
        })?;
        Ok(())
    }

    async fn release(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Release);
        if self.gated_wirings.is_empty() {
            return Err(ProductionDevStageError::invalid(
                "mint effective release",
                "the Gate stage produced no accepted wiring",
            ));
        }
        let principal = self.reauthenticate_publisher().await?;
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
        let wirings = self
            .gated_wirings
            .iter()
            .map(|gated| ReleaseWiringTarget {
                package_id: gated.package_id.to_string(),
                package_version: gated.package_version.to_string(),
                wiring_id: gated.wiring.wiring_id.clone(),
                wiring_version: gated.wiring.version,
            })
            .collect();
        let attachments = packages
            .iter()
            .map(|package| package.root.join(PACKAGE_ATTACHMENTS))
            .collect();
        let package_manifests = packages
            .iter()
            .map(|package| package.root.join(PACKAGE_MANIFEST))
            .collect();
        let identity = self.config.activation_identity();
        let request = PublishReleaseRequest {
            database_url: self.config.target_database_url().to_owned(),
            control_database_url: self.config.system_database_url().to_owned(),
            org: identity.org.clone(),
            project: identity.project.clone(),
            tenant: identity.tenant.clone(),
            effective_release_id: self.config.effective_release_id(),
            environment: identity.environment.clone(),
            verified_publisher_principal: principal,
            run_schema: RUN_SCHEMA.to_owned(),
            packages: package_coordinates,
            wirings,
            attachments,
            route_host: Some(self.config.route_host().to_owned()),
            package_manifests,
        };
        let local = self.config.local_artifacts();
        let documents = self
            .gated_wirings
            .iter()
            .map(|gated| {
                (
                    wamn_catalog::ComponentPackageScope {
                        tenant_id: identity.tenant.clone(),
                        package_id: gated.package_id.to_string(),
                        package_version: gated.package_version.to_string(),
                    },
                    gated.wiring.clone(),
                )
            })
            .collect();
        let (minted, mut local_facts) =
            publish_release::mint_local(request, &self.admissions, documents)
                .await
                .map_err(|source| {
                    ProductionDevStageError::owner("assemble local application", source)
                })?;
        let binding_inputs =
            prepare_local_bindings(&self.config, &self.admissions).map_err(|source| {
                ProductionDevStageError::owner("prepare local connection selections", source)
            })?;
        local_facts.bindings = resolve_local_bindings(&self.config, &binding_inputs, None)
            .await
            .map_err(|source| {
                ProductionDevStageError::owner("validate local connection selections", source)
            })?;
        wamn_runtime::local_application::validate_local_facts(&local_facts, &minted.manifest)
            .map_err(|source| {
                ProductionDevStageError::owner("validate complete local application", source)
            })?;
        fs::create_dir_all(&local.directory).map_err(|source| {
            ProductionDevStageError::owner("create local artifact directory", source.into())
        })?;
        for artifact in &self.artifacts {
            let bytes = fs::read(&artifact.path).map_err(|source| {
                ProductionDevStageError::owner("read local component", source.into())
            })?;
            if wamn_runtime::component_admission::component_digest(&bytes)
                != artifact.digest.as_ref()
            {
                return Err(ProductionDevStageError::invalid(
                    "stage local component",
                    "admitted component bytes changed",
                ));
            }
            let path = wamn_runtime::component_artifact_source::local_component_path(
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
            &minted.canonical_bytes,
        )
        .map_err(|source| {
            ProductionDevStageError::owner("stage local application manifest", source.into())
        })?;
        let admission_bytes = serde_json::to_vec(&local_facts).map_err(|source| {
            ProductionDevStageError::owner("encode local admissions", source.into())
        })?;
        let admission_digest =
            wamn_runtime::component_admission::component_digest(&admission_bytes);
        fs::write(
            local
                .directory
                .join(wamn_runtime::local_application::LOCAL_FACTS_FILE),
            admission_bytes,
        )
        .map_err(|source| {
            ProductionDevStageError::owner("stage local admissions", source.into())
        })?;
        let carrier = ReleaseCarrier {
            artifact_base: format!("local:{}", local.directory.display()),
            manifest_digest: minted.digest,
        };
        activation::prepare_local(&DevActivationRequest {
            config: &self.config,
            release: &carrier,
            identity: self.config.activation_identity(),
            host_binary: self.config.host_binary(),
            wasmtime_cache_dir: self.config.wasmtime_cache_dir(),
            host_output_log: None,
            local_admission_digest: Some(&admission_digest),
        })
        .map_err(|source| {
            ProductionDevStageError::owner(
                "validate local workload before replacement",
                source.into(),
            )
        })?;
        self.read_publisher
            .set_release(minted.manifest, carrier.clone());
        self.release = Some(carrier);
        self.local_bindings = local_facts.bindings;
        self.local_binding_inputs = binding_inputs;
        self.local_admission_digest = Some(admission_digest);
        Ok(())
    }

    async fn activate(&mut self) -> Result<(), ProductionDevStageError> {
        if self.activation.is_some() {
            self.shutdown().await?;
        }
        resolve_local_bindings(
            &self.config,
            &self.local_binding_inputs,
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
        self.acl_input_digest = Some(grants.input_digest.clone());
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
        let host_output_log = self
            .operator
            .as_ref()
            .map(|_| operator_host_output_log(self.config.wasmtime_cache_dir(), target_instance));
        if let Some(path) = &host_output_log {
            eprintln!("Host diagnostics: {}", path.display());
        }
        let activation = activation::activate(DevActivationRequest {
            config: &self.config,
            release,
            identity: self.config.activation_identity(),
            host_binary: self.config.host_binary(),
            wasmtime_cache_dir: self.config.wasmtime_cache_dir(),
            host_output_log: host_output_log.as_deref(),
            local_admission_digest: self.local_admission_digest.as_deref(),
        })
        .await
        .map_err(|source| {
            let context = host_output_log
                .as_ref()
                .map(|path| format!("{source}; host diagnostics: {}", path.display()));
            let source = anyhow::Error::new(source);
            let source = if let Some(context) = context {
                source.context(context)
            } else {
                source
            };
            ProductionDevStageError::owner("activate local host and flow-http", source)
        })?;
        let endpoint = DevRuntimeEndpoint::new(
            activation.http_base_url(),
            self.config.route_host(),
            target_instance,
        );
        self.activation = Some(activation);
        self.read_publisher.set_runtime_endpoint(endpoint.clone());
        if let Some((package, control)) = &self.operator {
            let launched = async {
                let executable = self
                    .native_binaries
                    .get(&package.component)
                    .ok_or_else(|| {
                        ProductionDevStageError::invalid(
                            "launch operator terminal",
                            "Build produced no selected native binary",
                        )
                    })?;
                let operator_token = self.config.operator_bearer_token().ok_or_else(|| {
                    ProductionDevStageError::invalid(
                        "launch operator terminal",
                        "dev.json has no operator_bearer_token",
                    )
                })?;
                control
                    .start(super::operator::LaunchSpec {
                        executable: executable.clone(),
                        base_url: endpoint.base_url().to_owned(),
                        route_host: endpoint.route_host().to_owned(),
                        target_instance: endpoint.target_instance().to_owned(),
                        operator_token: operator_token.to_owned(),
                    })
                    .await
                    .map_err(|source| {
                        ProductionDevStageError::owner("launch operator terminal", source.into())
                    })
            }
            .await;
            if let Err(error) = launched {
                if let Err(cleanup) = self.shutdown().await {
                    return Err(ProductionDevStageError::owner(
                        "clean up failed operator launch",
                        anyhow::Error::new(error)
                            .context(format!("activation cleanup also failed: {cleanup}")),
                    ));
                }
                return Err(error);
            }
        }
        Ok(())
    }

    /// Digest every authored byte Generate reads.
    ///
    /// Includes package contracts, SQL, grants, verifier sources and active
    /// checkers. Component Rust edits leave these generation inputs unchanged.
    fn generate_inputs_digest(&self) -> Result<String, ProductionDevStageError> {
        let mut inputs: Vec<(String, String)> = Vec::new();
        for package in self.package_inputs()? {
            let mut package_inputs = Vec::new();
            collect_authored_bytes(&package.root, &package.root, &mut package_inputs)?;
            inputs.extend(
                package_inputs
                    .into_iter()
                    .filter(|(path, _)| {
                        std::path::Path::new(path)
                            .extension()
                            .is_none_or(|extension| extension != "rs")
                            || path.starts_with("tests/")
                    })
                    .map(|(path, bytes)| {
                        (format!("{}:{path}", package.manifest.package.id), bytes)
                    }),
            );
        }
        for relative in [
            "Cargo.lock",
            "Cargo.toml",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            ".cargo/config",
            "tools/build-components",
        ] {
            let path = self.git.repository_root().join(relative);
            if path.exists() {
                inputs.push((relative.to_owned(), file_digest(&path)?));
            }
        }
        for path in [
            self.config.target_privileges_file(),
            self.config.target_database_acl_file(),
        ] {
            inputs.push((path.display().to_string(), file_digest(path)?));
        }
        if self.package_inputs()?.iter().any(|package| {
            wamn_control::delivery::sqlx::verifier_for(&package.manifest.package.id).is_some()
        }) {
            let path = std::env::var_os("PATH")
                .and_then(|path| {
                    std::env::split_paths(&path)
                        .map(|directory| directory.join("cargo-sqlx"))
                        .find(|path| path.is_file())
                })
                .ok_or_else(|| {
                    ProductionDevStageError::invalid(
                        "locate SQLx checker",
                        "SQLx CLI 0.9.0 is required on PATH",
                    )
                })?;
            inputs.push(("sqlx-checker".to_owned(), file_digest(&path)?));
        }
        let checker = std::env::current_exe().map_err(|source| {
            ProductionDevStageError::owner("locate active checker", source.into())
        })?;
        let metadata = fs::metadata(&checker).map_err(|source| {
            ProductionDevStageError::owner("inspect active checker", source.into())
        })?;
        inputs.push((
            "active-checker".to_owned(),
            format!(
                "{}:{}:{:?}",
                checker.display(),
                metadata.len(),
                metadata.modified()
            ),
        ));
        inputs.sort();
        let inputs = serde_json::to_value(&inputs).map_err(|source| {
            ProductionDevStageError::owner("serialize the authored input digest", source.into())
        })?;
        Ok(wamn_execution_contract::canonical_json_sha256(&inputs))
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

    async fn reauthenticate_publisher(&self) -> Result<String, ProductionDevStageError> {
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
            wamn_platform_identity::authenticate_pat(&client, self.config.gate_bearer_token())
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
        let authenticated = authenticated.ok_or_else(|| {
            ProductionDevStageError::owner_at(
                "re-authenticate publisher",
                endpoint,
                "the configured PAT was refused; supply a valid credential",
            )
        })?;
        Ok(authenticated.principal().id().as_str().to_owned())
    }

    fn clear_after(&mut self, stage: DevStage) {
        match stage {
            DevStage::Migrate | DevStage::Introspect => {
                self.catalogs.clear();
                self.build = None;
                self.artifacts.clear();
                self.verified_base_digests.clear();
                self.admissions.clear();
                self.gated_wirings.clear();
                self.release = None;
            }
            DevStage::Generate | DevStage::Build => {
                self.build = None;
                self.artifacts.clear();
                self.verified_base_digests.clear();
                self.admissions.clear();
                self.gated_wirings.clear();
                self.release = None;
            }
            DevStage::Virtualize => {
                self.artifacts.clear();
                self.verified_base_digests.clear();
                self.admissions.clear();
                self.gated_wirings.clear();
                self.release = None;
            }
            DevStage::Admit => {
                self.admissions.clear();
                self.gated_wirings.clear();
                self.release = None;
            }
            DevStage::Gate => {
                self.gated_wirings.clear();
                self.release = None;
            }
            DevStage::Acl | DevStage::Release => {
                self.release = None;
            }
            DevStage::Activate => {}
        }
    }
}

/// A fresh retained log for each activation, including saves on the same target.
pub(super) fn operator_host_output_log(cache_directory: &Path, target_instance: &str) -> PathBuf {
    // Wasmtime owns its cache contents; diagnostics use the existing sibling namespace.
    let mut directory = cache_directory
        .components()
        .collect::<PathBuf>()
        .into_os_string();
    directory.push(".operator-logs");
    let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(directory).join(format!(
        "operator-host-{}-{target_instance}-{sequence}.log",
        std::process::id()
    ))
}

fn file_digest(path: &Path) -> Result<String, ProductionDevStageError> {
    let bytes = fs::read(path).map_err(|source| {
        ProductionDevStageError::owner("read development input", source.into())
    })?;
    Ok(wamn_runtime::component_admission::component_digest(&bytes))
}

fn schema_inputs_digest(
    packages: &[PackageInput],
    config: &DevConfig,
) -> Result<String, ProductionDevStageError> {
    let mut inputs = Vec::new();
    for package in packages {
        let directory = apply_package::read_package_directory(&package.root)
            .map_err(|source| ProductionDevStageError::owner("read schema input", source))?;
        wamn_schema_control::plan_package_migrations(&directory, None).map_err(|source| {
            ProductionDevStageError::owner("validate schema inputs", source.into())
        })?;
        inputs.push(package_schema_inputs(&package.manifest, &directory));
    }
    inputs.push(serde_json::json!({"template": config.target_template_database()}));
    Ok(wamn_execution_contract::canonical_json_sha256(
        &serde_json::json!(inputs),
    ))
}

fn package_schema_inputs(
    manifest: &PackageManifest,
    directory: &wamn_schema_control::PackageDirectory,
) -> Value {
    let mut models = serde_json::to_value(&manifest.models).expect("models serialize");
    for model in models
        .as_object_mut()
        .expect("models are a map")
        .values_mut()
    {
        model
            .as_object_mut()
            .expect("model is an object")
            .remove("operations");
    }
    let migrations = directory
        .migrations
        .iter()
        .map(|migration| {
            (
                &migration.relative_path,
                wamn_runtime::component_admission::component_digest(&migration.bytes),
            )
        })
        .collect::<Vec<_>>();
    serde_json::json!({"package": manifest.package, "models": models, "internal-relations": manifest.internal_relations, "migrations": migrations})
}

/// Digest of the package inputs whose change needs a new target.
fn target_structure_digest(packages: &[PackageInput], config: &DevConfig) -> String {
    let mut inputs = packages
        .iter()
        .map(|package| package_structure_inputs(&package.manifest))
        .collect::<Vec<_>>();
    inputs.push(serde_json::json!({"template": config.target_template_database()}));
    wamn_execution_contract::canonical_json_sha256(&serde_json::json!(inputs))
}

/// The package identity, the models with their definition owners, and the internal relations.
///
/// A kept target takes appended migrations, enum_fields, server_owned_fields,
/// and audit_log in place, so they are not structure.
fn package_structure_inputs(manifest: &PackageManifest) -> Value {
    let models = manifest
        .models
        .iter()
        .map(|(model_id, model)| {
            let wamn_schema_generator::ModelDeclaration {
                schema,
                table,
                owner,
                client_field_extensible,
                field_owners,
                constraint_owners,
                server_owned_fields: _,
                enum_fields: _,
                audit_log: _,
                operations: _,
            } = model;
            (
                model_id,
                serde_json::json!({
                    "schema": schema,
                    "table": table,
                    "owner": owner,
                    "client_field_extensible": client_field_extensible,
                    "field_owners": field_owners,
                    "constraint_owners": constraint_owners,
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    serde_json::json!({"package": manifest.package, "models": models, "internal-relations": manifest.internal_relations})
}

fn generated_outputs_digest(packages: &[PackageInput]) -> Result<String, ProductionDevStageError> {
    let mut files = Vec::new();
    for package in packages {
        let mut roots = vec![("generated", package.root.join("generated"))];
        if wamn_control::delivery::sqlx::verifier_for(&package.manifest.package.id).is_some() {
            roots.push(("sqlx", package.root.join("tests/.sqlx")));
        }
        for (kind, root) in roots {
            if !root.is_dir() {
                return Ok("missing".to_owned());
            }
            let mut contents = Vec::new();
            collect_authored_bytes(&root, &root, &mut contents)?;
            if contents.is_empty() {
                return Ok("missing".to_owned());
            }
            files.extend(contents.into_iter().map(|(path, bytes)| {
                (
                    format!("{}:{kind}:{path}", package.manifest.package.id),
                    bytes,
                )
            }));
        }
    }
    files.sort();
    Ok(wamn_execution_contract::canonical_json_sha256(
        &serde_json::json!(files),
    ))
}

/// Everything that changes a verifier's SQLx metadata.
///
/// SQLx describes the emitted SQL against the verified schema, which includes
/// the record-history functions that authored SQL calls. The locked SQLx
/// macros write the metadata files. A Rust-only edit changes none of these.
#[derive(Debug, PartialEq, Eq)]
struct SqlxMetadataInputs {
    sql_corpus: Box<str>,
    schema_state: Box<str>,
    record_history: Vec<u8>,
    sqlx_packages: Vec<String>,
}

fn sqlx_metadata_inputs(
    weld: &[u8],
    record_history: Vec<u8>,
    cargo_lock: &[u8],
) -> anyhow::Result<SqlxMetadataInputs> {
    let weld = wamn_schema_generator::GeneratedPackageMetadata::from_slice(weld)?;
    let mut name = "";
    let mut sqlx_packages = Vec::new();
    for line in std::str::from_utf8(cargo_lock)?.lines() {
        if let Some(value) = line.strip_prefix("name = ") {
            name = value;
        } else if let Some(version) = line.strip_prefix("version = ")
            && (name == "\"sqlx\"" || name.starts_with("\"sqlx-"))
        {
            sqlx_packages.push(format!("{name} {version}"));
        }
    }
    Ok(SqlxMetadataInputs {
        sql_corpus: weld.application_sql_corpus_identity().into(),
        schema_state: weld.verified_schema_state_id().into(),
        record_history,
        sqlx_packages,
    })
}

fn sqlx_metadata_inputs_on_disk(
    repository: &Path,
    package: &Path,
) -> anyhow::Result<SqlxMetadataInputs> {
    let read = |path: PathBuf| fs::read(&path).with_context(|| format!("read {}", path.display()));
    sqlx_metadata_inputs(
        &read(package.join(PACKAGE_WELD))?,
        read(repository.join(RECORD_HISTORY_SQL))?,
        &read(repository.join("Cargo.lock"))?,
    )
}

/// Whether the verifier's SQLx metadata was prepared for `current`.
///
/// In a session, the last preparation answers, and an unfinished one never
/// matches. A new session compares with the committed files, whose metadata
/// release qualification checks.
async fn sqlx_metadata_is_current(
    prepared: Option<&Option<SqlxMetadataInputs>>,
    repository: &Path,
    package: &Path,
    current: &SqlxMetadataInputs,
) -> Result<bool, ProductionDevStageError> {
    if let Some(prepared) = prepared {
        return Ok(prepared.as_ref() == Some(current));
    }
    let (Some(weld), Some(record_history), Some(cargo_lock)) = (
        committed_file(package, PACKAGE_WELD).await?,
        committed_file(repository, RECORD_HISTORY_SQL).await?,
        committed_file(repository, "Cargo.lock").await?,
    ) else {
        return Ok(false);
    };
    Ok(sqlx_metadata_inputs(&weld, record_history, &cargo_lock)
        .is_ok_and(|committed| committed == *current))
}

/// The bytes of `path`, relative to `directory`, at `HEAD`; `None` if `HEAD` lacks it.
async fn committed_file(
    directory: &Path,
    path: &str,
) -> Result<Option<Vec<u8>>, ProductionDevStageError> {
    let output = super::execute_preparation(
        Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(["show", &format!("HEAD:./{path}")]),
        super::INPUT_COMMAND_TIMEOUT,
    )
    .await
    .map_err(|source| ProductionDevStageError::owner("read a committed SQLx input", source))?;
    Ok(output.status.success().then_some(output.stdout))
}

/// Read every authored file under `directory`, skipping the generated subtree.
///
/// A read failure is a refusal rather than an omission: a file the walk cannot
/// read is a file whose change would go unnoticed.
fn collect_authored_bytes(
    root: &Path,
    directory: &Path,
    inputs: &mut Vec<(String, String)>,
) -> Result<(), ProductionDevStageError> {
    let entries = std::fs::read_dir(directory)
        .map_err(|source| ProductionDevStageError::owner("read the package tree", source.into()))?;
    for entry in entries {
        let entry = entry.map_err(|source| {
            ProductionDevStageError::owner("read the package tree", source.into())
        })?;
        let path = entry.path();
        if path
            .file_name()
            .is_some_and(|name| name == "generated" || name == "target" || name == ".sqlx")
        {
            continue;
        }
        let kind = entry.file_type().map_err(|source| {
            ProductionDevStageError::owner("read the package tree", source.into())
        })?;
        if kind.is_dir() {
            collect_authored_bytes(root, &path, inputs)?;
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|source| {
            ProductionDevStageError::owner("read an authored package file", source.into())
        })?;
        let relative = path.strip_prefix(root).unwrap_or(&path);
        inputs.push((
            relative.to_string_lossy().into_owned(),
            // Authored inputs are text. Encoding the bytes rather than the text
            // keeps a non-UTF-8 file from hashing to the same value as another.
            bytes.iter().fold(String::new(), |mut hex, byte| {
                use std::fmt::Write as _;
                write!(hex, "{byte:02x}").expect("writing to a string is infallible");
                hex
            }),
        ));
    }
    Ok(())
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "`DevStageRunner` declares its stage seam as `async fn`; the stages this \
              runner answers from already-read state still have to match the trait"
)]
impl DevStageRunner for ProductionDevStageRunner {
    type Error = ProductionDevStageError;

    fn reset(&mut self, from: DevStage) {
        self.read_publisher.reset(from);
    }

    fn stage_started(&mut self, stage: DevStage) {
        self.read_publisher.stage_started(stage);
    }

    fn stage_completed(&mut self, stage: DevStage) {
        // The digest is promoted only after the stage it describes succeeded.
        // Recording it earlier would let a failed Generate be skipped next run.
        if stage == DevStage::Generate {
            self.generate_input_digest = self.generate_input_candidate.take();
        }
        self.read_publisher.stage_completed(stage);
    }

    fn stage_skipped(&mut self, stage: DevStage) {
        self.read_publisher.stage_skipped(stage);
    }

    async fn stage_is_unchanged(&mut self, stage: DevStage) -> Result<bool, Self::Error> {
        match stage {
            DevStage::Migrate => {
                return Ok(self.schema_input_digest.is_some()
                    && self.schema_input_digest == self.schema_input_candidate);
            }
            DevStage::Introspect => {
                return Ok(self.catalogs.len() == self.package_inputs()?.len());
            }
            DevStage::Acl => {
                return Ok(self.acl_input_digest.as_deref()
                    == Some(self.generate_inputs_digest()?.as_str()));
            }
            _ => {}
        }
        if stage != DevStage::Generate {
            return Ok(false);
        }
        let digest = self.generate_inputs_digest()?;
        let outputs = generated_outputs_digest(&self.package_inputs()?)?;
        if outputs != "missing"
            && self.generate_input_digest.as_deref() == Some(digest.as_str())
            && self.generated_output_digest.as_deref() == Some(outputs.as_str())
        {
            return Ok(true);
        }
        self.generate_input_candidate = Some(digest);
        Ok(false)
    }

    fn stage_failed(&mut self, stage: DevStage, failure: DevStageFailure) {
        self.read_publisher.stage_failed(stage, failure);
    }

    fn classify_error(&self, error: &Self::Error) -> DevStageFailure {
        DevStageFailure::new(error.kind.as_str(), error.to_string(), None)
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
        self.packages = Some(
            super::config::resolve_dev_packages(&self.config, &self.overlay_root).map_err(
                |source| {
                    ProductionDevStageError::owner("resolve local package closure", source.into())
                },
            )?,
        );
        let packages = self.package_inputs()?;
        let digest = schema_inputs_digest(&packages, &self.config)?;
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
            .check()
            .await
            .map_err(|source| {
                ProductionDevStageError::owner("check local session lease", source.into())
            })?;
        if self.schema_input_digest.as_deref() != Some(digest.as_str()) {
            let structure = target_structure_digest(&packages, &self.config);
            // A target keeps its data while its structure is unchanged. The first
            // run of a session reads the structure that a previous session
            // recorded, and it also keeps the catalogs of an unchanged schema.
            let kept = match &self.target_instance {
                None => self
                    .target_lease
                    .as_ref()
                    .expect("lease acquired")
                    .retained_target(&self.config, &structure)
                    .await
                    .map(|(instance, recorded, catalogs)| {
                        (instance, (recorded == digest).then_some(catalogs))
                    }),
                Some(instance) => (self.target_structure_digest.as_deref()
                    == Some(structure.as_str()))
                .then(|| (instance.clone(), None)),
            };
            let kept = match kept {
                Some(kept) => {
                    let roots = packages
                        .iter()
                        .map(|package| package.root.clone())
                        .collect::<Vec<_>>();
                    let reason = apply_package::local_target_recreate_reason(
                        self.config.target_database_url(),
                        &self.config.activation_identity().tenant,
                        &roots,
                    )
                    .await
                    .map_err(|source| {
                        ProductionDevStageError::owner("check the kept local target", source)
                    })?;
                    if let Some(reason) = &reason {
                        eprintln!("wamn dev recreates the target: {reason}");
                    }
                    reason.is_none().then_some(kept)
                }
                None => None,
            };
            self.shutdown().await?;
            self.schema_input_digest = None;
            self.generate_input_digest = None;
            self.acl_input_digest = None;
            self.local_grants = None;
            self.catalogs.clear();
            let lease = self.target_lease.as_ref().expect("lease acquired");
            let instance = match kept {
                Some((instance, Some(catalogs))) => {
                    self.schema_input_digest = Some(digest.clone());
                    self.catalogs = catalogs;
                    instance
                }
                Some((instance, None)) => instance,
                None => lease.recreate(&self.config).await.map_err(|source| {
                    ProductionDevStageError::owner("recreate local schema target", source.into())
                })?,
            };
            claim_environment_instance(
                self.config.system_database_url(),
                &self.config.activation_identity().tenant,
                &instance,
            )
            .await?;
            self.target_instance = Some(instance);
            self.target_structure_digest = Some(structure);
        }
        self.schema_input_candidate = Some(digest);
        Ok(())
    }

    async fn run(&mut self, stage: DevStage) -> Result<(), Self::Error> {
        match stage {
            DevStage::Migrate => self.migrate().await,
            DevStage::Introspect => self.introspect().await,
            DevStage::Generate => self.generate().await,
            DevStage::Build => self.build().await,
            DevStage::Virtualize => self.virtualize().await,
            DevStage::Admit => self.admit().await,
            DevStage::Gate => self.gate().await,
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
    instance_id: String,
    #[serde(default)]
    instance: Option<wamn_control::bind_connection::LocalInstanceInput>,
}

#[derive(Debug)]
struct PreparedLocalBinding {
    requirement: wamn_catalog::ComponentConnectionRequirement,
    instance_id: String,
    instance: Option<wamn_control::bind_connection::PreparedLocalInstance>,
}

fn prepare_local_bindings(
    config: &DevConfig,
    admissions: &[ComponentAdmission],
) -> anyhow::Result<Vec<PreparedLocalBinding>> {
    let path = config.local_artifacts().bindings.as_ref();
    let selections: Vec<LocalBindingSelection> = path
        .map(|path| -> anyhow::Result<_> {
            serde_json::from_slice(&fs::read(path).context("read local connection selections")?)
                .context("parse strict local connection selections")
        })
        .transpose()?
        .unwrap_or_default();
    let requirements = admissions
        .iter()
        .flat_map(|admission| {
            admission
                .requirements()
                .iter()
                .map(move |requirement| (admission, requirement))
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        selections.len() == requirements.len(),
        "local components require exactly one explicit selection for every declared connection; set local_artifacts.bindings"
    );
    let mut prepared = Vec::new();
    let mut used = BTreeSet::new();
    for (admission, requirement) in requirements {
        let matches = selections
            .iter()
            .enumerate()
            .filter(|(_, selection)| {
                selection.package_id == admission.package_id()
                    && selection.component == admission.component()
                    && selection.store_alias == requirement.store_alias()
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            matches.len() == 1,
            "local connection selection is missing or repeated for {}::{}:{}",
            admission.package_id(),
            admission.component(),
            requirement.store_alias()
        );
        let (index, selection) = matches[0];
        anyhow::ensure!(
            used.insert(index),
            "local connection selection was used more than once"
        );
        let instance = selection
            .instance
            .as_ref()
            .map(|input| -> anyhow::Result<_> {
                anyhow::ensure!(
                    &input.requirement_type.descriptor() == requirement.requirement(),
                    "local instance type differs from the declared requirement"
                );
                wamn_control::bind_connection::read_local_instance(input)
            })
            .transpose()?;
        prepared.push(PreparedLocalBinding {
            requirement: requirement.clone(),
            instance_id: selection.instance_id.clone(),
            instance,
        });
    }
    Ok(prepared)
}

async fn resolve_local_bindings(
    config: &DevConfig,
    prepared: &[PreparedLocalBinding],
    apply: Option<&[wamn_runtime::local_application::LocalBindingFacts]>,
) -> anyhow::Result<Vec<wamn_runtime::local_application::LocalBindingFacts>> {
    wamn_runtime::local_application::require_local_target(
        config.target_database_url(),
        &config.activation_identity().tenant,
        &config.activation_identity().environment,
    )
    .await?;
    if prepared.is_empty() {
        return Ok(Vec::new());
    }
    let (client, connection) = tokio_postgres::connect(config.target_database_url(), NoTls).await?;
    let driver = tokio::spawn(connection);
    let result = async {
        client.batch_execute("BEGIN").await?;
        client
            .query_one(
                "SELECT set_config('app.tenant', $1, true)",
                &[&config.activation_identity().tenant],
            )
            .await?;
        let mut bindings = Vec::new();
        for binding in prepared {
            if let Some(input) = &binding.instance {
                wamn_control::bind_connection::prepare_local_instance(
                    &client,
                    &config.activation_identity().tenant,
                    &config.activation_identity().environment,
                    &binding.instance_id,
                    input,
                )
                .await?;
            }
            let selection = wamn_runtime::local_application::read_local_binding(
                &client,
                &config.activation_identity().tenant,
                &config.activation_identity().environment,
                &binding.requirement,
                &binding.instance_id,
            )
            .await?;
            bindings.push(wamn_runtime::local_application::LocalBindingFacts {
                requirement: binding.requirement.clone(),
                selection,
            });
        }
        if let Some(expected) = apply {
            anyhow::ensure!(
                bindings == expected,
                "local live authority changed after candidate validation"
            );
        }
        client
            .batch_execute(if apply.is_some() {
                "COMMIT"
            } else {
                "ROLLBACK"
            })
            .await?;
        Ok::<_, anyhow::Error>(bindings)
    }
    .await;
    drop(client);
    driver.abort();
    result
}

fn select_component_artifacts(
    packages: &[PackageInput],
    plan: &[ComponentArtifactPlan],
) -> Result<Vec<SelectedComponentArtifact>, ProductionDevStageError> {
    let mut selected = Vec::with_capacity(packages.len());
    let mut build_packages = BTreeSet::new();
    for package in packages {
        if package.manifest.components.len() != 1 {
            return Err(ProductionDevStageError::invalid(
                "select package component artifact",
                format!(
                    "{}@{} must declare exactly one component for the POC loop",
                    package.manifest.package.id, package.manifest.package.version
                ),
            ));
        }
        let component = package
            .manifest
            .components
            .keys()
            .next()
            .expect("one package component was required above");
        let build_package = canonical_component_build_package(component);
        if !build_packages.insert(build_package.clone()) {
            return Err(ProductionDevStageError::invalid(
                "select package component artifact",
                format!("more than one package derives build identity {build_package}"),
            ));
        }
        let matches = plan
            .iter()
            .filter(|artifact| artifact.package == build_package)
            .collect::<Vec<_>>();
        let [artifact] = matches.as_slice() else {
            return Err(ProductionDevStageError::invalid(
                "select package component artifact",
                format!(
                    "{}@{} component {} derived build package {} with {} artifact matches",
                    package.manifest.package.id,
                    package.manifest.package.version,
                    component,
                    build_package,
                    matches.len()
                ),
            ));
        };
        let bytes = fs::read(&artifact.output).map_err(|source| {
            ProductionDevStageError::owner(
                "read virtualized component output",
                anyhow!(source).context(format!("read {}", artifact.output.display())),
            )
        })?;
        if bytes.is_empty() {
            return Err(ProductionDevStageError::invalid(
                "read virtualized component output",
                format!("{} is empty", artifact.output.display()),
            ));
        }
        selected.push(SelectedComponentArtifact {
            package_id: package.manifest.package.id.clone().into_boxed_str(),
            package_version: package.manifest.package.version.clone().into_boxed_str(),
            component: component.clone().into_boxed_str(),
            path: artifact.output.clone(),
            digest: wamn_runtime::component_admission::component_digest(&bytes).into_boxed_str(),
        });
    }
    Ok(selected)
}

fn canonical_component_build_package(component: &str) -> String {
    component.replace('_', "-")
}

fn load_wirings(packages: &[PackageInput]) -> Result<Vec<WiringInput>, ProductionDevStageError> {
    let mut inputs = Vec::new();
    for package in packages {
        let directory = package.root.join(PACKAGE_WIRINGS);
        let entries = fs::read_dir(&directory).map_err(|source| {
            ProductionDevStageError::owner(
                "read package wiring directory",
                anyhow!(source).context(format!("read {}", directory.display())),
            )
        })?;
        let mut paths = entries
            .map(|entry| {
                entry.map(|entry| entry.path()).map_err(|source| {
                    ProductionDevStageError::owner("read package wiring entry", source.into())
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();
        for path in paths {
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let bytes = fs::read(&path).map_err(|source| {
                ProductionDevStageError::owner(
                    "read package wiring",
                    anyhow!(source).context(format!("read {}", path.display())),
                )
            })?;
            let document = serde_json::from_slice(&bytes).map_err(|source| {
                ProductionDevStageError::owner(
                    "parse package wiring",
                    anyhow!(source).context(format!("parse {}", path.display())),
                )
            })?;
            let wiring = WiringDocument::parse(&document).map_err(|source| {
                ProductionDevStageError::owner("validate package wiring", source.into())
            })?;
            inputs.push(WiringInput {
                package_id: package.manifest.package.id.clone().into_boxed_str(),
                package_version: package.manifest.package.version.clone().into_boxed_str(),
                document,
                wiring,
            });
        }
    }
    if inputs.is_empty() {
        return Err(ProductionDevStageError::invalid(
            "load package wirings",
            "the package closure declares no wiring documents",
        ));
    }
    Ok(inputs)
}

/// Stamp the creation the recreate just minted onto the tenant's projected
/// environment, so every control-plane fact this run writes keys to it
/// (wamn-10yt.52).
///
/// One short-lived connection to the control database, not a member of the
/// coordinator's state: this happens once per run, before any stage, and holding
/// a connection open across the whole loop for one UPDATE would outlive its
/// purpose. The routine refuses a tenant that provisioning never projected —
/// absence means durable, and a durable environment is never recreated.
pub(crate) async fn claim_environment_instance(
    system_database_url: &str,
    tenant: &str,
    instance: &str,
) -> Result<(), ProductionDevStageError> {
    let (client, connection) = tokio_postgres::connect(system_database_url, NoTls)
        .await
        .map_err(|source| {
            ProductionDevStageError::owner(
                "connect to the control database to claim the environment instance",
                source.into(),
            )
        })?;
    let connection_task = tokio::spawn(connection);
    let claimed =
        wamn_control::provision_project_env::claim_environment_instance(&client, tenant, instance)
            .await;
    drop(client);
    connection_task.abort();
    claimed
        .map_err(|source| ProductionDevStageError::owner("claim the environment instance", source))
}

/// `target_instance` names WHICH CREATION of the target database this command
/// runs against, and it is what makes a replay honest here (wamn-10yt.51).
///
/// An authoring claim is recorded in the control database, which no dev run
/// recreates, while its effect lands in the project database, which every run
/// drops and clones afresh. Without the instance, the second run replays, gets
/// the first run's result back, and writes nothing into a database that was
/// just emptied. The idempotency law is untouched: a recreated database is a
/// different target, so the same document against it is a different command.
/// A durable target passes None, because nothing recreates it.
fn authoring_command_id(
    command: &str,
    package_id: &str,
    package_version: &str,
    document: &Value,
    target_instance: Option<&str>,
) -> String {
    let mut identity = serde_json::json!({
        "command": command,
        "package-id": package_id,
        "package-version": package_version,
        "document": document,
    });
    if let Some(target_instance) = target_instance {
        identity["target-instance"] = Value::String(target_instance.to_owned());
    }
    format!(
        "wamn-dev-{command}-{}",
        wamn_execution_contract::canonical_json_sha256(&identity)
            .strip_prefix("sha256:")
            .expect("the canonical hash has its fixed prefix")
    )
}

/// Translate a failure to read authored base digests into its stage failure.
fn base_digests_stage_error(source: ComponentDeclarationError) -> ProductionDevStageError {
    const OPERATION: &str = "read authored base digests";
    match source.kind() {
        ComponentDeclarationErrorKind::Read | ComponentDeclarationErrorKind::Parse => {
            ProductionDevStageError::owner(OPERATION, source.into())
        }
        ComponentDeclarationErrorKind::ManifestInvalid
        | ComponentDeclarationErrorKind::TemplateInvalid => {
            ProductionDevStageError::invalid(OPERATION, source.to_string())
        }
    }
}

/// Translate a failure to render a component declaration into its stage failure.
fn declaration_stage_error(source: ComponentDeclarationError) -> ProductionDevStageError {
    match source.kind() {
        ComponentDeclarationErrorKind::Read => {
            ProductionDevStageError::owner("read component declaration template", source.into())
        }
        ComponentDeclarationErrorKind::Parse => {
            ProductionDevStageError::owner("parse component declaration template", source.into())
        }
        ComponentDeclarationErrorKind::ManifestInvalid
        | ComponentDeclarationErrorKind::TemplateInvalid => {
            ProductionDevStageError::invalid("render component declaration", source.to_string())
        }
    }
}

fn render_component_declaration(
    template: &Path,
    tenant: &str,
    base_digests: &BTreeMap<Box<str>, Box<str>>,
) -> Result<TemporaryFile, ProductionDevStageError> {
    let document = render_declaration_document(template, tenant, base_digests)
        .map_err(declaration_stage_error)?;
    let rendered = serde_json::to_vec(&document).map_err(|source| {
        ProductionDevStageError::owner("serialize component declaration", source.into())
    })?;
    TemporaryFile::write(&rendered).map_err(|source| {
        ProductionDevStageError::owner("write rendered component declaration", source)
    })
}

fn require_command_success(
    operation: &'static str,
    output: &std::process::Output,
) -> Result<(), ProductionDevStageError> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    Err(ProductionDevStageError::invalid(
        operation,
        if stderr.is_empty() {
            format!("command exited with {}", output.status)
        } else {
            format!("command exited with {}: {stderr}", output.status)
        },
    ))
}

#[derive(Debug)]
struct TemporaryFile(PathBuf);

impl TemporaryFile {
    fn write(bytes: &[u8]) -> anyhow::Result<Self> {
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

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_schema_introspection::ir::{
        Column, ColumnDefault, ColumnType, Constraint, Exclusion, ExclusionAccessMethod,
        ExclusionElement, ExclusionKey,
    };

    fn package(id: &str, version: &str, component: &str) -> PackageInput {
        let manifest = serde_json::from_value(serde_json::json!({
            "package": {"id": id, "version": version},
            "required_platform_policy_contract": {"id": "receiving_data_access", "state": "satisfied"},
            "models": {},
            "connections": {"postgres": {"interface": "wamn:postgres@0.1.0"}},
            "components": {(component): {"connections": ["postgres"]}}
        }))
        .expect("fixture package manifest");
        PackageInput {
            root: PathBuf::from(format!("/packages/{id}")),
            manifest,
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
            .context("PostgreSQL introspection refused (unsupported-acl) in schema receiving")
            .context("introspect package schemas");

        let error = ProductionDevStageError::owner("introspect package", source);

        assert_eq!(
            error.to_string(),
            "dev-stage-owner-failed while introspect package: introspect package schemas: \
             PostgreSQL introspection refused (unsupported-acl) in schema receiving: \
             schema has an explicit ACL"
        );
        assert_eq!(
            ProductionDevStageError::owner("introspect package", anyhow::Error::msg("one line"))
                .to_string(),
            "dev-stage-owner-failed while introspect package: one line",
            "a cause with no context keeps the line it always printed"
        );
    }

    /// The claim says which creation of the target database it ran against.
    ///
    /// Measured live on 2026-09-08: without this, a second run replayed all
    /// fourteen publish commands, wrote nothing, and Release refused with
    /// "has no wiring purchase_order_get version 1" against a database the run
    /// had just recreated. The control database held one audit row per command
    /// for two runs. A third run with NO edit failed identically, so the wall
    /// was the recreate and not the edit.
    #[test]
    fn a_recreated_target_is_a_different_command_and_a_durable_one_is_unchanged() {
        let document = serde_json::json!({"wiring": "purchase_order_get", "version": 1});

        let durable = authoring_command_id("gate", "wamn_receiving", "1.0.0", &document, None);
        let first_instance =
            authoring_command_id("gate", "wamn_receiving", "1.0.0", &document, Some("16394"));
        let same_instance =
            authoring_command_id("gate", "wamn_receiving", "1.0.0", &document, Some("16394"));
        let next_instance =
            authoring_command_id("gate", "wamn_receiving", "1.0.0", &document, Some("16512"));

        assert_eq!(
            first_instance, same_instance,
            "one creation of the target replays as one command"
        );
        assert_ne!(
            first_instance, next_instance,
            "a recreated database is a different target"
        );
        assert_ne!(
            durable, first_instance,
            "a durable target carries no instance, so its claims never move"
        );
    }

    #[test]
    fn local_schema_inputs_separate_contract_and_sql_changes_from_migrations() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/wamn_receiving");
        let directory = apply_package::read_package_directory(&root).unwrap();
        let mut manifest = PackageManifest::from_slice(&directory.manifest_bytes).unwrap();
        let original = package_schema_inputs(&manifest, &directory);
        manifest
            .models
            .get_mut("purchase_order")
            .unwrap()
            .operations
            .get_mut(&wamn_schema_generator::CrudAction::Get)
            .unwrap()
            .permission = "purchase_order.changed".to_owned();
        manifest.custom_operations.clear();
        assert_eq!(package_schema_inputs(&manifest, &directory), original);
        let mut changed = directory.clone();
        changed.migrations[0]
            .bytes
            .extend_from_slice(b"\n-- schema changed\n");
        assert_ne!(package_schema_inputs(&manifest, &changed), original);
        manifest.models.get_mut("purchase_order").unwrap().table = "replacement".to_owned();
        assert_ne!(package_schema_inputs(&manifest, &directory), original);
    }

    /// Owner rulings 3 and 5 of wamn-ri4b: the changes a column edit makes keep
    /// the target, and a structure change or a changed applied migration
    /// recreates it.
    #[test]
    fn a_column_edit_keeps_the_target_and_a_structure_change_recreates_it() {
        use wamn_schema_control::{
            AppliedPackage, MigrationSource, PackageDirectory, RecordedMigration,
        };

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/wamn_receiving");
        let directory = apply_package::read_package_directory(&root).unwrap();
        let manifest = PackageManifest::from_slice(&directory.manifest_bytes).unwrap();
        let plan = wamn_schema_control::plan_package_migrations(&directory, None).unwrap();
        let applied = AppliedPackage {
            coordinate: plan.coordinate,
            predecessor_version: plan.predecessor_version,
            manifest_sha256: plan.manifest_sha256,
            migrations: plan
                .pending
                .into_iter()
                .map(|migration| RecordedMigration {
                    ordinal: migration.ordinal,
                    relative_path: migration.relative_path,
                    sha256: migration.sha256,
                })
                .collect(),
        };
        // The decision of prepare_run for a target that applied the shipped package.
        let keeps = |changed: &PackageManifest, migrations: &PackageDirectory| {
            let presented = PackageDirectory {
                manifest_bytes: serde_json::to_vec(changed).unwrap(),
                migrations: migrations.migrations.clone(),
            };
            package_structure_inputs(changed) == package_structure_inputs(&manifest)
                && apply_package::applied_migration_drift(&presented, &applied)
                    .unwrap()
                    .is_none()
        };
        let edit = |change: fn(&mut PackageManifest)| {
            let mut changed = manifest.clone();
            change(&mut changed);
            changed
        };

        let mut appended = directory.clone();
        appended.migrations.push(MigrationSource {
            relative_path: "migrations/0002_location_description.sql".to_owned(),
            bytes: b"ALTER TABLE receiving.location ADD COLUMN description text NOT NULL DEFAULT 'not_required';".to_vec(),
        });
        assert!(keeps(&manifest, &appended), "an appended migration keeps");
        assert_ne!(
            package_schema_inputs(&manifest, &appended),
            package_schema_inputs(&manifest, &directory),
            "an appended migration runs Migrate on the kept target"
        );
        let mut edited = directory.clone();
        edited.migrations[0]
            .bytes
            .extend_from_slice(b"\n-- schema changed\n");
        assert!(!keeps(&manifest, &edited), "an edited migration recreates");
        for (kept, change) in [
            (
                "enum_fields",
                edit(|manifest| {
                    manifest
                        .models
                        .get_mut("purchase_order")
                        .unwrap()
                        .enum_fields
                        .insert(
                            "status".to_owned(),
                            vec!["open".to_owned(), "held".to_owned()],
                        );
                }),
            ),
            (
                "server_owned_fields",
                edit(|manifest| {
                    manifest
                        .models
                        .get_mut("purchase_order")
                        .unwrap()
                        .server_owned_fields
                        .pop();
                }),
            ),
            (
                "audit_log",
                edit(|manifest| {
                    manifest
                        .models
                        .get_mut("location")
                        .unwrap()
                        .audit_log
                        .as_mut()
                        .unwrap()
                        .retention = "unlimited".to_owned();
                }),
            ),
        ] {
            assert!(keeps(&change, &directory), "{kept} keeps");
        }
        for (recreated, change) in [
            (
                "field_owners",
                edit(|manifest| {
                    manifest
                        .models
                        .get_mut("purchase_order")
                        .unwrap()
                        .field_owners
                        .insert("status".to_owned(), "wamn_receiving".to_owned());
                }),
            ),
            (
                "constraint_owners",
                edit(|manifest| {
                    manifest
                        .models
                        .get_mut("purchase_order")
                        .unwrap()
                        .constraint_owners
                        .insert(
                            "purchase_order_status_check".to_owned(),
                            "wamn_receiving".to_owned(),
                        );
                }),
            ),
            (
                "client_field_extensible",
                edit(|manifest| {
                    manifest
                        .models
                        .get_mut("purchase_order")
                        .unwrap()
                        .client_field_extensible = false;
                }),
            ),
            (
                "a package version bump",
                edit(|manifest| {
                    manifest.package.version = "1.0.1".to_owned();
                }),
            ),
            (
                "a predecessor_version change",
                edit(|manifest| {
                    manifest.package.predecessor_version = Some("0.9.0".to_owned());
                }),
            ),
            (
                "a new model",
                edit(|manifest| {
                    let model = manifest.models["location"].clone();
                    manifest.models.insert("location_copy".to_owned(), model);
                }),
            ),
            (
                "a removed internal relation",
                edit(|manifest| {
                    manifest.internal_relations.clear();
                }),
            ),
        ] {
            assert!(!keeps(&change, &directory), "{recreated} recreates");
        }
    }

    #[test]
    fn generated_output_reuse_detects_deleted_corrupt_and_extra_files() {
        let root = std::env::temp_dir().join(format!(
            "wamn-output-reuse-{}-{}",
            std::process::id(),
            TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("generated/contracts")).unwrap();
        fs::create_dir_all(root.join("tests/.sqlx")).unwrap();
        fs::write(root.join("tests/.sqlx/query.json"), b"metadata").unwrap();
        let path = root.join("generated/contracts/operation.json");
        fs::write(&path, b"original").unwrap();
        let package = PackageInput {
            root: root.clone(),
            manifest: PackageManifest::from_slice(include_bytes!(
                "../../../../apps/wamn_receiving/wamn.json"
            ))
            .unwrap(),
        };
        let original = generated_outputs_digest(std::slice::from_ref(&package)).unwrap();
        fs::write(&path, b"corrupt").unwrap();
        assert_ne!(
            generated_outputs_digest(std::slice::from_ref(&package)).unwrap(),
            original
        );
        fs::write(&path, b"original").unwrap();
        fs::write(root.join("generated/contracts/extra.json"), b"extra").unwrap();
        assert_ne!(
            generated_outputs_digest(std::slice::from_ref(&package)).unwrap(),
            original
        );
        fs::remove_dir_all(root.join("generated")).unwrap();
        assert_ne!(generated_outputs_digest(&[package]).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn sqlx_preparation_skips_a_rust_only_edit_and_runs_for_emitted_sql() {
        let root = std::env::temp_dir().join(format!(
            "wamn-sqlx-inputs-{}-{}",
            std::process::id(),
            TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let package = root.join("apps/demo");
        let weld = include_bytes!("../../../../apps/wamn_receiving/generated/package-weld.json");
        for (path, bytes) in [
            (package.join(PACKAGE_WELD), weld.as_slice()),
            (package.join("component/src/lib.rs"), b"pub fn value() {}"),
            (
                root.join(RECORD_HISTORY_SQL),
                b"CREATE SCHEMA wamn_history;",
            ),
            (
                root.join("Cargo.lock"),
                b"[[package]]\nname = \"sqlx-macros-core\"\nversion = \"0.9.0\"\n",
            ),
        ] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "fixture Git command failed: {args:?}");
        };
        git(&["init", "--quiet"]);
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=SQLx Inputs Test",
            "-c",
            "user.email=sqlx-inputs@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ]);

        fs::write(
            package.join("component/src/lib.rs"),
            b"pub fn value() -> u8 { 1 }",
        )
        .unwrap();
        let rust_only = sqlx_metadata_inputs_on_disk(&root, &package).unwrap();
        assert!(
            sqlx_metadata_is_current(None, &root, &package, &rust_only)
                .await
                .unwrap(),
            "a Rust-only edit keeps the committed SQLx metadata"
        );

        let corpus = wamn_schema_generator::GeneratedPackageMetadata::from_slice(weld)
            .unwrap()
            .application_sql_corpus_identity()
            .to_owned();
        let emitted = String::from_utf8(weld.to_vec())
            .unwrap()
            .replace(&corpus, &format!("sha256:{}", "0".repeat(64)));
        fs::write(package.join(PACKAGE_WELD), emitted).unwrap();
        let sql = sqlx_metadata_inputs_on_disk(&root, &package).unwrap();
        assert!(
            !sqlx_metadata_is_current(None, &root, &package, &sql)
                .await
                .unwrap(),
            "emitted SQL that differs from the committed SQL prepares"
        );
        assert!(
            !sqlx_metadata_is_current(Some(&None), &root, &package, &sql)
                .await
                .unwrap(),
            "an unfinished preparation runs again"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_catalog_projection_keeps_base_contract_additive() {
        let base = PackageInput {
            root: PathBuf::from("/apps/wamn_receiving"),
            manifest: PackageManifest::from_slice(include_bytes!(
                "../../../../apps/wamn_receiving/wamn.json"
            ))
            .expect("parse shipped base manifest"),
        };
        let mut overlay = PackageInput {
            root: PathBuf::from("/apps/client_acme_receiving"),
            manifest: PackageManifest::from_slice(include_bytes!(
                "../../../../apps/client_acme_receiving/wamn.json"
            ))
            .expect("parse shipped overlay manifest"),
        };
        overlay
            .manifest
            .models
            .get_mut("purchase_order")
            .expect("overlay extends the base purchase order")
            .constraint_owners
            .insert(
                "purchase_order_acme_quality_status_excl".to_owned(),
                "client_acme_receiving".to_owned(),
            );
        let base_exclusion = Exclusion::new(
            "purchase_order_supplier_id_excl",
            ExclusionAccessMethod::Gist,
            vec![ExclusionKey::new(
                ExclusionElement::column("supplier_id"),
                "=",
            )],
            ["supplier_id"],
        )
        .expect("base exclusion");
        let overlay_exclusion = Exclusion::new(
            "purchase_order_acme_quality_status_excl",
            ExclusionAccessMethod::Gist,
            vec![ExclusionKey::new(
                ExclusionElement::column("acme_quality_status"),
                "=",
            )],
            ["acme_quality_status"],
        )
        .expect("overlay exclusion");
        let catalog = CatalogIr::new(vec![
            Table::new(
                "receiving",
                "purchase_order",
                vec![
                    Column::new("id", ColumnType::Uuid, false, None, None),
                    Column::new("supplier_id", ColumnType::Uuid, false, None, None),
                    Column::new("created_at", ColumnType::Timestamptz, false, None, None),
                    Column::new("created_by", ColumnType::Uuid, false, None, None),
                    Column::new("updated_at", ColumnType::Timestamptz, false, None, None),
                    Column::new("updated_by", ColumnType::Uuid, false, None, None),
                    Column::new(
                        "acme_inspection_required",
                        ColumnType::Boolean,
                        false,
                        Some(ColumnDefault::boolean(false)),
                        None,
                    ),
                    Column::new(
                        "acme_quality_status",
                        ColumnType::Text,
                        false,
                        Some(ColumnDefault::text("not_required")),
                        None,
                    ),
                ],
                vec![
                    Constraint::primary_key("purchase_order_id_pkey", ["id"])
                        .expect("base primary key"),
                    Constraint::check(
                        "purchase_order_acme_quality_status_check",
                        "acme_quality_status = ANY (ARRAY['not_required'::text, 'pending'::text, 'approved'::text])",
                    )
                    .expect("overlay quality constraint"),
                ],
                Vec::new(),
            )
            .with_exclusions(vec![base_exclusion.clone(), overlay_exclusion]),
            Table::new(
                "receiving",
                "quality_inspection",
                vec![Column::new(
                    "receipt_id",
                    ColumnType::Uuid,
                    false,
                    None,
                    None,
                )],
                vec![
                    Constraint::primary_key("quality_inspection_receipt_id_pkey", ["receipt_id"])
                        .expect("overlay primary key"),
                ],
                Vec::new(),
            ),
        ]);
        let installed = [base.clone(), overlay.clone()];

        let base_catalog = project_catalog_for_package(&catalog, &base.manifest, &installed)
            .expect("project base");
        assert_eq!(base_catalog.tables().len(), 1);
        let base_purchase_order = &base_catalog.tables()[0];
        assert_eq!(base_purchase_order.name(), "purchase_order");
        assert_eq!(
            base_purchase_order
                .columns()
                .iter()
                .map(wamn_schema_introspection::ir::Column::name)
                .collect::<Vec<_>>(),
            [
                "created_at",
                "created_by",
                "id",
                "supplier_id",
                "updated_at",
                "updated_by"
            ]
        );
        assert_eq!(
            base_purchase_order
                .constraints()
                .iter()
                .map(wamn_schema_introspection::ir::Constraint::name)
                .collect::<Vec<_>>(),
            ["purchase_order_id_pkey"]
        );
        assert_eq!(
            base_purchase_order
                .exclusions()
                .iter()
                .map(wamn_schema_introspection::ir::Exclusion::name)
                .collect::<Vec<_>>(),
            ["purchase_order_supplier_id_excl"]
        );

        let overlay_catalog = project_catalog_for_package(&catalog, &overlay.manifest, &installed)
            .expect("project overlay");
        assert_eq!(overlay_catalog.tables().len(), 2);
        let overlay_purchase_order = overlay_catalog
            .tables()
            .iter()
            .find(|table| table.name() == "purchase_order")
            .expect("overlay includes the extended base relation");
        assert_eq!(overlay_purchase_order.columns().len(), 8);
        assert_eq!(overlay_purchase_order.constraints().len(), 2);
        assert_eq!(overlay_purchase_order.exclusions().len(), 2);
        let clean_base = CatalogIr::new(vec![
            Table::new(
                "receiving",
                "purchase_order",
                vec![
                    Column::new("id", ColumnType::Uuid, false, None, None),
                    Column::new("supplier_id", ColumnType::Uuid, false, None, None),
                    Column::new("created_at", ColumnType::Timestamptz, false, None, None),
                    Column::new("created_by", ColumnType::Uuid, false, None, None),
                    Column::new("updated_at", ColumnType::Timestamptz, false, None, None),
                    Column::new("updated_by", ColumnType::Uuid, false, None, None),
                ],
                vec![Constraint::primary_key("purchase_order_id_pkey", ["id"]).unwrap()],
                Vec::new(),
            )
            .with_exclusions(vec![base_exclusion]),
        ]);
        let mut manifest = base.manifest.clone();
        manifest.models.retain(|name, _| name == "purchase_order");
        manifest.custom_operations.clear();
        manifest.internal_relations.clear();
        let model = manifest.models.get_mut("purchase_order").unwrap();
        model.server_owned_fields = vec!["id".to_owned()];
        model.enum_fields.clear();
        model
            .operations
            .retain(|action, _| *action == wamn_schema_generator::CrudAction::Get);
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let transactional = wamn_schema_generator::StatementTransactionality::unclassified();
        let generate = |catalog| {
            wamn_schema_generator::generate(&wamn_schema_generator::GenerationInput::new(
                catalog,
                &bytes,
                &[],
                wamn_schema_generator::GenerationProvenance::new("fixture", "fixture"),
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
    fn canonical_component_name_selects_one_artifact_per_package() {
        let directory = std::env::temp_dir().join(format!(
            "wamn-dev-selection-{}-{}",
            std::process::id(),
            TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).expect("create fixture directory");
        let base = directory.join("receiving.wasm");
        let overlay = directory.join("client-acme-receiving.wasm");
        fs::write(&base, b"base").expect("write base fixture");
        fs::write(&overlay, b"overlay").expect("write overlay fixture");
        let plan = vec![
            ComponentArtifactPlan {
                package: "client-acme-receiving".to_owned(),
                output: overlay,
            },
            ComponentArtifactPlan {
                package: "receiving".to_owned(),
                output: base,
            },
        ];
        let selected = select_component_artifacts(
            &[
                package("wamn_receiving", "1.0.0", "receiving"),
                package("client_acme_receiving", "3.0.0", "client_acme_receiving"),
            ],
            &plan,
        )
        .expect("canonical names select both artifacts");
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].component.as_ref(), "receiving");
        assert_eq!(selected[1].component.as_ref(), "client_acme_receiving");
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }
}

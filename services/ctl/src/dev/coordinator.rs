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
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;
use tokio::time::Instant;
use tokio_postgres::NoTls;
use wamn_authoring_model::{
    AuthoringScope, CommitProvenance, Gate, GateReceipt, PublishValidatedDraft,
    PublishedWiringIdentity,
};
use wamn_catalog::{PackageCoordinate, WiringDocument};
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
use super::verification_world::RUN_SCHEMA;
use super::watch::GitSource;
use super::{DevStage, DevStageFailure, DevStageRunner};
use crate::apply_package::ApplyPackageArgs;
use crate::dev_gate::GateClient;
use crate::print_release_env::{ReleaseCarrier, lookup_release_snapshot};
use crate::publish_release::{PublishReleaseArgs, ReleaseWiringTarget};
use crate::push_component::{
    AdmitComponentArgs, ComponentAdmissionReceipt, PublishAdmittedComponentArgs, admit_component,
    project_admitted_component_for_verification, publish_admitted_component,
};
use crate::push_release_manifest::PushReleaseManifestArgs;
use crate::reconcile_package_data_access::ReconcilePackageDataAccessArgs;

const BUILD_PROFILE: &str = "m1";
const BUILD_TOOL: &str = "tools/build-components";
const AUTHORING_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const COMPONENT_DECLARATION_PLACEHOLDER: &str = "__TENANT_ID__";
const PACKAGE_MANIFEST: &str = "wamn.json";
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
        let detail = source.to_string().into_boxed_str();
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
            detail: "the configured identity authority is unavailable".into(),
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
            .map(|column| column.name())
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
        tables.push(Table::new(
            table.schema(),
            table.name(),
            columns,
            constraints,
            indexes,
        ));
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

#[derive(Clone, Debug)]
struct GatedWiring {
    input: WiringInput,
    receipt: GateReceipt,
}

#[derive(Clone, Debug)]
struct PublishedWiring {
    input: WiringInput,
    receipt: PublishedWiringIdentity,
}

/// Concrete runner carrying production-owner outputs through all twelve stages.
pub struct ProductionDevStageRunner {
    config: DevConfig,
    overlay_root: PathBuf,
    git: GitSource,
    authoring: GateClient,
    packages: Option<ResolvedDevPackages>,
    catalogs: BTreeMap<String, CatalogIr>,
    build: Option<BuildStageOutput>,
    artifacts: Vec<SelectedComponentArtifact>,
    verified_base_digests: Vec<VerifiedBaseComponentDigest>,
    admissions: Vec<ComponentAdmissionReceipt>,
    gated_wirings: Vec<GatedWiring>,
    published_wirings: Vec<PublishedWiring>,
    publish_provenance: Option<CommitProvenance>,
    release: Option<ReleaseCarrier>,
    activation: Option<DevActivation>,
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
            .field("admission_count", &self.admissions.len())
            .field("gated_wiring_count", &self.gated_wirings.len())
            .field("published_wiring_count", &self.published_wirings.len())
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
    /// Bind one strict request to the production authoring endpoint and Git source.
    pub fn new(
        config: DevConfig,
        overlay_root: PathBuf,
        git: GitSource,
    ) -> Result<Self, ProductionDevStageError> {
        let authoring =
            GateClient::new(config.gate_url(), config.gate_bearer_token()).map_err(|source| {
                ProductionDevStageError::owner("construct the authoring client", source.into())
            })?;
        let (read_publisher, read_handle) = dev_read_channel();
        Ok(Self {
            config,
            overlay_root,
            git,
            authoring,
            packages: None,
            catalogs: BTreeMap::new(),
            build: None,
            artifacts: Vec::new(),
            verified_base_digests: Vec::new(),
            admissions: Vec::new(),
            gated_wirings: Vec::new(),
            published_wirings: Vec::new(),
            publish_provenance: None,
            release: None,
            activation: None,
            read_publisher,
            read_handle,
            observation_readers: None,
        })
    }

    /// Read-only state for terminal and future console clients.
    pub fn read_handle(&self) -> DevReadHandle {
        self.read_handle.clone()
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

    /// Stop any active workload and local host owned by this runner.
    pub async fn shutdown(&mut self) -> Result<(), ProductionDevStageError> {
        let Some(active) = self.activation.take() else {
            return Ok(());
        };
        active
            .shutdown()
            .await
            .map_err(|source| ProductionDevStageError::owner("clean up activation", source.into()))
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
            .expect("the verification bootstrap owns a valid run schema");
        crate::verification_policy::project_environment_policy(
            self.config.system_database_url(),
            self.config.verification_database_url(),
            &run_schema,
            &self.config.activation_identity().org,
            &self.config.activation_identity().tenant,
            &self.config.activation_identity().environment,
        )
        .await
        .map_err(|source| ProductionDevStageError::owner("project environment policy", source))?;

        for package in package_inputs {
            crate::apply_package::run(ApplyPackageArgs {
                package: package.root,
                database_url: self.config.verification_database_url().to_owned(),
                tenant: self.config.activation_identity().tenant.clone(),
            })
            .await
            .map_err(|source| {
                ProductionDevStageError::owner("apply package to verification", source)
            })?;
        }
        Ok(())
    }

    async fn introspect(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Introspect);
        let packages = self.package_inputs()?;
        for package in &packages {
            let catalog = wamn_schema_generator::introspect_package(
                self.config.verification_database_url(),
                &package.root,
            )
            .await
            .map_err(|source| ProductionDevStageError::owner("introspect package", source))?;
            let catalog = project_catalog_for_package(&catalog, &package.manifest, &packages)?;
            self.catalogs
                .insert(package.manifest.package.id.clone(), catalog);
        }
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
            // which rewrites the committed contracts and then fails the very
            // next stage on a dirty worktree (wamn-10yt.10.33). The narrowed
            // catalog above is passed through, because every package here
            // shares one verification database and re-introspecting would hand
            // this package the relations and fields its neighbours own.
            wamn_schema_generator::materialize_package_verified_with_catalog(
                MaterializeMode::Write,
                catalog,
                self.config.verification_database_url(),
                &package.root,
            )
            .await
            .map_err(|source| {
                ProductionDevStageError::owner("materialize generated package", source)
            })?;
        }
        Ok(())
    }

    async fn build(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Build);
        let tool = self.git.repository_root().join(BUILD_TOOL);
        let output = Command::new(&tool)
            .args(["build-only", BUILD_PROFILE])
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|source| {
                ProductionDevStageError::owner(
                    "start the production component build",
                    source.into(),
                )
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
        let output = Command::new(&tool)
            .arg("virtualize-only")
            .arg(plan_file.path())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|source| {
                ProductionDevStageError::owner(
                    "start the production component virtualizer",
                    source.into(),
                )
            })?;
        require_command_success("virtualize production components", &output)?;
        self.artifacts = select_component_artifacts(
            &self.package_inputs()?,
            &build.plan.virtualization.artifacts,
        )?;

        let packages = self.packages.as_ref().expect("package_inputs proved state");
        for base in packages.base_packages() {
            let artifact = self
                .artifacts
                .iter()
                .find(|artifact| {
                    artifact.package_id.as_ref() == base.manifest().package.id.as_str()
                })
                .expect("selected artifacts contain every resolved package");
            let verified = base
                .component_digest()
                .verify(artifact.digest.clone())
                .map_err(|source| {
                    ProductionDevStageError::owner(
                        "verify the built base component digest",
                        source.into(),
                    )
                })?;
            self.verified_base_digests.push(verified);
        }
        Ok(())
    }

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
            let declaration =
                render_component_declaration(&template, &self.config.activation_identity().tenant)?;
            let admission = admit_component(AdmitComponentArgs {
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
            project_admitted_component_for_verification(
                &admission,
                self.config.verification_database_url(),
            )
            .await
            .map_err(|source| {
                ProductionDevStageError::owner("project admission into verification", source)
            })?;
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
        let scope = self.authoring_scope();
        for input in load_wirings(&self.package_inputs()?)? {
            let command_id = authoring_command_id(
                "gate",
                &input.package_id,
                &input.package_version,
                &input.document,
                None,
            );
            let outcome = self
                .authoring
                .submit(
                    &command_id,
                    Gate {
                        scope: scope.clone(),
                        package_id: input.package_id.to_string(),
                        package_version: input.package_version.to_string(),
                        document: input.document.clone(),
                    },
                    Instant::now() + AUTHORING_REQUEST_TIMEOUT,
                )
                .await
                .map_err(|source| {
                    ProductionDevStageError::owner("submit production Gate", source.into())
                })?;
            match outcome {
                Ok(receipt) => {
                    read_outcomes.push(DevGateOutcome {
                        package_id: input.package_id.to_string(),
                        package_version: input.package_version.to_string(),
                        wiring_id: input.wiring.wiring_id.clone(),
                        wiring_version: input.wiring.version,
                        verdict: DevGateVerdict::Accepted(receipt.clone()),
                    });
                    self.read_publisher.set_gate_outcomes(read_outcomes.clone());
                    self.gated_wirings.push(GatedWiring { input, receipt });
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

    async fn publish(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Publish);
        if self.gated_wirings.is_empty() {
            return Err(ProductionDevStageError::invalid(
                "publish package wirings",
                "the Gate stage produced no accepted wiring",
            ));
        }
        if self
            .gated_wirings
            .iter()
            .any(|gated| gated.receipt.report_id.is_empty())
        {
            return Err(ProductionDevStageError::invalid(
                "publish package wirings",
                "the Gate stage returned an empty report identity",
            ));
        }

        let source = self.git.snapshot().await.map_err(|source| {
            ProductionDevStageError::owner("read publication source commit", source.into())
        })?;
        if source.state() != super::DevSourceState::Clean {
            return Err(ProductionDevStageError::invalid(
                "read publication source commit",
                "the source worktree became dirty before Publish",
            ));
        }
        let provenance = CommitProvenance {
            commit: source.source_commit().to_owned(),
            r#ref: None,
            dirty: false,
        };

        for admission in &self.admissions {
            publish_admitted_component(
                admission,
                PublishAdmittedComponentArgs {
                    artifact_base: self.config.component_artifact_base().to_owned(),
                    registry_auth_file: self.config.registry_auth_file().to_owned(),
                    insecure_registry: self.config.insecure_registry(),
                    project_database_url: self.config.verification_database_url().to_owned(),
                    control_database_url: self.config.system_database_url().to_owned(),
                },
            )
            .await
            .map_err(|source| {
                ProductionDevStageError::owner("publish admitted component", source)
            })?;
        }

        let scope = self.authoring_scope();
        for gated in self.gated_wirings.clone() {
            let input = gated.input;
            let command_id = authoring_command_id(
                "publish",
                &input.package_id,
                &input.package_version,
                &input.document,
                Some(&provenance.commit),
            );
            let outcome = self
                .authoring
                .publish(
                    &command_id,
                    PublishValidatedDraft {
                        scope: scope.clone(),
                        package_id: input.package_id.to_string(),
                        package_version: input.package_version.to_string(),
                        document: input.document.clone(),
                        provenance: Some(provenance.clone()),
                    },
                    Instant::now() + AUTHORING_REQUEST_TIMEOUT,
                )
                .await
                .map_err(|source| {
                    ProductionDevStageError::owner(
                        "publish wiring through the authenticated authoring endpoint",
                        source.into(),
                    )
                })?;
            let receipt = outcome.map_err(|refusal| {
                ProductionDevStageError::invalid(
                    "publish package wiring",
                    format!(
                        "{}@{}::{} was refused: {refusal:?}",
                        input.package_id, input.package_version, input.wiring.wiring_id
                    ),
                )
            })?;
            let expected_hash = input.wiring.wiring_hash();
            if receipt.wiring_id != input.wiring.wiring_id
                || receipt.version != input.wiring.version
                || receipt.artifact_hash != expected_hash.as_str()
            {
                return Err(ProductionDevStageError::invalid(
                    "carry published wiring identity",
                    format!(
                        "published identity {:?} differs from {}@{}::{}={} ({})",
                        receipt,
                        input.package_id,
                        input.package_version,
                        input.wiring.wiring_id,
                        input.wiring.version,
                        expected_hash.as_str()
                    ),
                ));
            }
            self.published_wirings
                .push(PublishedWiring { input, receipt });
        }
        self.publish_provenance = Some(provenance);
        Ok(())
    }

    async fn apply(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Apply);
        for package in self.package_inputs()? {
            crate::apply_package::run(ApplyPackageArgs {
                package: package.root,
                database_url: self.config.target_database_url().to_owned(),
                tenant: self.config.activation_identity().tenant.clone(),
            })
            .await
            .map_err(|source| {
                ProductionDevStageError::owner("apply package to the durable environment", source)
            })?;
        }
        Ok(())
    }

    async fn acl(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Acl);
        crate::reconcile_package_data_access::reconcile_package_data_access(
            ReconcilePackageDataAccessArgs {
                packages: self
                    .package_inputs()?
                    .into_iter()
                    .map(|package| package.root)
                    .collect(),
                database_url: self.config.target_database_url().to_owned(),
                tenant: self.config.activation_identity().tenant.clone(),
            },
        )
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("reconcile generated package data access", source)
        })?;
        Ok(())
    }

    async fn release(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Release);
        if self.published_wirings.is_empty() {
            return Err(ProductionDevStageError::invalid(
                "mint effective release",
                "the Publish stage produced no exact wiring identities",
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
            .published_wirings
            .iter()
            .map(|published| ReleaseWiringTarget {
                package_id: published.input.package_id.to_string(),
                package_version: published.input.package_version.to_string(),
                wiring_id: published.receipt.wiring_id.clone(),
                wiring_version: published.receipt.version,
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
        let source_commit = self
            .publish_provenance
            .as_ref()
            .ok_or_else(|| {
                ProductionDevStageError::invalid(
                    "mint effective release",
                    "the Publish stage produced no source provenance",
                )
            })?
            .commit
            .clone();
        crate::publish_release::run(PublishReleaseArgs {
            database_url: self.config.verification_database_url().to_owned(),
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
        })
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("mint the verified effective release", source)
        })?;

        crate::push_release_manifest::run_with_source_commit(
            PushReleaseManifestArgs {
                database_url: self.config.verification_database_url().to_owned(),
                org: identity.org.clone(),
                project: identity.project.clone(),
                tenant: identity.tenant.clone(),
                effective_release_id: self.config.effective_release_id(),
                artifact_base: self.config.release_artifact_base().to_owned(),
                registry_auth_file: self.config.registry_auth_file().to_owned(),
                insecure_registry: self.config.insecure_registry(),
                control_database_url: self.config.system_database_url().to_owned(),
            },
            &source_commit,
        )
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("publish the effective release manifest", source)
        })?;

        let release = lookup_release_snapshot(
            self.config.verification_database_url(),
            &identity.tenant,
            self.config.effective_release_id(),
            self.config.release_artifact_base(),
        )
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("load the exact release snapshot", source)
        })?;
        self.read_publisher
            .set_release(release.manifest, release.carrier.clone());
        self.release = Some(release.carrier);
        Ok(())
    }

    async fn activate(&mut self) -> Result<(), ProductionDevStageError> {
        self.clear_after(DevStage::Activate);
        let release = self.release.as_ref().ok_or_else(|| {
            ProductionDevStageError::invalid(
                "activate local runtime",
                "the Release stage produced no carrier",
            )
        })?;
        let activation = activation::activate(DevActivationRequest {
            config: &self.config,
            release,
            identity: self.config.activation_identity(),
            host_binary: self.config.host_binary(),
            wasmtime_cache_dir: self.config.wasmtime_cache_dir(),
        })
        .await
        .map_err(|source| {
            ProductionDevStageError::owner("activate local host and flow-http", source.into())
        })?;
        self.read_publisher
            .set_runtime_endpoint(DevRuntimeEndpoint::new(
                activation.http_base_url(),
                self.config.route_host(),
            ));
        self.activation = Some(activation);
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

    fn authoring_scope(&self) -> AuthoringScope {
        AuthoringScope {
            project_id: self.config.activation_identity().project.clone(),
            environment: self.config.activation_identity().environment.clone(),
        }
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
                self.published_wirings.clear();
                self.publish_provenance = None;
                self.release = None;
            }
            DevStage::Generate | DevStage::Build => {
                self.build = None;
                self.artifacts.clear();
                self.verified_base_digests.clear();
                self.admissions.clear();
                self.gated_wirings.clear();
                self.published_wirings.clear();
                self.publish_provenance = None;
                self.release = None;
            }
            DevStage::Virtualize => {
                self.artifacts.clear();
                self.verified_base_digests.clear();
                self.admissions.clear();
                self.gated_wirings.clear();
                self.published_wirings.clear();
                self.publish_provenance = None;
                self.release = None;
            }
            DevStage::Admit => {
                self.admissions.clear();
                self.gated_wirings.clear();
                self.published_wirings.clear();
                self.publish_provenance = None;
                self.release = None;
            }
            DevStage::Gate => {
                self.gated_wirings.clear();
                self.published_wirings.clear();
                self.publish_provenance = None;
                self.release = None;
            }
            DevStage::Publish => {
                self.published_wirings.clear();
                self.publish_provenance = None;
                self.release = None;
            }
            DevStage::Apply | DevStage::Acl | DevStage::Release => {
                self.release = None;
            }
            DevStage::Activate => {}
        }
    }
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

    fn stage_failed(&mut self, stage: DevStage, failure: DevStageFailure) {
        self.read_publisher.stage_failed(stage, failure);
    }

    fn classify_error(&self, error: &Self::Error) -> DevStageFailure {
        DevStageFailure::new(error.kind.as_str(), error.to_string(), None)
    }

    async fn run(&mut self, stage: DevStage) -> Result<(), Self::Error> {
        if self.activation.is_some() {
            self.shutdown().await?;
        }
        match stage {
            DevStage::Migrate => self.migrate().await,
            DevStage::Introspect => self.introspect().await,
            DevStage::Generate => self.generate().await,
            DevStage::Build => self.build().await,
            DevStage::Virtualize => self.virtualize().await,
            DevStage::Admit => self.admit().await,
            DevStage::Gate => self.gate().await,
            DevStage::Publish => self.publish().await,
            DevStage::Apply => self.apply().await,
            DevStage::Acl => self.acl().await,
            DevStage::Release => self.release().await,
            DevStage::Activate => self.activate().await,
        }
    }
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

fn authoring_command_id(
    command: &str,
    package_id: &str,
    package_version: &str,
    document: &Value,
    source_commit: Option<&str>,
) -> String {
    let mut identity = serde_json::json!({
        "command": command,
        "package-id": package_id,
        "package-version": package_version,
        "document": document,
    });
    if let Some(source_commit) = source_commit {
        identity["source-commit"] = Value::String(source_commit.to_owned());
    }
    format!(
        "wamn-dev-{command}-{}",
        wamn_execution_contract::canonical_json_sha256(&identity)
            .strip_prefix("sha256:")
            .expect("the canonical hash has its fixed prefix")
    )
}

fn render_component_declaration(
    template: &Path,
    tenant: &str,
) -> Result<TemporaryFile, ProductionDevStageError> {
    let bytes = fs::read(template).map_err(|source| {
        ProductionDevStageError::owner(
            "read component declaration template",
            anyhow!(source).context(format!("read {}", template.display())),
        )
    })?;
    let mut document: Value = serde_json::from_slice(&bytes).map_err(|source| {
        ProductionDevStageError::owner(
            "parse component declaration template",
            anyhow!(source).context(format!("parse {}", template.display())),
        )
    })?;
    let slot = document.pointer_mut("/scope/tenant-id").ok_or_else(|| {
        ProductionDevStageError::invalid(
            "render component declaration",
            format!("{} has no scope.tenant-id", template.display()),
        )
    })?;
    if slot.as_str() != Some(COMPONENT_DECLARATION_PLACEHOLDER) {
        return Err(ProductionDevStageError::invalid(
            "render component declaration",
            format!(
                "{} must leave scope.tenant-id as the deployment placeholder",
                template.display()
            ),
        ));
    }
    *slot = Value::String(tenant.to_owned());
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
    use wamn_schema_introspection::ir::{Column, ColumnDefault, ColumnType, Constraint};

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

    #[test]
    fn command_identity_is_stable_and_publish_separates_source_commits() {
        let document = serde_json::json!({"wiring-id": "purchase_order_get", "version": 1});
        let first_gate = authoring_command_id("gate", "wamn_receiving", "1.0.0", &document, None);
        let second_gate = authoring_command_id("gate", "wamn_receiving", "1.0.0", &document, None);
        let first_publish = authoring_command_id(
            "publish",
            "wamn_receiving",
            "1.0.0",
            &document,
            Some("0123456789abcdef"),
        );
        let repeated_publish = authoring_command_id(
            "publish",
            "wamn_receiving",
            "1.0.0",
            &document,
            Some("0123456789abcdef"),
        );
        let next_commit_publish = authoring_command_id(
            "publish",
            "wamn_receiving",
            "1.0.0",
            &document,
            Some("fedcba9876543210"),
        );
        assert_eq!(first_gate, second_gate);
        assert_eq!(first_publish, repeated_publish);
        assert_ne!(first_gate, first_publish);
        assert_ne!(first_publish, next_commit_publish);
    }

    #[test]
    fn package_catalog_projection_keeps_base_contract_additive() {
        let base = PackageInput {
            root: PathBuf::from("/packages/receiving"),
            manifest: PackageManifest::from_slice(include_bytes!(
                "../../../../packages/receiving/wamn.json"
            ))
            .expect("parse shipped base manifest"),
        };
        let overlay = PackageInput {
            root: PathBuf::from("/packages/client_acme_receiving"),
            manifest: PackageManifest::from_slice(include_bytes!(
                "../../../../packages/client_acme_receiving/wamn.json"
            ))
            .expect("parse shipped overlay manifest"),
        };
        let catalog = CatalogIr::new(vec![
            Table::new(
                "receiving",
                "purchase_order",
                vec![
                    Column::new("id", ColumnType::Uuid, false, None, None),
                    Column::new("supplier_id", ColumnType::Uuid, false, None, None),
                    Column::new(
                        "acme_inspection_required",
                        ColumnType::Boolean,
                        false,
                        Some(ColumnDefault::BooleanFalse),
                        None,
                    ),
                    Column::new(
                        "acme_quality_status",
                        ColumnType::Text,
                        false,
                        Some(ColumnDefault::TextNotRequired),
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
            ),
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
                .map(|column| column.name())
                .collect::<Vec<_>>(),
            ["id", "supplier_id"]
        );
        assert_eq!(
            base_purchase_order
                .constraints()
                .iter()
                .map(|constraint| constraint.name())
                .collect::<Vec<_>>(),
            ["purchase_order_id_pkey"]
        );

        let overlay_catalog = project_catalog_for_package(&catalog, &overlay.manifest, &installed)
            .expect("project overlay");
        assert_eq!(overlay_catalog.tables().len(), 2);
        let overlay_purchase_order = overlay_catalog
            .tables()
            .iter()
            .find(|table| table.name() == "purchase_order")
            .expect("overlay includes the extended base relation");
        assert_eq!(overlay_purchase_order.columns().len(), 4);
        assert_eq!(overlay_purchase_order.constraints().len(), 2);
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

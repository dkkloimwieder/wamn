//! Admit exact component bytes, and publish one digest-addressed OCI layer.
//!
//! This is the sole production caller of component byte admission. It validates
//! the local bytes against their package and declaration. `push-package` pushes
//! each admitted component with [`publish_and_verify`] and pulls it back.

pub(super) mod plan;
pub use plan::portable_requirement;
pub use plan::require_admitted_statements;
use plan::{
    bind_signed_file_results, ensure_component_package_matches, expected_operation_contracts,
    host_transaction_exports, require_statement_sql, validate_component_operation_assignment,
    validate_generated_statement_shape,
};

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component as PathComponent, Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use oci_client::client::{Config, ImageLayer};
use oci_client::manifest::OciImageManifest;
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use serde::Deserialize;
use wamn_catalog::ComponentConnectionRequirement;
use wamn_catalog::ConnectionTypeDescriptor;
use wamn_catalog::package_schema::package_metadata;
use wamn_catalog::{
    AdmittedComponent, ComponentConnection, ComponentConnectionType, ComponentDeclaration,
    ComponentDescriptor, ComponentSqlField, ComponentSqlStatement, bind_component_statement_facts,
};
use wamn_component_artifact::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_engine::artifact_source::ArtifactSource as _;
use wamn_engine::component_admission::validate_component_admission;
use wamn_engine::component_artifact::component_artifact_layout;
use wamn_package_artifact::read_ca_bundles;
use wamn_package_artifact::registry_credentials::RegistryCredentials;

use wamn_compiler::declared_sql;

/// Bound each registry connect/read phase without adding a second deployment knob.
const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Stable prefix for package/component projection refusals.
pub const COMPONENT_PROJECTION_REFUSAL: &str = "component-projection-refused";

/// Remedy-distinct projector refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentProjectionErrorType {
    PackageCoordinateMismatch,
    StatementContractInvalid,
    StatementOperationMismatch,
    StatementComponentMismatch,
    StatementPathInvalid,
    StatementCorpusMismatch,
    StatementRefused,
}

impl ComponentProjectionErrorType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PackageCoordinateMismatch => "package-coordinate-mismatch",
            Self::StatementContractInvalid => "statement-contract-invalid",
            Self::StatementOperationMismatch => "statement-operation-mismatch",
            Self::StatementComponentMismatch => "statement-component-mismatch",
            Self::StatementPathInvalid => "statement-path-invalid",
            Self::StatementCorpusMismatch => "statement-corpus-mismatch",
            Self::StatementRefused => "statement-refused",
        }
    }
}

/// Contextual refusal from component admission.
#[derive(Debug)]
pub struct ComponentProjectionError {
    type_: ComponentProjectionErrorType,
    detail: String,
}

impl ComponentProjectionError {
    fn new(type_: ComponentProjectionErrorType, detail: impl Into<String>) -> Self {
        Self {
            type_,
            detail: detail.into(),
        }
    }

    pub const fn error_type(&self) -> ComponentProjectionErrorType {
        self.type_
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for ComponentProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{COMPONENT_PROJECTION_REFUSAL} ({}): {}",
            self.type_.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for ComponentProjectionError {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratedStatementContract {
    name: String,
    path: String,
    binds: Vec<ComponentSqlField>,
    columns: Vec<ComponentSqlField>,
    relations: Vec<wamn_catalog::profile::RelationProfile>,
    /// PostgreSQL's own verdict from the generic plan at generation time.
    transactional: bool,
}

/// Source-owned inputs for one exact component admission.
#[derive(Debug)]
pub struct AdmitComponentRequest {
    /// Package root whose strict manifest and generated evidence own the component.
    pub package: PathBuf,
    /// Exact wasm component bytes to admit.
    pub component_bytes: PathBuf,
    /// Package-owned component declaration to bind to the bytes.
    pub declaration: PathBuf,
    /// Closed platform-capability set granted to these exact bytes.
    pub admitted_platform_packages: Vec<String>,
    /// The component digest of each base the package pins: the authored
    /// `base_dependencies` digests, with a base this run built layered over
    /// its pin for a disposable target.
    pub base_digests: BTreeSet<String>,
}

/// Exact saved-byte admission reused by later publication.
///
/// Fields stay private so callers cannot replace admitted facts, package
/// evidence, or component bytes independently. The admission deliberately does
/// not implement `Clone`; retries borrow the same admission.
pub struct ComponentAdmission {
    component_bytes: Box<[u8]>,
    component: AdmittedComponent,
    connections: Box<[ComponentConnection]>,
    requirements: Box<[ComponentConnectionRequirement]>,
    statement_instances: BTreeMap<String, String>,
}

impl fmt::Debug for ComponentAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComponentAdmission")
            .field("package_id", &self.component.target.package_id)
            .field("package_version", &self.component.target.package_version)
            .field("component", &self.component.component)
            .field("component_digest", &self.component.component_digest)
            .field("component_bytes_len", &self.component_bytes.len())
            .field("requirement_count", &self.requirements.len())
            .finish_non_exhaustive()
    }
}

impl ComponentAdmission {
    /// Exact saved bytes, for publish-time application composition.
    pub fn bytes(&self) -> &[u8] {
        &self.component_bytes
    }

    pub fn facts(&self) -> &AdmittedComponent {
        &self.component
    }

    pub fn requirements(&self) -> &[ComponentConnectionRequirement] {
        &self.requirements
    }

    /// Each named `statements` import and its member's digest, as admission
    /// verified them in the bytes.
    pub fn statement_instances(&self) -> &BTreeMap<String, String> {
        &self.statement_instances
    }

    /// The admitted descriptor (docs/operations/building.md#reading-a-pushed-package
    /// ): the facts without their target, and the declared connections sorted
    /// by store alias, as `push-package` writes it.
    pub fn descriptor(&self) -> ComponentDescriptor {
        ComponentDescriptor::new(self.component.clone(), self.connections.to_vec())
            .with_statement_instances(self.statement_instances.clone())
    }

    /// Exact package identity carried by the admitted component facts.
    pub fn package_id(&self) -> &str {
        &self.component.target.package_id
    }

    /// Exact package version carried by the admitted component facts.
    pub fn package_version(&self) -> &str {
        &self.component.target.package_version
    }

    /// Package-local component name bound to the admitted bytes.
    pub fn component(&self) -> &str {
        &self.component.component
    }

    /// SHA-256 identity derived from the exact admitted bytes.
    pub fn component_digest(&self) -> &str {
        &self.component.component_digest
    }
}

/// Validate saved component and package bytes without publication side effects.
pub fn admit_component(args: AdmitComponentRequest) -> anyhow::Result<ComponentAdmission> {
    let directory = crate::package_source::read_package_directory(&args.package)?;
    let package = package_metadata(&directory)
        .context("validate strict package directory before component publication")?;
    let package_manifest =
        wamn_compiler::manifest::PackageManifest::from_slice(&directory.manifest_bytes)
            .context("parse strict package manifest before component publication")?;
    let component_bytes = std::fs::read(&args.component_bytes)
        .with_context(|| format!("read component bytes {}", args.component_bytes.display()))?;
    wamn_component_composer::validate_application_member(&component_bytes)
        .context("refuse application-owned entry authority")?;
    let declaration_bytes = std::fs::read(&args.declaration)
        .with_context(|| format!("read component declaration {}", args.declaration.display()))?;
    let document: serde_json::Value = serde_json::from_slice(&declaration_bytes)
        .with_context(|| format!("parse component declaration {}", args.declaration.display()))?;
    let declaration: ComponentDeclaration = serde_json::from_value(document)
        .with_context(|| format!("parse component declaration {}", args.declaration.display()))?;

    let engine = wamn_engine::build_engine(&[]).context("build component admission engine")?;
    let facts = validate_component_admission(
        &engine,
        &component_bytes,
        wamn_engine::component_admission::ComponentAdmissionRequest {
            declaration,
            admitted_platform_packages: args
                .admitted_platform_packages
                .into_iter()
                .collect::<BTreeSet<_>>(),
            base_digests: args.base_digests,
        },
    )
    .context("validate exact component bytes")?;
    let mut admitted = facts.component;
    ensure_component_package_matches(&admitted, &package.coordinate)?;
    let statement_facts =
        load_component_statement_facts(&args.package, &package_manifest, &admitted)?;
    let host_transactions = host_transaction_exports(&package_manifest, &statement_facts)?;
    bind_component_statement_facts(&mut admitted, statement_facts, &host_transactions)
        .context("bind generated SQL facts to exact component operations")?;
    bind_signed_file_results(&mut admitted, &package_manifest)
        .context("bind package signed file results")?;
    let requirements = facts
        .connections
        .iter()
        .map(|connection| {
            let alias = connection
                .store_alias
                .name()
                .context("package admission requires concrete connection aliases")?;
            Ok(portable_requirement(
                &admitted.component_digest,
                connection,
                alias,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut connections = facts.connections;
    for connection in &mut connections {
        let alias = connection
            .store_alias
            .name()
            .context("package connection needs a fixed alias")?;
        let declaration = package_manifest
            .connections
            .get(alias)
            .with_context(|| format!("package declares no connection {alias}"))?;
        connection.mode = match declaration.mode {
            wamn_compiler::manifest::ConnectionMode::Read => wamn_catalog::ConnectionMode::Read,
            wamn_compiler::manifest::ConnectionMode::Write => wamn_catalog::ConnectionMode::Write,
        };
    }
    connections.sort_by(|left, right| left.store_alias.cmp(&right.store_alias));

    Ok(ComponentAdmission {
        component_bytes: component_bytes.into_boxed_slice(),
        component: admitted,
        connections: connections.into_boxed_slice(),
        requirements: requirements.into_boxed_slice(),
        statement_instances: facts.statement_instances,
    })
}

#[derive(Debug)]
struct ExpectedOperationContract {
    local_operation: String,
    component: String,
    public: bool,
}

fn load_component_statement_facts(
    package_root: &Path,
    manifest: &wamn_compiler::manifest::PackageManifest,
    component: &AdmittedComponent,
) -> Result<BTreeMap<String, BTreeMap<String, ComponentSqlStatement>>, ComponentProjectionError> {
    load_package_statements(package_root, manifest, Some(component))
}

/// Load every manifest operation's exact statement facts without component publication.
#[cfg(any(test, feature = "test-util"))]
pub fn load_package_statement_facts(
    package_root: &Path,
    manifest: &wamn_compiler::manifest::PackageManifest,
) -> Result<BTreeMap<String, BTreeMap<String, ComponentSqlStatement>>, ComponentProjectionError> {
    load_package_statements(package_root, manifest, None)
}

fn load_package_statements(
    package_root: &Path,
    manifest: &wamn_compiler::manifest::PackageManifest,
    component: Option<&AdmittedComponent>,
) -> Result<BTreeMap<String, BTreeMap<String, ComponentSqlStatement>>, ComponentProjectionError> {
    let expected = expected_operation_contracts(manifest)?;
    let expected_component_operations = component
        .map(|component| validate_component_operation_assignment(manifest, component, &expected))
        .transpose()?;
    let canonical_root = package_root.canonicalize().map_err(|error| {
        ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementPathInvalid,
            format!(
                "cannot resolve package root {} while reading statement evidence: {error}",
                package_root.display()
            ),
        )
    })?;
    let mut all_statements = BTreeMap::new();
    let mut corpus = BTreeMap::<String, Vec<u8>>::new();
    for (operation, expected_contract) in &expected {
        let description = manifest
            .compiled_operations
            .get(&expected_contract.local_operation)
            .ok_or_else(|| {
                ComponentProjectionError::new(
                    ComponentProjectionErrorType::StatementContractInvalid,
                    format!("operation {operation} has no compiled description"),
                )
            })?;
        if description.operation.operation != *operation {
            return Err(ComponentProjectionError::new(
                ComponentProjectionErrorType::StatementOperationMismatch,
                format!("compiled operation {operation} has a different identity"),
            ));
        }
        let statements = description
            .operation
            .statements
            .iter()
            .map(|statement| GeneratedStatementContract {
                name: statement.name.clone(),
                path: statement.path.clone(),
                binds: statement
                    .binds
                    .iter()
                    .map(|field| ComponentSqlField {
                        name: field.name.clone(),
                        value_type: field.ty,
                        nullable: field.nullable,
                    })
                    .collect(),
                columns: statement
                    .columns
                    .iter()
                    .map(|field| ComponentSqlField {
                        name: field.name.clone(),
                        value_type: field.ty,
                        nullable: field.nullable,
                    })
                    .collect(),
                relations: statement.relations.clone(),
                transactional: statement.transactional,
            })
            .collect();
        let operation_statements = load_operation_statements(
            package_root,
            &canonical_root,
            operation,
            statements,
            &mut corpus,
        )?;
        all_statements.insert(operation.clone(), operation_statements);
    }

    let Some(expected_component_operations) = expected_component_operations else {
        return Ok(all_statements);
    };
    let mut selected = BTreeMap::new();
    for operation in expected_component_operations {
        selected.insert(
            operation.clone(),
            all_statements
                .remove(&operation)
                .expect("every expected operation contract was loaded"),
        );
    }
    Ok(selected)
}

fn load_operation_statements(
    package_root: &Path,
    canonical_root: &Path,
    operation: &str,
    statements: Vec<GeneratedStatementContract>,
    corpus: &mut BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, ComponentSqlStatement>, ComponentProjectionError> {
    let mut admitted = BTreeMap::new();
    let mut names = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for statement in statements {
        validate_generated_statement_shape(operation, &statement)?;
        if !names.insert(statement.name.clone()) || !paths.insert(statement.path.clone()) {
            return Err(ComponentProjectionError::new(
                ComponentProjectionErrorType::StatementContractInvalid,
                format!("operation {operation:?} repeats a statement name or path"),
            ));
        }
        let bytes = read_package_owned_file(
            package_root,
            canonical_root,
            &format!("operation {operation:?} statement"),
            &statement.path,
            "sql",
        )?;
        let sql = std::str::from_utf8(&bytes)
            .map(str::to_owned)
            .map_err(|error| {
                ComponentProjectionError::new(
                    ComponentProjectionErrorType::StatementContractInvalid,
                    format!(
                        "operation {operation:?} statement {:?} is not UTF-8 SQL: {error}",
                        statement.name
                    ),
                )
            })?;
        require_statement_sql(operation, &statement.name, &sql)?;
        let fact = ComponentSqlStatement {
            name: statement.name,
            path: statement.path.clone(),
            sql,
            binds: statement.binds,
            columns: statement.columns,
            relations: statement.relations,
            transactional: statement.transactional,
        };
        admitted.insert(statement.path.clone(), fact);
        if let Some(existing) = corpus.get(&statement.path) {
            if existing != &bytes {
                return Err(ComponentProjectionError::new(
                    ComponentProjectionErrorType::StatementCorpusMismatch,
                    format!(
                        "package SQL path {:?} resolves to different bytes across operation contracts",
                        statement.path
                    ),
                ));
            }
        } else {
            corpus.insert(statement.path, bytes);
        }
    }
    Ok(admitted)
}

fn read_package_owned_file(
    package_root: &Path,
    canonical_root: &Path,
    subject: &str,
    relative_path: &str,
    extension: &str,
) -> Result<Vec<u8>, ComponentProjectionError> {
    let path = Path::new(relative_path);
    if path.as_os_str().is_empty()
        || path
            .extension()
            .is_none_or(|observed| observed != extension)
        || relative_path.contains('\\')
        || !path
            .components()
            .all(|part| matches!(part, PathComponent::Normal(_)))
        || path
            .components()
            .filter_map(|part| match part {
                PathComponent::Normal(part) => part.to_str(),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/")
            != relative_path
    {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementPathInvalid,
            format!(
                "{subject} path {relative_path:?} is not a canonical package-relative .{extension} path"
            ),
        ));
    }
    // A `generated/` path names a file of the package's output root, which
    // lies outside the package. It must stay inside that output root.
    let (candidate, owner) = match path.strip_prefix("generated") {
        Ok(output) => {
            let output_root = wamn_compiler::output_root(package_root);
            let owner = output_root.canonicalize().map_err(|error| {
                ComponentProjectionError::new(
                    ComponentProjectionErrorType::StatementPathInvalid,
                    format!(
                        "{subject} cannot resolve the build output of {relative_path:?}: {error}"
                    ),
                )
            })?;
            (output_root.join(output), owner)
        }
        Err(_) if package_root.join("generated").is_dir() => {
            (package_root.join(path), canonical_root.to_owned())
        }
        Err(_) => {
            let output = wamn_compiler::output_root(package_root);
            let owner = output.canonicalize().map_err(|source| {
                ComponentProjectionError::new(
                    ComponentProjectionErrorType::StatementPathInvalid,
                    source.to_string(),
                )
            })?;
            (output.join(path), owner)
        }
    };
    let metadata = std::fs::symlink_metadata(&candidate).map_err(|error| {
        ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementPathInvalid,
            format!("{subject} cannot inspect path {relative_path:?}: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementPathInvalid,
            format!("{subject} path {relative_path:?} is not a package-owned regular file"),
        ));
    }
    let canonical_candidate = candidate.canonicalize().map_err(|error| {
        ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementPathInvalid,
            format!("{subject} cannot resolve path {relative_path:?}: {error}"),
        )
    })?;
    if !canonical_candidate.starts_with(&owner) {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementPathInvalid,
            format!("{subject} path {relative_path:?} escapes the package"),
        ));
    }
    std::fs::read(&canonical_candidate).map_err(|error| {
        ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementPathInvalid,
            format!("{subject} cannot read path {relative_path:?}: {error}"),
        )
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the push and its verification pull share one registry's transport facts"
)]
pub async fn publish_and_verify(
    reference: &Reference,
    artifact_base: &str,
    insecure: bool,
    oci_ca_paths: &[PathBuf],
    component_bytes: &[u8],
    config_bytes: &[u8],
    component: &AdmittedComponent,
    credentials: Option<&RegistryCredentials>,
) -> anyhow::Result<()> {
    // Built here rather than borrowed from `wash_runtime::oci`, which exposes no
    // client-construction seam. Its `push_component` mints the config blob
    // itself from `WasmConfig` and tags the layer `WASM_LAYER_MEDIA_TYPE`; this
    // artifact carries the platform's own `component_artifact_layout`, whose
    // config blob is the admitted-component fact `pull_verified` verifies again
    // below. Routing through that API would discard the admitted fact. See
    // `docs/architecture/native-alignment.md#retained-wamn-implementations`
    // (`wamn-kdhw`).
    let (client, auth) = registry_client(reference, insecure, oci_ca_paths, credentials)?;
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
    // shows the descriptor, config facts, and exact component body.
    let mut source_config =
        ComponentArtifactSourceConfig::new(artifact_base, insecure, REGISTRY_IO_TIMEOUT)
            .context("configure published component verification source")?
            .with_ca_paths(oci_ca_paths)
            .context("read component registry CA bundles")?;
    if let Some(credentials) = credentials {
        source_config = source_config.with_credentials(credentials.clone());
    }
    ComponentArtifactSource::new(source_config)
        .context("configure published component verification client")?
        .pull_verified(component)
        .await
        .with_context(|| format!("verify published component artifact {reference}"))?;
    Ok(())
}

/// One registry client for the component repository of `reference`, and its
/// authentication.
pub fn registry_client(
    reference: &Reference,
    insecure: bool,
    oci_ca_paths: &[PathBuf],
    credentials: Option<&RegistryCredentials>,
) -> anyhow::Result<(OciClient, RegistryAuth)> {
    let base = wamn_catalog::parse_component_artifact_base(&format!(
        "{}/{}",
        reference.resolve_registry(),
        reference.repository()
    ))?;
    let client = wamn_package_artifact::registry_source::RegistrySource::new(
        base,
        insecure,
        REGISTRY_IO_TIMEOUT,
    )
    .with_ca_bundles(read_ca_bundles(oci_ca_paths).context("read component registry CA bundles")?)
    .client()
    .context("configure component registry push client")?;
    // No credential: the auth file states an anonymous push for this registry.
    let auth = credentials.map_or(RegistryAuth::Anonymous, |credentials| {
        RegistryAuth::Basic(
            credentials.username().to_owned(),
            credentials.password().to_owned(),
        )
    });
    Ok((client, auth))
}

pub(crate) fn artifact_layout(
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

#[cfg(test)]
mod tests {
    use wamn_catalog::{
        AdmittedComponentOperation, ComponentOperationDeclaration, ComponentPackageCoordinate,
        component_artifact_reference,
    };
    use wamn_component_artifact::component_artifact_source::OCI_CA_PATHS_ENV;
    use wamn_engine::component_artifact::component_artifact_config_bytes;
    use wamn_package_artifact::registry_credentials::read_registry_push_credentials;

    use super::*;

    fn fixture_component() -> AdmittedComponent {
        let mut document: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/component_package/component.json"
        ))
        .expect("fixture component declaration is JSON");
        document["target"]["project_env_key"] = serde_json::json!("org-a");
        let declaration: ComponentDeclaration =
            serde_json::from_value(document).expect("fixture component declaration parses");
        AdmittedComponent {
            target: declaration.target,
            component: declaration.component,
            interface_version: declaration.interface_version,
            operations: declaration
                .operations
                .into_iter()
                .map(|(export, operation)| {
                    (
                        export,
                        AdmittedComponentOperation {
                            connections: operation.connections,
                            committed_result_schema: operation
                                .committed_result_schema
                                .map(|schema| wamn_catalog::ComponentSchema { schema }),
                            registered_operation: operation.registered_operation,
                            host_transaction: false,
                            input_ports: Vec::new(),
                            output_ports: Vec::new(),
                            parameters: Vec::new(),
                            statements: BTreeMap::new(),
                        },
                    )
                })
                .collect(),
            component_digest: format!("sha256:{}", "c".repeat(64)),
            imports: Vec::new(),
            imports_fingerprint: format!("sha256:{}", "d".repeat(64)),
            effects: Vec::new(),
        }
    }

    #[test]
    fn package_evidence_binds_exact_sql_to_each_manifest_operation() {
        let package_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/component_package");
        let manifest = wamn_compiler::manifest::PackageManifest::from_slice(include_bytes!(
            "../tests/fixtures/component_package/generated/wamn.json"
        ))
        .expect("fixture package manifest parses");
        let mut component = fixture_component();

        let statements = load_component_statement_facts(&package_root, &manifest, &component)
            .expect("generated operation contracts and package metadata agree");
        assert_eq!(
            load_package_statement_facts(&package_root, &manifest)
                .expect("qualification loads the same exact package facts"),
            statements
        );
        assert_eq!(statements.len(), component.operations.len());
        assert!(statements.values().all(|operation| !operation.is_empty()));
        let host_transactions = host_transaction_exports(&manifest, &statements)
            .expect("the manifest names canonical operations");
        bind_component_statement_facts(&mut component, statements, &host_transactions)
            .expect("manifest-backed facts bind to the exact exports");

        let operation = "source-fixture:item/get@1.0.0";
        let statement = component.operations[operation]
            .statements
            .values()
            .next()
            .expect("get has one admitted statement");

        assert_eq!(
            component.operations[operation].statement(&statement.path),
            Some(statement)
        );
    }

    #[test]
    fn unsafe_paths_are_typed_statement_refusals() {
        let package_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/component_package");
        let canonical_root = package_root.canonicalize().expect("package root resolves");
        let path_error = read_package_owned_file(
            &package_root,
            &canonical_root,
            "generated operation contract",
            "../Cargo.toml",
            "json",
        )
        .expect_err("a path traversal was admitted");
        assert_eq!(
            path_error.error_type(),
            ComponentProjectionErrorType::StatementPathInvalid
        );
    }

    #[test]
    fn declared_sql_that_calls_a_counter_function_is_refused_at_admission() {
        let package_root = std::env::temp_dir().join(format!(
            "wamn-declared-sql-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(package_root.join("generated/sql"))
            .expect("create the package root");
        let canonical_root = package_root.canonicalize().expect("package root resolves");
        for (sql, refused) in [
            ("SELECT id FROM item WHERE id = $1", false),
            ("SELECT platform.bump_counter($1::uuid, 'po', '')", true),
            (
                "SELECT \"platform\".\"assign_counter\"($1::uuid, 'po', '')",
                true,
            ),
            (
                "WITH n AS (SELECT set_counter($1::uuid, 'po', '', 1)) SELECT 1",
                true,
            ),
            ("UPDATE platform.setting SET value = $1", true),
            ("DO $$ BEGIN EXECUTE 'SELECT 1'; END $$", true),
        ] {
            std::fs::write(package_root.join("generated/sql/q.sql"), sql)
                .expect("write the statement");
            let statement = GeneratedStatementContract {
                name: "q".to_owned(),
                path: "generated/sql/q.sql".to_owned(),
                binds: Vec::new(),
                columns: Vec::new(),
                relations: Vec::new(),
                transactional: false,
            };
            let result = load_operation_statements(
                &package_root,
                &canonical_root,
                "source-fixture:item/get@1.0.0",
                vec![statement],
                &mut BTreeMap::new(),
            );
            if refused {
                assert_eq!(
                    result.expect_err(sql).error_type(),
                    ComponentProjectionErrorType::StatementRefused,
                    "{sql:?}"
                );
            } else {
                result.unwrap_or_else(|error| panic!("{sql:?} is refused: {error}"));
            }
        }
        std::fs::remove_dir_all(&package_root).expect("remove the package root");
    }

    /// Every statement the shipped packages declare passes the parsed test.
    /// It reads the build output of `wamn build` (`tools/lane-start`).

    #[test]
    fn private_manifest_operation_cannot_cross_component_ownership() {
        let mut document: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/observer_package/generated/wamn.json"
        ))
        .expect("fixture observer manifest is JSON");
        let primary = "observer_fixture";
        for model in document["models"]
            .as_object_mut()
            .expect("models is an object")
            .values_mut()
        {
            for operation in model["operations"]
                .as_object_mut()
                .expect("model operations is an object")
                .values_mut()
            {
                operation["component"] = serde_json::json!(primary);
            }
        }
        for operation in document["custom_operations"]
            .as_object_mut()
            .expect("custom operations is an object")
            .values_mut()
        {
            operation["component"] = serde_json::json!(primary);
        }
        document["connections"] = serde_json::json!({"secondary-only":{"interface":"wamn:connection/http@0.1.0","mode":"write"}});
        document["components"]["secondary"] = serde_json::json!({});
        document["custom_operations"]["audit.observe"]["component"] =
            serde_json::json!("secondary");
        let manifest: wamn_compiler::manifest::PackageManifest =
            serde_json::from_value(document).expect("two-component manifest parses");
        let operation = "observer-fixture:audit/observe@3.0.0".to_owned();
        let component = AdmittedComponent {
            target: wamn_catalog::ComponentPackageCoordinate {
                project_env_key: "org-a".to_owned(),
                package_id: manifest.package.id.clone(),
                package_version: manifest.package.version.clone(),
            },
            component: primary.to_owned(),
            interface_version: "0.1.0".to_owned(),
            operations: BTreeMap::from([(
                operation,
                AdmittedComponentOperation {
                    connections: Vec::new(),
                    committed_result_schema: None,
                    registered_operation: None,
                    host_transaction: false,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
            component_digest: format!("sha256:{}", "e".repeat(64)),
            imports: Vec::new(),
            imports_fingerprint: format!("sha256:{}", "f".repeat(64)),
            effects: Vec::new(),
        };
        let expected =
            expected_operation_contracts(&manifest).expect("operation contract coordinates derive");

        let error = validate_component_operation_assignment(&manifest, &component, &expected)
            .expect_err("a private export assigned to another component was accepted");

        assert_eq!(
            error.error_type(),
            ComponentProjectionErrorType::StatementComponentMismatch
        );
    }

    #[test]
    fn signed_file_result_metadata_is_bound_from_the_package_only() {
        let mut manifest = overlay_manifest();
        let declaration = manifest.custom_operations.get_mut("audit.observe").unwrap();
        declaration.type_ = wamn_compiler::manifest::CustomOperationType::Projection;
        declaration.visibility = wamn_compiler::manifest::OperationVisibility::Public;
        declaration.permission = Some("audit.observe".into());
        declaration.errors.push("permission_denied".into());
        declaration.result = Some(
            serde_json::from_value(serde_json::json!({
                "class": "one", "fields": [{"path": "document", "type": "uuid", "nullable": false}],
                "signed_reads": {"document": {"lifetime_seconds": 12}}
            }))
            .unwrap(),
        );
        let export = wamn_compiler::manifest::canonical_operation_identity(
            &manifest.package,
            "audit.observe",
        )
        .unwrap();
        let mut component = stranger(&manifest, "observer_fixture", Some(&export));
        let (_, mut operation) = component.operations.pop_first().unwrap();
        operation
            .output_ports
            .push(wamn_catalog::AdmittedComponentPort {
                name: "main".into(),
                schema: wamn_catalog::ComponentSchema {
                    schema: serde_json::json!({"type": "array"}),
                },
            });
        component.operations.insert(export.clone(), operation);
        super::bind_signed_file_results(&mut component, &manifest).unwrap();
        let reads = wamn_catalog::signed_file_reads(&component.operations[&export])
            .unwrap()
            .unwrap();

        assert_eq!(reads.fields[0].read.lifetime_seconds().unwrap(), 12);
        assert!(!reads.fields[0].nullable);
        // A component cannot add its own signing authority through an annotation.
        assert!(super::bind_signed_file_results(&mut component, &manifest).is_err());
        component.operations.get_mut(&export).unwrap().output_ports[0]
            .schema
            .schema
            .as_object_mut()
            .unwrap()
            .remove(wamn_catalog::SIGNED_FILE_READS);
        manifest
            .custom_operations
            .get_mut("audit.observe")
            .unwrap()
            .result
            .as_mut()
            .unwrap()
            .signed_reads
            .get_mut("document")
            .unwrap()
            .lifetime_seconds = Some(901);
        assert!(super::bind_signed_file_results(&mut component, &manifest).is_err());
    }

    fn overlay_manifest() -> wamn_compiler::manifest::PackageManifest {
        serde_json::from_str(include_str!(
            "../tests/fixtures/observer_package/generated/wamn.json"
        ))
        .expect("fixture observer manifest parses")
    }

    /// A component absent from the manifest, exporting one handler and
    /// registering nothing: the palette-node shape.
    fn stranger(
        manifest: &wamn_compiler::manifest::PackageManifest,
        name: &str,
        registered: Option<&str>,
    ) -> AdmittedComponent {
        AdmittedComponent {
            target: wamn_catalog::ComponentPackageCoordinate {
                project_env_key: "org-a".to_owned(),
                package_id: manifest.package.id.clone(),
                package_version: manifest.package.version.clone(),
            },
            component: name.to_owned(),
            interface_version: "0.1.0".to_owned(),
            operations: BTreeMap::from([(
                "wamn:node/handler@0.1.0".to_owned(),
                AdmittedComponentOperation {
                    connections: Vec::new(),
                    committed_result_schema: None,
                    registered_operation: registered.map(str::to_owned),
                    host_transaction: false,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
            component_digest: format!("sha256:{}", "a".repeat(64)),
            imports: Vec::new(),
            imports_fingerprint: format!("sha256:{}", "b".repeat(64)),
            effects: Vec::new(),
        }
    }

    /// RULED wamn-362o.40: a palette node is not the package's to declare.
    #[test]
    fn palette_node_absent_from_the_manifest_serves_no_manifest_operation() {
        let manifest = overlay_manifest();
        assert!(!manifest.components.contains_key("label-render"));
        let expected =
            expected_operation_contracts(&manifest).expect("operation contract coordinates derive");

        let served = validate_component_operation_assignment(
            &manifest,
            &stranger(&manifest, "label-render", None),
            &expected,
        )
        .expect("an unregistered palette node is admitted without manifest membership");

        assert!(
            served.is_empty(),
            "a palette node serves no manifest operation: {served:?}"
        );
    }

    /// The other half of the ruling: membership is still the fence for anyone
    /// declaring that it SERVES the package's operations.
    #[test]
    fn stranger_registering_a_manifest_operation_is_refused_on_membership() {
        let manifest = overlay_manifest();
        let expected =
            expected_operation_contracts(&manifest).expect("operation contract coordinates derive");
        let registered = expected
            .keys()
            .next()
            .expect("the manifest registers operations")
            .clone();

        let error = validate_component_operation_assignment(
            &manifest,
            &stranger(&manifest, "stranger", Some(&registered)),
            &expected,
        )
        .expect_err("a stranger registering a manifest operation was admitted");

        assert_eq!(
            error.error_type(),
            ComponentProjectionErrorType::StatementComponentMismatch
        );
    }

    /// The exact bytes this publisher stores in `requirement_json`, frozen
    /// whole. `requirement_hash` is the SHA-256 of these same bytes, so an
    /// added, removed, or renamed field moves the persisted identity of every
    /// component connection and must not pass silently.

    #[tokio::test]
    #[ignore = "requires: env:WAMN_COMPONENT_ARTIFACT_BASE, env:WAMN_REGISTRY_AUTH_FILE"]
    async fn production_publisher_and_puller_round_trip_exact_bytes() {
        wamn_test_postgres::require_ignored_prerequisites(
            "requires: env:WAMN_COMPONENT_ARTIFACT_BASE, env:WAMN_REGISTRY_AUTH_FILE",
        );
        let artifact_base = std::env::var("WAMN_COMPONENT_ARTIFACT_BASE")
            .expect("set WAMN_COMPONENT_ARTIFACT_BASE to a disposable registry/repository");
        let registry_auth_file = std::env::var("WAMN_REGISTRY_AUTH_FILE")
            .expect("set WAMN_REGISTRY_AUTH_FILE to its Docker config credential");
        // A CA names a TLS registry; without one the registry stays plain HTTP.
        let oci_ca_paths: Vec<PathBuf> = std::env::var(OCI_CA_PATHS_ENV)
            .map(|paths| paths.split(',').map(PathBuf::from).collect())
            .unwrap_or_default();
        let insecure_registry = oci_ca_paths.is_empty();
        let operation = "wamn:node/handler@0.1.0";
        let mut resolve = wit_parser::Resolve::new();
        let package = resolve
            .push_str(
                "wamn-node.wit",
                include_str!("../../../../wit/wamn-node/package.wit"),
            )
            .expect("the live node WIT parses");
        let world = resolve
            .select_world(&[package], Some("node"))
            .expect("the live node world resolves");
        let mut module =
            wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
        wit_component::embed_component_metadata(
            &mut module,
            &resolve,
            world,
            wit_component::StringEncoding::UTF8,
        )
        .expect("fixture component metadata embeds");
        let component_bytes = wit_component::ComponentEncoder::default()
            .module(&module)
            .expect("fixture core module is accepted")
            .validate(true)
            .encode()
            .expect("fixture component encodes");
        let engine = wamn_engine::build_engine(&[]).expect("component admission engine builds");
        let component = validate_component_admission(
            &engine,
            &component_bytes,
            wamn_engine::component_admission::ComponentAdmissionRequest {
                declaration: ComponentDeclaration {
                    target: ComponentPackageCoordinate {
                        project_env_key: "org-a".to_owned(),
                        package_id: "orders".to_owned(),
                        package_version: "1.0.0".to_owned(),
                    },
                    component: "round-trip".to_owned(),
                    interface_version: "0.1.0".to_owned(),
                    operations: BTreeMap::from([(
                        operation.to_owned(),
                        ComponentOperationDeclaration {
                            connections: Vec::new(),
                            committed_result_schema: None,
                            registered_operation: None,
                            input_ports: Vec::new(),
                            output_ports: Vec::new(),
                            parameters: Vec::new(),
                        },
                    )]),
                },
                admitted_platform_packages: std::collections::BTreeSet::new(),
                base_digests: std::collections::BTreeSet::new(),
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
        let credentials = read_registry_push_credentials(
            PathBuf::from(&registry_auth_file).as_path(),
            artifact.registry(),
        )
        .expect("load live registry credential");

        publish_and_verify(
            &reference,
            &artifact_base,
            insecure_registry,
            &oci_ca_paths,
            &component_bytes,
            &config_bytes,
            &component,
            credentials.as_ref(),
        )
        .await
        .expect("production publisher and puller agree");
    }
}

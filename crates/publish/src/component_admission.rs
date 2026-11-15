//! Admit exact component bytes, and publish one digest-addressed OCI layer.
//!
//! This is the sole production caller of component byte admission. It validates
//! the local bytes against their package and declaration. `push-package` pushes
//! each admitted component with [`publish_and_verify`] and pulls it back.

pub(super) mod plan;
pub use plan::portable_requirement;
use plan::{
    bind_signed_file_results, ensure_component_package_matches, expected_operation_contracts,
    host_transaction_exports, validate_component_operation_assignment,
};

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use oci_client::client::{Config, ImageLayer};
use oci_client::manifest::OciImageManifest;
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use wamn_catalog::ComponentConnectionRequirement;
use wamn_catalog::ConnectionTypeDescriptor;
use wamn_catalog::package_schema::package_metadata;
use wamn_catalog::{
    AdmittedComponent, ComponentConnection, ComponentConnectionType, ComponentDeclaration,
    ComponentDescriptor,
};
use wamn_component_artifact::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_engine::artifact_source::ArtifactSource as _;
use wamn_engine::component_admission::validate_component_admission;
use wamn_engine::component_artifact::component_artifact_layout;
use wamn_package_artifact::read_ca_bundles;
use wamn_package_artifact::registry_credentials::RegistryCredentials;

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
}

impl ComponentProjectionErrorType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PackageCoordinateMismatch => "package-coordinate-mismatch",
            Self::StatementContractInvalid => "statement-contract-invalid",
            Self::StatementOperationMismatch => "statement-operation-mismatch",
            Self::StatementComponentMismatch => "statement-component-mismatch",
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
    pub base_digests: BTreeSet<wamn_digest::Digest>,
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

    /// The admitted descriptor (docs/operations/building.md#reading-a-pushed-package
    /// ): the facts without their target, and the declared connections sorted
    /// by store alias, as `push-package` writes it.
    pub fn descriptor(&self) -> ComponentDescriptor {
        ComponentDescriptor::new(self.component.clone(), self.connections.to_vec())
    }

    /// Exact package identity carried by the admitted component facts.
    pub fn package_id(&self) -> &str {
        &self.component.target.package_id
    }

    /// Exact package version carried by the admitted component facts.
    pub fn package_version(&self) -> &wamn_catalog::package_version::PackageVersion {
        &self.component.target.package_version
    }

    /// Package-local component name bound to the admitted bytes.
    pub fn component(&self) -> &str {
        &self.component.component
    }

    /// SHA-256 identity derived from the exact admitted bytes.
    pub fn component_digest(&self) -> &wamn_digest::Digest {
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
    let expected = expected_operation_contracts(&package_manifest)?;
    let selected =
        validate_component_operation_assignment(&package_manifest, &admitted, &expected)?;
    let host_transactions = host_transaction_exports(&package_manifest, &selected)?;
    for (name, operation) in &mut admitted.operations {
        operation.host_transaction = host_transactions.contains(name.as_str());
    }
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
    })
}

#[derive(Debug)]
struct ExpectedOperationContract {
    component: String,
    public: bool,
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
                    committed_result_schema: None,
                    registered_operation: None,
                    host_transaction: false,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                },
            )]),
            component_digest: (format!("sha256:{}", "e".repeat(64)))
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            imports: Vec::new(),
            imports_fingerprint: (format!("sha256:{}", "f".repeat(64)))
                .parse::<wamn_digest::Digest>()
                .unwrap(),
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
        declaration.permission = Some("audit.observe".parse().unwrap());
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
            &"audit.observe".parse().unwrap(),
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
        component.operations.insert(export.to_string(), operation);
        super::bind_signed_file_results(&mut component, &manifest).unwrap();
        let reads = wamn_catalog::signed_file_reads(&component.operations[export.as_str()])
            .unwrap()
            .unwrap();

        assert_eq!(reads.fields[0].read.lifetime_seconds().unwrap(), 12);
        assert!(!reads.fields[0].nullable);
        // A component cannot add its own signing authority through an annotation.
        assert!(super::bind_signed_file_results(&mut component, &manifest).is_err());
        component
            .operations
            .get_mut(export.as_str())
            .unwrap()
            .output_ports[0]
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
        registered: Option<&wamn_catalog::registered_operation::RegisteredOperation>,
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
                    committed_result_schema: None,
                    registered_operation: registered.cloned(),
                    host_transaction: false,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                },
            )]),
            component_digest: (format!("sha256:{}", "a".repeat(64)))
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            imports: Vec::new(),
            imports_fingerprint: (format!("sha256:{}", "b".repeat(64)))
                .parse::<wamn_digest::Digest>()
                .unwrap(),
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
                include_str!("../../../wit/wamn-node/package.wit"),
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
                    connections: Vec::new(),
                    target: ComponentPackageCoordinate {
                        project_env_key: "org-a".to_owned(),
                        package_id: "orders".to_owned(),
                        package_version: "1.0.0".parse().unwrap(),
                    },
                    component: "round-trip".to_owned(),
                    interface_version: "0.1.0".to_owned(),
                    operations: BTreeMap::from([(
                        operation.to_owned(),
                        ComponentOperationDeclaration {
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

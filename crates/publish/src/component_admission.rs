//! Admit exact component bytes, and publish one digest-addressed OCI layer.
//!
//! This is the sole production caller of component byte admission. It validates
//! the local bytes against their package and declaration. `push-package` pushes
//! each admitted component with stock `push_component`.

pub(super) mod plan;
pub use plan::portable_requirement;
use plan::{
    bind_signed_file_results, ensure_component_package_matches, expected_operation_contracts,
    host_transaction_exports, validate_component_operation_assignment,
};

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

use anyhow::Context as _;
use wamn_catalog::ComponentConnectionRequirement;
use wamn_catalog::ConnectionTypeDescriptor;
use wamn_catalog::package_schema::package_metadata;
use wamn_catalog::{
    AdmittedComponent, ComponentConnection, ComponentConnectionType, ComponentDeclaration,
    ComponentDescriptor, ComponentPackageCoordinate,
};
use wamn_engine::component_admission::validate_component_admission;

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
    /// Closed platform import set granted to these exact bytes.
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
    pub fn package_id(&self) -> &wamn_catalog::PackageName {
        &self.component.target.package_id
    }

    /// Exact package version carried by the admitted component facts.
    pub fn package_version(&self) -> &wamn_catalog::package_version::PackageVersion {
        &self.component.target.package_version
    }

    /// Package-local component name bound to the admitted bytes.
    pub fn component(&self) -> &wamn_catalog::canonical_name::CanonicalName {
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
        wamn_catalog::manifest::PackageManifest::from_slice(&directory.manifest_bytes)
            .context("parse strict package manifest before component publication")?;
    let component_bytes = std::fs::read(&args.component_bytes)
        .with_context(|| format!("read component bytes {}", args.component_bytes.display()))?;
    let declaration_bytes = std::fs::read(&args.declaration)
        .with_context(|| format!("read component declaration {}", args.declaration.display()))?;
    let document: serde_json::Value = serde_json::from_slice(&declaration_bytes)
        .with_context(|| format!("parse component declaration {}", args.declaration.display()))?;
    let mut declaration: ComponentDeclaration<ComponentPackageCoordinate> =
        serde_json::from_value(document).with_context(|| {
            format!("parse component declaration {}", args.declaration.display())
        })?;

    declaration.data_change = package_manifest.components
        .get(declaration.component.as_str())
        .is_some_and(|component| component.data_change.is_some());

    let (declaration, target) = declaration.split_target();
    let (descriptor, requirements) = admit(
        &package_manifest,
        &component_bytes,
        declaration,
        args.admitted_platform_packages.into_iter().collect(),
        args.base_digests,
    )?;
    let facts = descriptor.into_admitted(target);
    ensure_component_package_matches(&facts.component, &package.coordinate)?;
    Ok(ComponentAdmission {
        component_bytes: component_bytes.into_boxed_slice(),
        component: facts.component,
        connections: facts.connections.into_boxed_slice(),
        requirements: requirements.into_boxed_slice(),
    })
}

pub(crate) fn admit(
    package_manifest: &wamn_catalog::manifest::PackageManifest,
    component_bytes: &[u8],
    declaration: ComponentDeclaration,
    admitted_platform_packages: BTreeSet<String>,
    base_digests: BTreeSet<wamn_digest::Digest>,
) -> anyhow::Result<(ComponentDescriptor, Vec<ComponentConnectionRequirement>)> {
    wamn_build::composer::validate_application_member(component_bytes)
        .context("refuse application-owned entry authority")?;
    let engine = wamn_engine::build_engine(&[]).context("build component admission engine")?;
    let mut admitted = validate_component_admission(
        &engine,
        component_bytes,
        wamn_engine::component_admission::ComponentAdmissionRequest {
            declaration,
            admitted_platform_packages,
            base_digests,
        },
    )
    .context("validate exact component bytes")?;
    let expected = expected_operation_contracts(package_manifest)?;
    let selected = validate_component_operation_assignment(package_manifest, &admitted, &expected)?;
    let host_transactions = host_transaction_exports(package_manifest, &selected)?;
    for (name, operation) in &mut admitted.operations {
        operation.host_transaction = host_transactions.contains(name.as_str());
    }
    bind_signed_file_results(&mut admitted, package_manifest)
        .context("bind package signed file results")?;
    let requirements = admitted
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
    for connection in &mut admitted.connections {
        let alias = connection
            .store_alias
            .name()
            .context("package connection needs a fixed alias")?;
        let declaration = package_manifest
            .connections
            .get(alias)
            .with_context(|| format!("package declares no connection {alias}"))?;
        connection.mode = match declaration.mode {
            wamn_catalog::manifest::ConnectionMode::Read => wamn_catalog::ConnectionMode::Read,
            wamn_catalog::manifest::ConnectionMode::Write => wamn_catalog::ConnectionMode::Write,
        };
    }
    admitted
        .connections
        .sort_by(|left, right| left.store_alias.cmp(&right.store_alias));
    Ok((admitted, requirements))
}

#[derive(Debug)]
struct ExpectedOperationContract {
    component: wamn_catalog::canonical_name::CanonicalName,
    public: bool,
}

#[cfg(test)]
mod tests {
    use wamn_catalog::AdmittedComponentOperation;

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
        let manifest: wamn_catalog::manifest::PackageManifest =
            serde_json::from_value(document).expect("two-component manifest parses");
        let operation = "observer-fixture:audit/observe@3.0.0".parse().unwrap();
        let component = ComponentDescriptor {
            data_change: false,
            connections: Vec::new(),
            component: primary.parse().unwrap(),
            interface_version: "0.1.0".parse().unwrap(),
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
        declaration.type_ = wamn_catalog::manifest::CustomOperationType::Projection;
        declaration.visibility = wamn_catalog::manifest::OperationVisibility::Public;
        declaration.permission = Some("audit.observe".parse().unwrap());
        declaration
            .errors
            .push("permission_denied".parse().unwrap());
        declaration.result = Some(
            serde_json::from_value(serde_json::json!({
                "class": "one", "fields": [{"path": "document", "type": "uuid", "nullable": false}],
                "signed_reads": {"document": {"lifetime_seconds": 12}}
            }))
            .unwrap(),
        );
        let export = wamn_catalog::manifest::canonical_operation_identity(
            &manifest.package,
            &"audit.observe".parse().unwrap(),
        )
        .unwrap();
        let mut component = stranger("observer_fixture", Some(&export));
        let (_, mut operation) = component.operations.pop_first().unwrap();
        operation
            .output_ports
            .push(wamn_catalog::AdmittedComponentPort {
                name: "main".parse().unwrap(),
                schema: wamn_catalog::ComponentSchema {
                    schema: serde_json::json!({"type": "array"}),
                },
            });
        component
            .operations
            .insert(export.clone().into(), operation);
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

    fn overlay_manifest() -> wamn_catalog::manifest::PackageManifest {
        serde_json::from_str(include_str!(
            "../tests/fixtures/observer_package/generated/wamn.json"
        ))
        .expect("fixture observer manifest parses")
    }

    /// A component absent from the manifest, exporting one handler and
    /// registering nothing: the palette-node shape.
    fn stranger(
        name: &str,
        registered: Option<&wamn_catalog::registered_operation::RegisteredOperation>,
    ) -> ComponentDescriptor {
        ComponentDescriptor {
            data_change: false,
            connections: Vec::new(),
            component: name.parse().unwrap(),
            interface_version: "0.1.0".parse().unwrap(),
            operations: BTreeMap::from([(
                "wamn:node/handler@0.1.0".parse().unwrap(),
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
            &stranger("label-render", None),
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
            &stranger("stranger", Some(&registered)),
            &expected,
        )
        .expect_err("a stranger registering a manifest operation was admitted");

        assert_eq!(
            error.error_type(),
            ComponentProjectionErrorType::StatementComponentMismatch
        );
    }

}

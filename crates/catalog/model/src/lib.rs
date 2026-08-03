//! Canonical immutable catalog identities and definition hashing.
//!
//! This crate owns only the pure definition plane. Persistence, publication,
//! activation transitions, and compatibility readers live in effect crates.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use wamn_flow::{Flow, ResolvedInterfaces};
use wamn_node_manifest::{
    CONNECTION_DESCRIPTOR_VERSION, ConnectionRecoverySupport, ConnectionTypeDescriptor,
    EXECUTABLE_RECOVERY_CONTRACT_VERSION, ExecutableConnectionRecoveryMode, ExecutableIdentity,
    ExecutableRecoveryClaim, ExecutableRecoveryContract, OCCURRENCE_RECOVERY_SELECTION_VERSION,
    OccurrenceRecoverySelection, PORTABLE_CONNECTION_REQUIREMENT_VERSION,
    PortableConnectionRequirement, PortableRecoveryClaim, RESOLVED_CONTRACT_VERSION,
    RecoveryClaimParameterSchema, RecoveryClaimSchema, RecoveryClass, ResolvedComponent,
    ResolvedNodeContract, ResolvedNodeInterface, ResolvedPurity,
};

const HASH_PREFIX: &str = "sha256:";
const HASH_HEX_LEN: usize = 64;
const IDENTITY_FORMAT: &[u8] = b"wamn.catalog.identity.v1";
const MODEL_OWNED_NODES: [&str; 0] = [];
const HISTORICAL_MODEL_OWNED_NODES: [&str; 5] = ["cron", "event", "fail", "request", "respond"];
const HISTORICAL_RESOLVED_CONTRACT_VERSION: &str = "1";
const LEGACY_RESOLVED_CONTRACT_VERSION: &str = "legacy-v0";
const LEGACY_INTERFACE_CONTRACT: &str = "legacy-unpinned-wamn-node";
const LEGACY_PLATFORM_REVISION: &str = "legacy-unpinned-platform";
/// Serialized execution-bundle key shape. Bump on any incompatible frame change.
const EXECUTION_BUNDLE_IDENTITY_VERSION: &[u8] = b"1";

/// A catalog identity construction error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogIdentityError {
    EmptyIdentity { field: &'static str },
    NonCanonicalIdentity { field: &'static str },
    ZeroVersion { field: &'static str },
    InvalidDigest { field: &'static str },
    InvalidDefinition { message: String },
    NonCanonicalJson,
    MutableIdentityInput { field: String },
    DuplicateInterface { node_type: String },
    NonCanonicalInterfaceOrder { node_type: String },
    InvalidInterface { node_type: String, message: String },
    InterfaceBundleHashMismatch,
    OccurrenceRecoveryHashMismatch,
    GraphHashMismatch,
    ArtifactIdMismatch,
    ArtifactHashMismatch,
    UnresolvedInterface { node_type: String },
    UnexpectedInterface { node_type: String },
    FlowInvalid { codes: Vec<&'static str> },
    DuplicateMember { field: &'static str, id: String },
    NonCanonicalMemberOrder { field: &'static str, id: String },
    ArtifactMismatch,
    UnresolvedSource { source_id: String },
    SourceMismatch { source_id: String },
    ExecutionBundleIdentityMismatch,
    ExecutionBundleOutputMismatch,
}

impl fmt::Display for CatalogIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyIdentity { field } => write!(formatter, "{field} is empty"),
            Self::NonCanonicalIdentity { field } => {
                write!(formatter, "{field} is not in canonical form")
            }
            Self::ZeroVersion { field } => write!(formatter, "{field} must be greater than zero"),
            Self::InvalidDigest { field } => {
                write!(
                    formatter,
                    "{field} must be sha256:<64 lowercase hex digits>"
                )
            }
            Self::InvalidDefinition { message } => write!(formatter, "{message}"),
            Self::NonCanonicalJson => write!(formatter, "JSON input is not RFC 8785 canonical"),
            Self::MutableIdentityInput { field } => {
                write!(
                    formatter,
                    "mutable field {field:?} cannot enter definition identity"
                )
            }
            Self::DuplicateInterface { node_type } => {
                write!(formatter, "node interface {node_type:?} is duplicated")
            }
            Self::NonCanonicalInterfaceOrder { node_type } => {
                write!(
                    formatter,
                    "node interface {node_type:?} is not in canonical order"
                )
            }
            Self::InvalidInterface { node_type, message } => {
                write!(
                    formatter,
                    "node interface {node_type:?} is invalid: {message}"
                )
            }
            Self::InterfaceBundleHashMismatch => {
                write!(formatter, "resolved interface bundle hash does not match")
            }
            Self::OccurrenceRecoveryHashMismatch => {
                write!(formatter, "occurrence recovery hash does not match")
            }
            Self::GraphHashMismatch => write!(formatter, "flow graph hash does not match"),
            Self::ArtifactIdMismatch => {
                write!(
                    formatter,
                    "flow graph does not match the pinned artifact id"
                )
            }
            Self::ArtifactHashMismatch => write!(formatter, "flow artifact hash does not match"),
            Self::UnresolvedInterface { node_type } => {
                write!(
                    formatter,
                    "node type {node_type:?} has no resolved interface"
                )
            }
            Self::UnexpectedInterface { node_type } => {
                write!(
                    formatter,
                    "node type {node_type:?} is not used by the graph"
                )
            }
            Self::FlowInvalid { codes } => {
                write!(formatter, "flow validation failed: {}", codes.join(", "))
            }
            Self::DuplicateMember { field, id } => {
                write!(formatter, "{field} member {id:?} is duplicated")
            }
            Self::NonCanonicalMemberOrder { field, id } => {
                write!(formatter, "{field} member {id:?} is not in canonical order")
            }
            Self::ArtifactMismatch => write!(formatter, "attachment artifact does not match"),
            Self::UnresolvedSource { source_id } => {
                write!(formatter, "source {source_id:?} is unresolved")
            }
            Self::SourceMismatch { source_id } => {
                write!(
                    formatter,
                    "source {source_id:?} differs from its resolved definition"
                )
            }
            Self::ExecutionBundleIdentityMismatch => {
                write!(
                    formatter,
                    "execution-bundle provenance identity does not match"
                )
            }
            Self::ExecutionBundleOutputMismatch => {
                write!(
                    formatter,
                    "execution-bundle rebuild output does not match provenance"
                )
            }
        }
    }
}

impl std::error::Error for CatalogIdentityError {}

fn validate_text(value: &str, field: &'static str) -> Result<(), CatalogIdentityError> {
    if value.is_empty() {
        return Err(CatalogIdentityError::EmptyIdentity { field });
    }
    if value.trim() != value || value.as_bytes().contains(&0) {
        return Err(CatalogIdentityError::NonCanonicalIdentity { field });
    }
    Ok(())
}

/// A canonical SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct DefinitionHash(String);

impl DefinitionHash {
    /// Parse a canonical SHA-256 definition hash.
    pub fn parse(value: impl Into<String>) -> Result<Self, CatalogIdentityError> {
        let value = value.into();
        validate_digest(&value, "definition-hash")?;
        Ok(Self(value))
    }

    /// The `sha256:<hex>` representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DefinitionHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A canonical artifact-content digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ArtifactHash(String);

impl ArtifactHash {
    /// The `sha256:<hex>` representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn validate_digest(value: &str, field: &'static str) -> Result<(), CatalogIdentityError> {
    let valid = value.strip_prefix(HASH_PREFIX).is_some_and(|hex| {
        hex.len() == HASH_HEX_LEN
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if valid {
        Ok(())
    } else {
        Err(CatalogIdentityError::InvalidDigest { field })
    }
}

fn digest(input: &[u8]) -> String {
    let digest = Sha256::digest(input);
    let mut output = String::with_capacity(HASH_PREFIX.len() + HASH_HEX_LEN);
    output.push_str(HASH_PREFIX);
    for byte in digest {
        use fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a string is infallible");
    }
    output
}

/// The tenant-scoped immutable identity of a flow artifact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ArtifactId {
    tenant_id: String,
    flow_id: String,
    flow_version: u32,
}

impl ArtifactId {
    pub fn new(
        tenant_id: impl Into<String>,
        flow_id: impl Into<String>,
        flow_version: u32,
    ) -> Result<Self, CatalogIdentityError> {
        let tenant_id = tenant_id.into();
        let flow_id = flow_id.into();
        validate_text(&tenant_id, "tenant-id")?;
        validate_text(&flow_id, "flow-id")?;
        if flow_version == 0 {
            return Err(CatalogIdentityError::ZeroVersion {
                field: "flow-version",
            });
        }
        Ok(Self {
            tenant_id,
            flow_id,
            flow_version,
        })
    }

    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    pub fn flow_id(&self) -> &str {
        &self.flow_id
    }

    pub fn flow_version(&self) -> u32 {
        self.flow_version
    }
}

/// The immutable identity of a catalog release.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ReleaseId {
    tenant_id: String,
    catalog_id: String,
    catalog_version: u32,
}

impl ReleaseId {
    pub fn new(
        tenant_id: impl Into<String>,
        catalog_id: impl Into<String>,
        catalog_version: u32,
    ) -> Result<Self, CatalogIdentityError> {
        let tenant_id = tenant_id.into();
        let catalog_id = catalog_id.into();
        validate_text(&tenant_id, "tenant-id")?;
        validate_text(&catalog_id, "catalog-id")?;
        if catalog_version == 0 {
            return Err(CatalogIdentityError::ZeroVersion {
                field: "catalog-version",
            });
        }
        Ok(Self {
            tenant_id,
            catalog_id,
            catalog_version,
        })
    }

    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    pub fn catalog_id(&self) -> &str {
        &self.catalog_id
    }

    pub fn catalog_version(&self) -> u32 {
        self.catalog_version
    }
}

macro_rules! string_id {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, CatalogIdentityError> {
                let value = value.into();
                validate_text(&value, $field)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

string_id!(SourceId, "source-id");
string_id!(AttachmentId, "attachment-id");

/// Canonical JSON used by immutable source and attachment definitions.
#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalJson {
    value: Value,
    bytes: Box<[u8]>,
}

impl CanonicalJson {
    /// Canonicalize a JSON object, refusing operationally mutable top-level fields.
    pub fn new(value: Value) -> Result<Self, CatalogIdentityError> {
        let object = value
            .as_object()
            .ok_or_else(|| CatalogIdentityError::InvalidDefinition {
                message: "a source or attachment definition must be a JSON object".to_string(),
            })?;
        for field in [
            "active",
            "applied-version",
            "changed-at",
            "confirmed-definition-hash",
            "created-at",
            "enabled",
            "updated-at",
        ] {
            if object.contains_key(field) {
                return Err(CatalogIdentityError::MutableIdentityInput {
                    field: field.to_string(),
                });
            }
        }
        let bytes = canonical_json(&value).into_boxed_slice();
        Ok(Self { value, bytes })
    }

    /// Parse bytes that must already be in RFC 8785 canonical form.
    pub fn parse(input: &str) -> Result<Self, CatalogIdentityError> {
        let value: Value = serde_json::from_str(input).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("definition JSON is invalid: {error}"),
            }
        })?;
        let canonical = Self::new(value)?;
        if canonical.bytes.as_ref() != input.as_bytes() {
            return Err(CatalogIdentityError::NonCanonicalJson);
        }
        Ok(canonical)
    }

    pub fn as_value(&self) -> &Value {
        &self.value
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Whether an implementation is platform-pinned or supplied by the release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeImplementation {
    contract: ResolvedNodeContract,
}

impl NodeImplementation {
    pub fn platform(
        interface: ResolvedNodeInterface,
        executable_recovery: ExecutableRecoveryContract,
    ) -> Self {
        Self {
            contract: ResolvedNodeContract {
                executable_recovery,
                interface,
                executable: ExecutableIdentity::Platform {
                    revision: "wamn-standard-nodes@0.1.0".to_string(),
                },
                connection_recovery_support: Vec::new(),
                portable_connections: Vec::new(),
            },
        }
    }

    pub fn supplied(
        interface: ResolvedNodeInterface,
        component_digest: impl Into<String>,
        executable_recovery: ExecutableRecoveryContract,
    ) -> Result<Self, CatalogIdentityError> {
        let component_digest = component_digest.into();
        validate_digest(&component_digest, "component-digest")?;
        Ok(Self {
            contract: ResolvedNodeContract {
                executable_recovery,
                interface,
                executable: ExecutableIdentity::Component {
                    digest: component_digest,
                },
                connection_recovery_support: Vec::new(),
                portable_connections: Vec::new(),
            },
        })
    }

    pub fn from_resolved_component(
        component: ResolvedComponent,
    ) -> Result<Self, CatalogIdentityError> {
        let ExecutableIdentity::Component { digest } = &component.contract.executable else {
            return Err(CatalogIdentityError::InvalidDefinition {
                message: "a supplied component must carry component executable identity"
                    .to_string(),
            });
        };
        validate_digest(digest, "component-digest")?;
        Ok(Self {
            contract: component.contract,
        })
    }

    /// Accept a complete platform contract resolved by its owning descriptor.
    pub fn from_resolved_platform_contract(
        contract: ResolvedNodeContract,
    ) -> Result<Self, CatalogIdentityError> {
        if !matches!(contract.executable, ExecutableIdentity::Platform { .. }) {
            return Err(CatalogIdentityError::InvalidDefinition {
                message: "a standard node must carry platform executable identity".to_string(),
            });
        }
        let implementation = Self { contract };
        validate_implementation_order(std::slice::from_ref(&implementation))?;
        Ok(implementation)
    }

    pub fn interface(&self) -> &ResolvedNodeInterface {
        &self.contract.interface
    }

    /// Replace the authoritative executable recovery declaration.
    pub fn with_executable_recovery(
        mut self,
        recovery: ExecutableRecoveryContract,
    ) -> Result<Self, CatalogIdentityError> {
        self.contract.executable_recovery = recovery;
        validate_executable_recovery(&self.contract)?;
        Ok(self)
    }

    /// Attach executable recovery declarations for exact connection contracts.
    pub fn with_connection_recovery_support(
        mut self,
        connection_recovery_support: Vec<ConnectionRecoverySupport>,
    ) -> Result<Self, CatalogIdentityError> {
        self.contract.connection_recovery_support = connection_recovery_support;
        validate_connection_recovery_support(&self.contract)?;
        validate_portable_connections(&self.contract)?;
        Ok(self)
    }

    pub fn connection_recovery_support(&self) -> &[ConnectionRecoverySupport] {
        &self.contract.connection_recovery_support
    }

    /// Attach canonical portable connection semantics to this resolution.
    pub fn with_portable_connections(
        mut self,
        portable_connections: Vec<PortableConnectionRequirement>,
    ) -> Result<Self, CatalogIdentityError> {
        self.contract.portable_connections = portable_connections;
        validate_connection_recovery_support(&self.contract)?;
        validate_portable_connections(&self.contract)?;
        Ok(self)
    }

    pub fn portable_connections(&self) -> &[PortableConnectionRequirement] {
        &self.contract.portable_connections
    }

    pub fn component_digest(&self) -> Option<&str> {
        match &self.contract.executable {
            ExecutableIdentity::Component { digest } => Some(digest),
            ExecutableIdentity::Platform { .. } => None,
        }
    }

    /// The canonical resolved-node contract shared by all identity consumers.
    pub fn contract(&self) -> &ResolvedNodeContract {
        &self.contract
    }
}

/// The complete immutable artifact identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ArtifactIdentity {
    id: ArtifactId,
    artifact_hash: ArtifactHash,
}

impl ArtifactIdentity {
    pub fn id(&self) -> &ArtifactId {
        &self.id
    }

    pub fn artifact_hash(&self) -> &ArtifactHash {
        &self.artifact_hash
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        frames(
            "artifact-identity",
            [
                ("tenant-id", self.id.tenant_id.as_bytes()),
                ("flow-id", self.id.flow_id.as_bytes()),
                ("flow-version", &self.id.flow_version.to_be_bytes()),
                ("artifact-hash", self.artifact_hash.as_str().as_bytes()),
            ],
        )
    }
}

/// A canonical immutable flow artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    identity: ArtifactIdentity,
    schema_version: String,
    graph_hash: String,
    interface_bundle: InterfaceBundle,
    supplied_components: Vec<ResolvedComponent>,
    occurrence_recovery: Box<[OccurrenceRecoverySelection]>,
    occurrence_recovery_bytes: Box<[u8]>,
    occurrence_recovery_hash: String,
    canonical_bytes: Box<[u8]>,
}

impl Artifact {
    /// Build an artifact from its graph and fully resolved implementations.
    pub fn new(
        tenant_id: impl Into<String>,
        flow: &Flow,
        implementations: Vec<NodeImplementation>,
    ) -> Result<Self, CatalogIdentityError> {
        let occurrence_recovery = conservative_occurrence_recovery(flow, &implementations)?;
        Self::new_with_recovery_selections(tenant_id, flow, implementations, occurrence_recovery)
    }

    /// Build an artifact with an explicit ordered recovery choice for every graph occurrence.
    pub fn new_with_recovery_selections(
        tenant_id: impl Into<String>,
        flow: &Flow,
        implementations: Vec<NodeImplementation>,
        occurrence_recovery: Vec<OccurrenceRecoverySelection>,
    ) -> Result<Self, CatalogIdentityError> {
        let id = ArtifactId::new(tenant_id, flow.flow_id.clone(), flow.version)?;
        validate_text(&flow.schema_version, "schema-version")?;
        validate_implementations(flow, &implementations)?;
        validate_occurrence_recovery(flow, &implementations, &occurrence_recovery)?;

        let interfaces: Vec<_> = implementations
            .iter()
            .map(|implementation| implementation.interface().clone())
            .collect();
        let resolved: ResolvedInterfaces = interfaces
            .iter()
            .map(|interface| (interface.node_type.clone(), interface.output_ports.clone()))
            .collect();
        let flow_errors = flow.validate(&resolved).err().unwrap_or_default();
        if !flow_errors.is_empty() {
            return Err(CatalogIdentityError::FlowInvalid {
                codes: flow_errors.into_iter().map(|issue| issue.code).collect(),
            });
        }
        let graph_bytes = flow.canonical_bytes();
        let graph_hash = digest(&graph_bytes);
        let mut owned = Vec::new();
        let id_bytes = canonical_json(&serde_json::to_value(&id).expect("artifact id serializes"));
        owned.push(("artifact-id", id_bytes));
        owned.push(("schema-version", flow.schema_version.as_bytes().to_vec()));
        owned.push(("graph", graph_bytes));
        for implementation in &implementations {
            owned.push((
                "resolved-node",
                canonical_json(
                    &serde_json::to_value(implementation.contract())
                        .expect("resolved node contract serializes"),
                ),
            ));
        }
        if !implementations.iter().all(|implementation| {
            implementation.interface().contract_version == RESOLVED_CONTRACT_VERSION
        }) {
            return Err(CatalogIdentityError::InvalidDefinition {
                message: "current artifact producers require the current resolved-contract version"
                    .to_string(),
            });
        }
        for selection in &occurrence_recovery {
            owned.push((
                "occurrence-recovery",
                canonical_json(
                    &serde_json::to_value(selection)
                        .expect("occurrence recovery selection serializes"),
                ),
            ));
        }
        let supplied_components: Vec<_> = implementations
            .iter()
            .filter(|implementation| implementation.component_digest().is_some())
            .map(|implementation| ResolvedComponent {
                contract: implementation.contract().clone(),
            })
            .collect();
        let borrowed: Vec<_> = owned
            .iter()
            .map(|(tag, bytes)| (*tag, bytes.as_slice()))
            .collect();
        let canonical_bytes = frames("artifact", borrowed).into_boxed_slice();
        let artifact_hash = ArtifactHash(digest(&canonical_bytes));
        let interface_bundle = InterfaceBundle::from_implementations(&implementations)?;
        let occurrence_recovery_bytes = canonical_json(
            &serde_json::to_value(&occurrence_recovery)
                .expect("occurrence recovery selections serialize"),
        )
        .into_boxed_slice();
        let occurrence_recovery_hash = digest(&occurrence_recovery_bytes);
        Ok(Self {
            identity: ArtifactIdentity { id, artifact_hash },
            schema_version: flow.schema_version.clone(),
            graph_hash,
            interface_bundle,
            supplied_components,
            occurrence_recovery: occurrence_recovery.into_boxed_slice(),
            occurrence_recovery_bytes,
            occurrence_recovery_hash,
            canonical_bytes,
        })
    }

    pub fn identity(&self) -> &ArtifactIdentity {
        &self.identity
    }

    pub fn schema_version(&self) -> &str {
        &self.schema_version
    }

    pub fn graph_hash(&self) -> &str {
        &self.graph_hash
    }

    pub fn interfaces(&self) -> &[ResolvedNodeInterface] {
        self.interface_bundle.interfaces()
    }

    /// The canonical resolved interface bundle persisted with this artifact.
    pub fn interface_bundle(&self) -> &InterfaceBundle {
        &self.interface_bundle
    }

    pub fn supplied_components(&self) -> &[ResolvedComponent] {
        &self.supplied_components
    }

    /// Ordered recovery choices keyed by exact graph occurrence id.
    pub fn occurrence_recovery(&self) -> &[OccurrenceRecoverySelection] {
        &self.occurrence_recovery
    }

    /// Canonical ordered occurrence-recovery-selection JSON persisted with this artifact.
    pub fn occurrence_recovery_bytes(&self) -> &[u8] {
        &self.occurrence_recovery_bytes
    }

    /// SHA-256 of [`Self::occurrence_recovery_bytes`].
    pub fn occurrence_recovery_hash(&self) -> &str {
        &self.occurrence_recovery_hash
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }
}

/// Persisted resolved-interface shape used before the canonical node contract landed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct LegacyResolvedNodeInterface {
    node_type: String,
    output_ports: Vec<String>,
    purity: ResolvedPurity,
    recovery_class: wamn_node_manifest::RecoveryClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct LegacyResolvedComponent {
    interface: LegacyResolvedNodeInterface,
    component_digest: String,
}

/// Exact resolved-node contract persisted by historical contract version 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct HistoricalResolvedNodeInterfaceV1 {
    contract_version: String,
    node_type: String,
    interface_contract: String,
    output_ports: Vec<String>,
    capability_classes: Vec<wamn_node_manifest::CapabilityClass>,
    connection_requirements: Vec<wamn_node_manifest::ConnectionRequirement>,
    purity: ResolvedPurity,
    recovery_class: RecoveryClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct HistoricalResolvedNodeContractV1 {
    interface: HistoricalResolvedNodeInterfaceV1,
    executable: ExecutableIdentity,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    connection_recovery_support: Vec<ConnectionRecoverySupport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    portable_connections: Vec<PortableConnectionRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct HistoricalResolvedComponentV1 {
    contract: HistoricalResolvedNodeContractV1,
}

fn project_historical_v1(
    historical: &HistoricalResolvedNodeContractV1,
) -> Result<ResolvedNodeContract, CatalogIdentityError> {
    if historical.interface.contract_version != HISTORICAL_RESOLVED_CONTRACT_VERSION {
        return Err(CatalogIdentityError::InvalidInterface {
            node_type: historical.interface.node_type.clone(),
            message: format!(
                "unsupported historical resolved contract version {:?}",
                historical.interface.contract_version
            ),
        });
    }
    let contract = ResolvedNodeContract {
        interface: ResolvedNodeInterface {
            contract_version: historical.interface.contract_version.clone(),
            node_type: historical.interface.node_type.clone(),
            interface_contract: historical.interface.interface_contract.clone(),
            output_ports: historical.interface.output_ports.clone(),
            capability_classes: historical.interface.capability_classes.clone(),
            connection_requirements: historical.interface.connection_requirements.clone(),
        },
        executable: historical.executable.clone(),
        executable_recovery: ExecutableRecoveryContract::historical_v1_projection(
            historical.interface.purity,
            historical.interface.recovery_class,
        ),
        connection_recovery_support: historical.connection_recovery_support.clone(),
        portable_connections: historical.portable_connections.clone(),
    };
    validate_executable_recovery(&contract)?;
    Ok(contract)
}

fn synthesize_legacy_contract(
    legacy: &LegacyResolvedNodeInterface,
    executable: ExecutableIdentity,
) -> ResolvedNodeContract {
    let executable_recovery =
        ExecutableRecoveryContract::historical_v1_projection(legacy.purity, legacy.recovery_class);
    ResolvedNodeContract {
        interface: synthesize_legacy_interface(legacy),
        executable,
        executable_recovery,
        connection_recovery_support: Vec::new(),
        portable_connections: Vec::new(),
    }
}

fn synthesize_legacy_interface(legacy: &LegacyResolvedNodeInterface) -> ResolvedNodeInterface {
    ResolvedNodeInterface {
        contract_version: LEGACY_RESOLVED_CONTRACT_VERSION.to_string(),
        node_type: legacy.node_type.clone(),
        interface_contract: LEGACY_INTERFACE_CONTRACT.to_string(),
        output_ports: legacy.output_ports.clone(),
        capability_classes: if legacy.purity == ResolvedPurity::Pure {
            vec![wamn_node_manifest::CapabilityClass::Pure]
        } else {
            Vec::new()
        },
        connection_requirements: Vec::new(),
    }
}

/// Exact canonical JSON bytes and typed semantics for one resolved interface bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceBundle {
    contracts: Box<[ResolvedNodeContract]>,
    interfaces: Box<[ResolvedNodeInterface]>,
    canonical_bytes: Box<[u8]>,
    hash: String,
}

/// Packaging boundary selected for an execution bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionBundlePackaging {
    ExactNode,
    CapabilityClass,
}

impl ExecutionBundlePackaging {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ExactNode => "exact-node",
            Self::CapabilityClass => "capability-class",
        }
    }
}

/// Immutable named bytes participating in execution-bundle composition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ExecutionBundleInput {
    identity: String,
    digest: String,
}

impl ExecutionBundleInput {
    pub fn new(
        identity: impl Into<String>,
        digest: impl Into<String>,
    ) -> Result<Self, CatalogIdentityError> {
        let identity = identity.into();
        let digest = digest.into();
        validate_text(&identity, "execution-bundle-input-identity")?;
        validate_digest(&digest, "execution-bundle-input-digest")?;
        Ok(Self { identity, digest })
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// Deterministic manifest for one executable plug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ExecutionPlugManifest {
    identity: String,
    node_types: Box<[String]>,
    component_digest: String,
}

impl ExecutionPlugManifest {
    pub fn new(
        identity: impl Into<String>,
        node_types: Vec<String>,
        component_digest: impl Into<String>,
    ) -> Result<Self, CatalogIdentityError> {
        let identity = identity.into();
        let component_digest = component_digest.into();
        validate_text(&identity, "execution-plug-identity")?;
        if node_types.is_empty() {
            return Err(CatalogIdentityError::InvalidDefinition {
                message: "execution plug must contain at least one node type".to_string(),
            });
        }
        validate_sorted_text(&node_types, "execution-plug-node-type")?;
        validate_digest(&component_digest, "execution-plug-component-digest")?;
        Ok(Self {
            identity,
            node_types: node_types.into_boxed_slice(),
            component_digest,
        })
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn node_types(&self) -> &[String] {
        &self.node_types
    }

    pub fn component_digest(&self) -> &str {
        &self.component_digest
    }
}

/// Builder for the complete byte-affecting execution-bundle input set.
#[derive(Debug, Clone)]
pub struct ExecutionBundleIdentityBuilder {
    packaging: ExecutionBundlePackaging,
    runner: ExecutionBundleInput,
    composition_tool: ExecutionBundleInput,
    implementations: Vec<NodeImplementation>,
    plugs: Vec<ExecutionPlugManifest>,
    adapters: Vec<ExecutionBundleInput>,
}

impl ExecutionBundleIdentityBuilder {
    pub fn implementations(mut self, implementations: Vec<NodeImplementation>) -> Self {
        self.implementations = implementations;
        self
    }

    pub fn plugs(mut self, plugs: Vec<ExecutionPlugManifest>) -> Self {
        self.plugs = plugs;
        self
    }

    pub fn adapters(mut self, adapters: Vec<ExecutionBundleInput>) -> Self {
        self.adapters = adapters;
        self
    }

    pub fn build(self) -> Result<ExecutionBundleIdentity, CatalogIdentityError> {
        validate_execution_bundle_inputs(&self)?;

        let mut owned = Vec::with_capacity(
            4 + self.implementations.len() + self.plugs.len() + self.adapters.len(),
        );
        owned.push((
            "identity-version",
            EXECUTION_BUNDLE_IDENTITY_VERSION.to_vec(),
        ));
        owned.push(("packaging-arm", self.packaging.as_str().as_bytes().to_vec()));
        owned.push(("runner", canonical_serialized(&self.runner)));
        for implementation in &self.implementations {
            owned.push((
                "resolved-node",
                canonical_serialized(implementation.contract()),
            ));
        }
        for plug in &self.plugs {
            owned.push(("plug-manifest", canonical_serialized(plug)));
        }
        for adapter in &self.adapters {
            owned.push(("adapter", canonical_serialized(adapter)));
        }
        owned.push((
            "composition-tool",
            canonical_serialized(&self.composition_tool),
        ));
        let borrowed = owned
            .iter()
            .map(|(tag, bytes)| (*tag, bytes.as_slice()))
            .collect::<Vec<_>>();
        let canonical_bytes = frames("execution-bundle", borrowed).into_boxed_slice();
        let hash = digest(&canonical_bytes);
        Ok(ExecutionBundleIdentity {
            hash,
            canonical_bytes,
        })
    }
}

/// Content key for a composed runner and every input that can change its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionBundleIdentity {
    hash: String,
    canonical_bytes: Box<[u8]>,
}

impl ExecutionBundleIdentity {
    pub fn builder(
        packaging: ExecutionBundlePackaging,
        runner: ExecutionBundleInput,
        composition_tool: ExecutionBundleInput,
    ) -> ExecutionBundleIdentityBuilder {
        ExecutionBundleIdentityBuilder {
            packaging,
            runner,
            composition_tool,
            implementations: Vec::new(),
            plugs: Vec::new(),
            adapters: Vec::new(),
        }
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }

    /// Record the exact inputs and output digest needed to verify a rebuild.
    pub fn provenance(&self, output: &[u8], composition_log: &[u8]) -> ExecutionBundleProvenance {
        ExecutionBundleProvenance::new(self, output, composition_log)
    }
}

/// Reproducible record binding one input key to exactly one composed output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionBundleProvenance {
    identity_hash: String,
    identity_bytes: Box<[u8]>,
    output_digest: String,
    composition_log: Box<[u8]>,
    canonical_bytes: Box<[u8]>,
}

impl ExecutionBundleProvenance {
    fn new(identity: &ExecutionBundleIdentity, output: &[u8], composition_log: &[u8]) -> Self {
        let output_digest = digest(output);
        let canonical_bytes = frames(
            "execution-bundle-provenance",
            [
                ("execution-bundle-key", identity.hash.as_bytes()),
                ("execution-bundle-inputs", identity.canonical_bytes.as_ref()),
                ("output-digest", output_digest.as_bytes()),
                ("composition-log", composition_log),
            ],
        )
        .into_boxed_slice();
        Self {
            identity_hash: identity.hash.clone(),
            identity_bytes: identity.canonical_bytes.clone(),
            output_digest,
            composition_log: composition_log.into(),
            canonical_bytes,
        }
    }

    pub fn identity_hash(&self) -> &str {
        &self.identity_hash
    }

    pub fn identity_bytes(&self) -> &[u8] {
        &self.identity_bytes
    }

    pub fn output_digest(&self) -> &str {
        &self.output_digest
    }

    pub fn composition_log(&self) -> &[u8] {
        &self.composition_log
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }

    /// Reject a cache entry or rebuild that differs in inputs or output bytes.
    pub fn verify_rebuild(
        &self,
        identity: &ExecutionBundleIdentity,
        output: &[u8],
    ) -> Result<(), CatalogIdentityError> {
        if self.identity_hash != identity.hash || self.identity_bytes != identity.canonical_bytes {
            return Err(CatalogIdentityError::ExecutionBundleIdentityMismatch);
        }
        if self.output_digest != digest(output) {
            return Err(CatalogIdentityError::ExecutionBundleOutputMismatch);
        }
        Ok(())
    }
}

impl InterfaceBundle {
    /// Build a canonical bundle from complete implementations resolved at publication.
    pub fn new(implementations: Vec<NodeImplementation>) -> Result<Self, CatalogIdentityError> {
        Self::from_implementations(&implementations)
    }

    fn from_implementations(
        implementations: &[NodeImplementation],
    ) -> Result<Self, CatalogIdentityError> {
        validate_implementation_order(implementations)?;
        let interfaces = implementations
            .iter()
            .map(|implementation| implementation.interface().clone())
            .collect::<Vec<_>>();
        validate_interface_order(&interfaces)?;
        let contracts = implementations
            .iter()
            .map(|implementation| implementation.contract().clone())
            .collect::<Vec<_>>();
        let value =
            serde_json::to_value(&contracts).expect("resolved node contracts serialize to JSON");
        let canonical_bytes = canonical_json(&value).into_boxed_slice();
        let hash = digest(&canonical_bytes);
        Ok(Self {
            contracts: contracts.into_boxed_slice(),
            interfaces: interfaces.into_boxed_slice(),
            canonical_bytes,
            hash,
        })
    }

    /// Parse exact RFC 8785 bytes from immutable storage.
    pub fn from_canonical_json(input: &str) -> Result<Self, CatalogIdentityError> {
        let value: Value = serde_json::from_str(input).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("resolved interface bundle JSON is invalid: {error}"),
            }
        })?;
        if canonical_json(&value) != input.as_bytes() {
            return Err(CatalogIdentityError::NonCanonicalJson);
        }
        let contracts: Vec<ResolvedNodeContract> = match serde_json::from_value(value.clone()) {
            Ok(contracts) => contracts,
            Err(current_error) => {
                if let Ok(historical) =
                    serde_json::from_value::<Vec<HistoricalResolvedNodeContractV1>>(value.clone())
                {
                    let implementations = historical
                        .iter()
                        .map(project_historical_v1)
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .map(|contract| NodeImplementation { contract })
                        .collect::<Vec<_>>();
                    let mut bundle = Self::from_implementations(&implementations)?;
                    bundle.canonical_bytes = input.as_bytes().into();
                    bundle.hash = digest(input.as_bytes());
                    return Ok(bundle);
                }
                let legacy: Vec<LegacyResolvedNodeInterface> = serde_json::from_value(value)
                    .map_err(|legacy_error| CatalogIdentityError::InvalidDefinition {
                        message: format!(
                            "resolved interface bundle shape is invalid: current: {current_error}; historical-v1/legacy: {legacy_error}"
                        ),
                    })?;
                let implementations = legacy
                    .iter()
                    .map(|interface| NodeImplementation {
                        contract: synthesize_legacy_contract(
                            interface,
                            ExecutableIdentity::Platform {
                                revision: LEGACY_PLATFORM_REVISION.to_string(),
                            },
                        ),
                    })
                    .collect::<Vec<_>>();
                let mut bundle = Self::from_implementations(&implementations)?;
                bundle.canonical_bytes = input.as_bytes().into();
                bundle.hash = digest(input.as_bytes());
                return Ok(bundle);
            }
        };
        let implementations = contracts
            .into_iter()
            .map(|contract| NodeImplementation { contract })
            .collect::<Vec<_>>();
        let bundle = Self::from_implementations(&implementations)?;
        Ok(bundle)
    }

    pub fn interfaces(&self) -> &[ResolvedNodeInterface] {
        &self.interfaces
    }

    pub fn contracts(&self) -> &[ResolvedNodeContract] {
        &self.contracts
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn resolved_ports(&self) -> ResolvedInterfaces {
        self.interfaces
            .iter()
            .map(|interface| (interface.node_type.clone(), interface.output_ports.clone()))
            .collect()
    }

    pub fn interface(&self, node_type: &str) -> Option<&ResolvedNodeInterface> {
        self.interfaces
            .binary_search_by(|interface| interface.node_type.as_str().cmp(node_type))
            .ok()
            .map(|index| &self.interfaces[index])
    }

    pub fn contract(&self, node_type: &str) -> Option<&ResolvedNodeContract> {
        self.contracts
            .binary_search_by(|contract| contract.interface.node_type.as_str().cmp(node_type))
            .ok()
            .map(|index| &self.contracts[index])
    }
}

/// A graph and resolved interfaces verified against one immutable artifact row.
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedArtifact {
    flow: Flow,
    interface_bundle: InterfaceBundle,
    occurrence_recovery: Box<[OccurrenceRecoverySelection]>,
}

impl PinnedArtifact {
    /// Verify storage bytes before exposing them to the runtime.
    #[expect(
        clippy::too_many_arguments,
        reason = "the immutable artifact row's identity and content columns are verified together"
    )]
    pub fn from_storage(
        expected_tenant_id: &str,
        expected_flow_id: &str,
        expected_flow_version: u32,
        graph_json: &str,
        graph_hash: &str,
        artifact_hash: &str,
        interface_bundle_json: &str,
        interface_bundle_hash: &str,
        component_digests_json: &str,
        occurrence_recovery_json: Option<&str>,
        occurrence_recovery_hash: Option<&str>,
    ) -> Result<Self, CatalogIdentityError> {
        validate_digest(artifact_hash, "artifact-hash")?;
        validate_digest(graph_hash, "graph-hash")?;
        validate_digest(interface_bundle_hash, "interface-bundle-hash")?;
        let flow = Flow::from_json(graph_json).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("flow graph JSON is invalid: {error}"),
            }
        })?;
        if flow.flow_id != expected_flow_id || flow.version != expected_flow_version {
            return Err(CatalogIdentityError::ArtifactIdMismatch);
        }
        if digest(&flow.canonical_bytes()) != graph_hash {
            return Err(CatalogIdentityError::GraphHashMismatch);
        }
        let interface_bundle = InterfaceBundle::from_canonical_json(interface_bundle_json)?;
        if interface_bundle.hash() != interface_bundle_hash {
            return Err(CatalogIdentityError::InterfaceBundleHashMismatch);
        }
        flow.validate(&interface_bundle.resolved_ports())
            .map_err(|issues| CatalogIdentityError::FlowInvalid {
                codes: issues.into_iter().map(|issue| issue.code).collect(),
            })?;
        if interface_bundle
            .contracts()
            .iter()
            .all(|contract| contract.interface.contract_version == LEGACY_RESOLVED_CONTRACT_VERSION)
        {
            require_historical_selection_absence(
                occurrence_recovery_json,
                occurrence_recovery_hash,
            )?;
            return Self::from_legacy_storage(
                expected_tenant_id,
                flow,
                artifact_hash,
                interface_bundle_json,
                interface_bundle,
                component_digests_json,
            );
        }
        if interface_bundle.contracts().iter().all(|contract| {
            contract.interface.contract_version == HISTORICAL_RESOLVED_CONTRACT_VERSION
        }) {
            require_historical_selection_absence(
                occurrence_recovery_json,
                occurrence_recovery_hash,
            )?;
            return Self::from_historical_v1_storage(
                expected_tenant_id,
                flow,
                artifact_hash,
                interface_bundle_json,
                interface_bundle,
                component_digests_json,
            );
        }
        let supplied_components: Vec<ResolvedComponent> =
            serde_json::from_str(component_digests_json).map_err(|error| {
                CatalogIdentityError::InvalidDefinition {
                    message: format!("supplied component digest bundle is invalid: {error}"),
                }
            })?;
        let mut components_by_node = BTreeMap::new();
        for component in supplied_components {
            let node_type = component.contract.interface.node_type.clone();
            if components_by_node
                .insert(node_type.clone(), component)
                .is_some()
            {
                return Err(CatalogIdentityError::DuplicateInterface { node_type });
            }
        }
        let mut implementations = Vec::with_capacity(interface_bundle.contracts().len());
        for contract in interface_bundle.contracts() {
            let interface = &contract.interface;
            let implementation =
                if let Some(component) = components_by_node.remove(&interface.node_type) {
                    if component.contract != *contract {
                        return Err(CatalogIdentityError::ArtifactHashMismatch);
                    }
                    NodeImplementation::from_resolved_component(component)?
                } else {
                    if !matches!(contract.executable, ExecutableIdentity::Platform { .. }) {
                        return Err(CatalogIdentityError::ArtifactHashMismatch);
                    }
                    NodeImplementation {
                        contract: contract.clone(),
                    }
                };
            implementations.push(implementation);
        }
        if let Some((node_type, _)) = components_by_node.pop_first() {
            return Err(CatalogIdentityError::UnexpectedInterface { node_type });
        }
        if !implementations.iter().all(|implementation| {
            implementation.interface().contract_version == RESOLVED_CONTRACT_VERSION
        }) {
            return Err(CatalogIdentityError::InvalidDefinition {
                message:
                    "a current persisted artifact must use the current resolved-contract version"
                        .to_string(),
            });
        }
        let occurrence_recovery = parse_persisted_occurrence_recovery(
            occurrence_recovery_json,
            occurrence_recovery_hash,
        )?;
        let reconstructed = Artifact::new_with_recovery_selections(
            expected_tenant_id,
            &flow,
            implementations,
            occurrence_recovery,
        )?;
        if reconstructed.identity().artifact_hash().as_str() != artifact_hash {
            return Err(CatalogIdentityError::ArtifactHashMismatch);
        }
        Ok(Self {
            flow,
            interface_bundle,
            occurrence_recovery: reconstructed.occurrence_recovery.clone(),
        })
    }

    fn from_historical_v1_storage(
        expected_tenant_id: &str,
        flow: Flow,
        artifact_hash: &str,
        interface_bundle_json: &str,
        interface_bundle: InterfaceBundle,
        component_digests_json: &str,
    ) -> Result<Self, CatalogIdentityError> {
        let historical: Vec<HistoricalResolvedNodeContractV1> =
            serde_json::from_str(interface_bundle_json).map_err(|error| {
                CatalogIdentityError::InvalidDefinition {
                    message: format!("historical v1 resolved contract bundle is invalid: {error}"),
                }
            })?;
        let historical_components: Vec<HistoricalResolvedComponentV1> =
            serde_json::from_str(component_digests_json).map_err(|error| {
                CatalogIdentityError::InvalidDefinition {
                    message: format!("historical v1 supplied component bundle is invalid: {error}"),
                }
            })?;
        let mut components_by_node = BTreeMap::new();
        for component in &historical_components {
            let node_type = component.contract.interface.node_type.clone();
            if components_by_node
                .insert(node_type.clone(), component)
                .is_some()
            {
                return Err(CatalogIdentityError::DuplicateInterface { node_type });
            }
        }
        let mut implementations = Vec::with_capacity(historical.len());
        for stored in &historical {
            let node_type = &stored.interface.node_type;
            let projected = project_historical_v1(stored)?;
            let implementation = if let Some(component) = components_by_node.remove(node_type) {
                if component.contract != *stored {
                    return Err(CatalogIdentityError::ArtifactHashMismatch);
                }
                NodeImplementation::from_resolved_component(ResolvedComponent {
                    contract: projected,
                })?
            } else {
                if !matches!(projected.executable, ExecutableIdentity::Platform { .. }) {
                    return Err(CatalogIdentityError::ArtifactHashMismatch);
                }
                NodeImplementation {
                    contract: projected,
                }
            };
            implementations.push(implementation);
        }
        if let Some((node_type, _)) = components_by_node.pop_first() {
            return Err(CatalogIdentityError::UnexpectedInterface { node_type });
        }
        validate_implementations_with_model_owned(
            &flow,
            &implementations,
            &HISTORICAL_MODEL_OWNED_NODES,
        )?;
        let id = ArtifactId::new(expected_tenant_id, flow.flow_id.clone(), flow.version)?;
        let mut owned = vec![
            (
                "artifact-id",
                canonical_json(&serde_json::to_value(&id).expect("artifact id serializes")),
            ),
            ("schema-version", flow.schema_version.as_bytes().to_vec()),
            ("graph", flow.canonical_bytes()),
        ];
        for contract in &historical {
            owned.push((
                "resolved-node",
                canonical_json(
                    &serde_json::to_value(contract)
                        .expect("historical v1 resolved contract serializes"),
                ),
            ));
        }
        let borrowed = owned
            .iter()
            .map(|(tag, bytes)| (*tag, bytes.as_slice()))
            .collect::<Vec<_>>();
        if digest(&frames("artifact", borrowed)) != artifact_hash {
            return Err(CatalogIdentityError::ArtifactHashMismatch);
        }
        let occurrence_recovery = conservative_occurrence_recovery_with_model_owned(
            &flow,
            &implementations,
            &HISTORICAL_MODEL_OWNED_NODES,
        )?;
        Ok(Self {
            flow,
            interface_bundle,
            occurrence_recovery: occurrence_recovery.into_boxed_slice(),
        })
    }

    fn from_legacy_storage(
        expected_tenant_id: &str,
        flow: Flow,
        artifact_hash: &str,
        interface_bundle_json: &str,
        mut interface_bundle: InterfaceBundle,
        component_digests_json: &str,
    ) -> Result<Self, CatalogIdentityError> {
        let legacy_interfaces: Vec<LegacyResolvedNodeInterface> =
            serde_json::from_str(interface_bundle_json).map_err(|error| {
                CatalogIdentityError::InvalidDefinition {
                    message: format!("legacy resolved interface bundle is invalid: {error}"),
                }
            })?;
        let legacy_components: Vec<LegacyResolvedComponent> =
            serde_json::from_str(component_digests_json).map_err(|error| {
                CatalogIdentityError::InvalidDefinition {
                    message: format!("legacy supplied component bundle is invalid: {error}"),
                }
            })?;
        let mut components_by_node = BTreeMap::new();
        for component in &legacy_components {
            validate_digest(&component.component_digest, "component-digest")?;
            let node_type = component.interface.node_type.clone();
            if components_by_node
                .insert(node_type.clone(), component)
                .is_some()
            {
                return Err(CatalogIdentityError::DuplicateInterface { node_type });
            }
        }
        let mut implementations = Vec::with_capacity(legacy_interfaces.len());
        for legacy in &legacy_interfaces {
            let contract = if let Some(component) = components_by_node.remove(&legacy.node_type) {
                if component.interface != *legacy {
                    return Err(CatalogIdentityError::ArtifactHashMismatch);
                }
                synthesize_legacy_contract(
                    legacy,
                    ExecutableIdentity::Component {
                        digest: component.component_digest.clone(),
                    },
                )
            } else {
                synthesize_legacy_contract(
                    legacy,
                    ExecutableIdentity::Platform {
                        revision: LEGACY_PLATFORM_REVISION.to_string(),
                    },
                )
            };
            implementations.push(NodeImplementation { contract });
        }
        if let Some((node_type, _)) = components_by_node.pop_first() {
            return Err(CatalogIdentityError::UnexpectedInterface { node_type });
        }
        validate_implementations_with_model_owned(
            &flow,
            &implementations,
            &HISTORICAL_MODEL_OWNED_NODES,
        )?;
        let id = ArtifactId::new(expected_tenant_id, flow.flow_id.clone(), flow.version)?;
        let mut owned = Vec::new();
        owned.push((
            "artifact-id",
            canonical_json(&serde_json::to_value(&id).expect("artifact id serializes")),
        ));
        owned.push(("schema-version", flow.schema_version.as_bytes().to_vec()));
        owned.push(("graph", flow.canonical_bytes()));
        for interface in &legacy_interfaces {
            owned.push((
                "interface",
                canonical_json(
                    &serde_json::to_value(interface).expect("legacy interface serializes"),
                ),
            ));
        }
        for component in &legacy_components {
            owned.push((
                "supplied-component",
                canonical_json(&serde_json::json!({
                    "component-digest": component.component_digest,
                    "node-type": component.interface.node_type,
                })),
            ));
        }
        let borrowed = owned
            .iter()
            .map(|(tag, bytes)| (*tag, bytes.as_slice()))
            .collect::<Vec<_>>();
        if digest(&frames("artifact", borrowed)) != artifact_hash {
            return Err(CatalogIdentityError::ArtifactHashMismatch);
        }
        interface_bundle.contracts = implementations
            .iter()
            .map(|implementation| implementation.contract().clone())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        interface_bundle.interfaces = implementations
            .iter()
            .map(|implementation| implementation.interface().clone())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let occurrence_recovery = conservative_occurrence_recovery_with_model_owned(
            &flow,
            &implementations,
            &HISTORICAL_MODEL_OWNED_NODES,
        )?
        .into_boxed_slice();
        Ok(Self {
            flow,
            interface_bundle,
            occurrence_recovery,
        })
    }

    pub fn flow(&self) -> &Flow {
        &self.flow
    }

    pub fn interface_bundle(&self) -> &InterfaceBundle {
        &self.interface_bundle
    }

    pub fn occurrence_recovery(&self) -> &[OccurrenceRecoverySelection] {
        &self.occurrence_recovery
    }
}

fn parse_persisted_occurrence_recovery(
    occurrence_recovery_json: Option<&str>,
    occurrence_recovery_hash: Option<&str>,
) -> Result<Vec<OccurrenceRecoverySelection>, CatalogIdentityError> {
    let (Some(json), Some(hash)) = (occurrence_recovery_json, occurrence_recovery_hash) else {
        return Err(CatalogIdentityError::InvalidDefinition {
            message: "current artifact omits occurrence recovery selections or hash".to_string(),
        });
    };
    validate_digest(hash, "occurrence-recovery-hash")?;
    if digest(json.as_bytes()) != hash {
        return Err(CatalogIdentityError::OccurrenceRecoveryHashMismatch);
    }
    let selections: Vec<OccurrenceRecoverySelection> =
        serde_json::from_str(json).map_err(|error| CatalogIdentityError::InvalidDefinition {
            message: format!("occurrence recovery selections are invalid: {error}"),
        })?;
    let canonical = canonical_json(
        &serde_json::to_value(&selections).expect("occurrence recovery selections serialize"),
    );
    if canonical != json.as_bytes() {
        return Err(CatalogIdentityError::NonCanonicalJson);
    }
    Ok(selections)
}

fn require_historical_selection_absence(
    occurrence_recovery_json: Option<&str>,
    occurrence_recovery_hash: Option<&str>,
) -> Result<(), CatalogIdentityError> {
    if occurrence_recovery_json.is_some() || occurrence_recovery_hash.is_some() {
        return Err(CatalogIdentityError::InvalidDefinition {
            message: "historical artifacts cannot carry unversioned occurrence recovery data"
                .to_string(),
        });
    }
    Ok(())
}

fn validate_interface_order(
    interfaces: &[ResolvedNodeInterface],
) -> Result<(), CatalogIdentityError> {
    let mut previous = None;
    for interface in interfaces {
        validate_text(&interface.node_type, "node-type")?;
        if previous.is_some_and(|value: &str| value >= interface.node_type.as_str()) {
            return if previous == Some(interface.node_type.as_str()) {
                Err(CatalogIdentityError::DuplicateInterface {
                    node_type: interface.node_type.clone(),
                })
            } else {
                Err(CatalogIdentityError::NonCanonicalInterfaceOrder {
                    node_type: interface.node_type.clone(),
                })
            };
        }
        validate_interface(interface)?;
        previous = Some(interface.node_type.as_str());
    }
    Ok(())
}

fn conservative_occurrence_recovery(
    flow: &Flow,
    implementations: &[NodeImplementation],
) -> Result<Vec<OccurrenceRecoverySelection>, CatalogIdentityError> {
    conservative_occurrence_recovery_with_model_owned(flow, implementations, &MODEL_OWNED_NODES)
}

fn conservative_occurrence_recovery_with_model_owned(
    flow: &Flow,
    implementations: &[NodeImplementation],
    model_owned_nodes: &[&str],
) -> Result<Vec<OccurrenceRecoverySelection>, CatalogIdentityError> {
    let mut selections = Vec::new();
    for node in &flow.nodes {
        if model_owned_nodes.contains(&node.node_type.as_str()) {
            continue;
        }
        let implementation = implementations
            .iter()
            .find(|implementation| implementation.interface().node_type == node.node_type)
            .ok_or_else(|| CatalogIdentityError::UnresolvedInterface {
                node_type: node.node_type.clone(),
            })?;
        selections.push(OccurrenceRecoverySelection::conservative(
            node.id.clone(),
            node.node_type.clone(),
            implementation.contract(),
        ));
    }
    selections.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    Ok(selections)
}

fn validate_occurrence_recovery(
    flow: &Flow,
    implementations: &[NodeImplementation],
    selections: &[OccurrenceRecoverySelection],
) -> Result<(), CatalogIdentityError> {
    let mut expected = flow
        .nodes
        .iter()
        .filter(|node| !MODEL_OWNED_NODES.contains(&node.node_type.as_str()))
        .map(|node| (node.id.as_str(), node.node_type.as_str()))
        .collect::<Vec<_>>();
    expected.sort_unstable();
    if selections.len() != expected.len() {
        return Err(CatalogIdentityError::InvalidDefinition {
            message: "recovery selections must cover every non-model-owned graph occurrence"
                .to_string(),
        });
    }
    for ((expected_id, expected_type), selection) in expected.into_iter().zip(selections) {
        if selection.selection_version != OCCURRENCE_RECOVERY_SELECTION_VERSION {
            return Err(CatalogIdentityError::InvalidDefinition {
                message: format!(
                    "unsupported occurrence recovery selection version {:?}",
                    selection.selection_version
                ),
            });
        }
        if selection.node_id != expected_id || selection.node_type != expected_type {
            return Err(CatalogIdentityError::InvalidDefinition {
                message: "recovery selections must be ordered by node id and match the graph"
                    .to_string(),
            });
        }
        let implementation = implementations
            .iter()
            .find(|implementation| implementation.interface().node_type == expected_type)
            .ok_or_else(|| CatalogIdentityError::UnresolvedInterface {
                node_type: expected_type.to_string(),
            })?;
        let recovery = implementation.contract().recovery_contract();
        if !recovery
            .supported_classes
            .contains(&selection.recovery_class)
            || recovery_rank(selection.recovery_class) < recovery_rank(recovery.conservative_class)
        {
            return Err(CatalogIdentityError::InvalidDefinition {
                message: format!(
                    "recovery selection for occurrence {:?} weakens or exceeds executable support",
                    selection.node_id
                ),
            });
        }
        match (&selection.recovery_class, &selection.portable_connection) {
            (RecoveryClass::Replay, None) | (RecoveryClass::NeverReplay, None) => {}
            (RecoveryClass::IdempotentWithKey, Some(requirement))
                if matches!(
                    requirement.recovery,
                    PortableRecoveryClaim::StableKeyDedupV1 { .. }
                ) && implementation
                    .contract()
                    .portable_connections
                    .contains(requirement) => {}
            (RecoveryClass::NeverReplay, Some(requirement))
                if matches!(requirement.recovery, PortableRecoveryClaim::NeverReplay)
                    && implementation
                        .contract()
                        .portable_connections
                        .contains(requirement) => {}
            _ => {
                return Err(CatalogIdentityError::InvalidDefinition {
                    message: format!(
                        "recovery selection for occurrence {:?} has no exact portable claim",
                        selection.node_id
                    ),
                });
            }
        }
    }
    Ok(())
}

const fn recovery_rank(class: RecoveryClass) -> u8 {
    match class {
        RecoveryClass::NeverReplay => 0,
        RecoveryClass::IdempotentWithKey => 1,
        RecoveryClass::Replay => 2,
    }
}

fn validate_implementations(
    flow: &Flow,
    implementations: &[NodeImplementation],
) -> Result<(), CatalogIdentityError> {
    validate_implementations_with_model_owned(flow, implementations, &MODEL_OWNED_NODES)
}

fn validate_implementations_with_model_owned(
    flow: &Flow,
    implementations: &[NodeImplementation],
    model_owned_nodes: &[&str],
) -> Result<(), CatalogIdentityError> {
    let mut resolved = BTreeSet::new();
    let interfaces: Vec<_> = implementations
        .iter()
        .map(|implementation| implementation.interface().clone())
        .collect();
    validate_interface_order(&interfaces)?;
    validate_implementation_order(implementations)?;
    for implementation in implementations {
        let interface = implementation.interface();
        resolved.insert(interface.node_type.clone());
    }

    let required: BTreeSet<_> = flow
        .nodes
        .iter()
        .map(|node| node.node_type.as_str())
        .filter(|node_type| !model_owned_nodes.contains(node_type))
        .collect();
    for node_type in &required {
        if !resolved.contains(*node_type) {
            return Err(CatalogIdentityError::UnresolvedInterface {
                node_type: (*node_type).to_string(),
            });
        }
    }
    for node_type in resolved {
        if !required.contains(node_type.as_str()) {
            return Err(CatalogIdentityError::UnexpectedInterface { node_type });
        }
    }
    Ok(())
}

fn validate_executable_recovery(
    contract: &ResolvedNodeContract,
) -> Result<(), CatalogIdentityError> {
    let interface = &contract.interface;
    if !matches!(
        interface.contract_version.as_str(),
        RESOLVED_CONTRACT_VERSION
            | HISTORICAL_RESOLVED_CONTRACT_VERSION
            | LEGACY_RESOLVED_CONTRACT_VERSION
    ) {
        return Err(CatalogIdentityError::InvalidInterface {
            node_type: interface.node_type.clone(),
            message: format!(
                "unsupported resolved contract version {:?}",
                interface.contract_version
            ),
        });
    }
    let recovery = &contract.executable_recovery;
    if recovery.contract_version != EXECUTABLE_RECOVERY_CONTRACT_VERSION {
        return Err(CatalogIdentityError::InvalidInterface {
            node_type: interface.node_type.clone(),
            message: format!(
                "unsupported executable recovery contract version {:?}",
                recovery.contract_version
            ),
        });
    }
    if recovery.supported_classes.is_empty()
        || recovery
            .supported_classes
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || !recovery
            .supported_classes
            .contains(&recovery.conservative_class)
    {
        return Err(CatalogIdentityError::InvalidInterface {
            node_type: interface.node_type.clone(),
            message: "executable recovery classes must be non-empty, sorted, unique, and contain the conservative class".to_string(),
        });
    }
    let current_valid = match recovery.purity {
        ResolvedPurity::Pure => {
            recovery.conservative_class == RecoveryClass::Replay
                && recovery.supported_classes == [RecoveryClass::Replay]
        }
        ResolvedPurity::Effectful => {
            recovery.conservative_class == RecoveryClass::NeverReplay
                && !recovery.supported_classes.contains(&RecoveryClass::Replay)
        }
    };
    let historical_valid = match recovery.purity {
        ResolvedPurity::Pure => recovery.conservative_class == RecoveryClass::Replay,
        ResolvedPurity::Effectful => recovery.conservative_class != RecoveryClass::Replay,
    } && recovery.supported_classes == [recovery.conservative_class];
    if (interface.contract_version == RESOLVED_CONTRACT_VERSION && !current_valid)
        || (interface.contract_version != RESOLVED_CONTRACT_VERSION && !historical_valid)
    {
        return Err(CatalogIdentityError::InvalidInterface {
            node_type: interface.node_type.clone(),
            message: "executable recovery semantics are invalid".to_string(),
        });
    }
    Ok(())
}

fn validate_implementation_order(
    implementations: &[NodeImplementation],
) -> Result<(), CatalogIdentityError> {
    for implementation in implementations {
        validate_interface(implementation.interface())?;
        validate_executable_recovery(implementation.contract())?;
        validate_connection_recovery_support(implementation.contract())?;
        validate_portable_connections(implementation.contract())?;
        match &implementation.contract().executable {
            ExecutableIdentity::Platform { revision } => {
                validate_text(revision, "platform-revision")?;
            }
            ExecutableIdentity::Component { digest } => {
                validate_digest(digest, "component-digest")?;
            }
        }
    }
    Ok(())
}

fn validate_connection_recovery_support(
    contract: &ResolvedNodeContract,
) -> Result<(), CatalogIdentityError> {
    if contract
        .connection_recovery_support
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(CatalogIdentityError::InvalidInterface {
            node_type: contract.interface.node_type.clone(),
            message: "connection recovery support must be sorted and unique".to_string(),
        });
    }
    for support in &contract.connection_recovery_support {
        validate_connection_descriptor(&support.descriptor)?;
        if !contract
            .interface
            .connection_requirements
            .iter()
            .any(|requirement| {
                requirement.requirement_type == support.descriptor.requirement_type
                    && requirement.contract == support.descriptor.contract
            })
        {
            return Err(CatalogIdentityError::InvalidInterface {
                node_type: contract.interface.node_type.clone(),
                message: format!(
                    "connection recovery support {:?} is absent from the resolved interface",
                    support.descriptor.contract
                ),
            });
        }
        if support.supported_modes.is_empty()
            || support
                .supported_modes
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err(CatalogIdentityError::InvalidInterface {
                node_type: contract.interface.node_type.clone(),
                message: format!(
                    "connection recovery modes for {:?} must be non-empty, sorted, and unique",
                    support.descriptor.contract
                ),
            });
        }
        if support.descriptor.conservative_recovery == RecoveryClass::NeverReplay
            && !support
                .supported_modes
                .contains(&ExecutableConnectionRecoveryMode::NeverReplay)
        {
            return Err(CatalogIdentityError::InvalidInterface {
                node_type: contract.interface.node_type.clone(),
                message: format!(
                    "connection recovery modes for {:?} omit its conservative never-replay mode",
                    support.descriptor.contract
                ),
            });
        }
        for mode in &support.supported_modes {
            match mode {
                ExecutableConnectionRecoveryMode::NeverReplay => {}
                ExecutableConnectionRecoveryMode::IdempotentWithKey {
                    claim: ExecutableRecoveryClaim::StableKeyDedupV1,
                    key_propagation,
                } => {
                    if *key_propagation != support.descriptor.idempotency_key_injection {
                        return Err(CatalogIdentityError::InvalidInterface {
                            node_type: contract.interface.node_type.clone(),
                            message: format!(
                                "stable-key-dedup-v1 key propagation does not match connection descriptor {:?}",
                                support.descriptor.contract
                            ),
                        });
                    }
                    if !descriptor_declares_stable_key_dedup(&support.descriptor) {
                        return Err(CatalogIdentityError::InvalidInterface {
                            node_type: contract.interface.node_type.clone(),
                            message: format!(
                                "stable-key-dedup-v1 is absent from connection descriptor {:?}",
                                support.descriptor.contract
                            ),
                        });
                    }
                }
            }
        }
    }
    // An empty set is an explicit conservative-only surface. Once a descriptor
    // declares richer support, require complete coverage so omissions cannot
    // selectively weaken one connection requirement.
    if !contract.connection_recovery_support.is_empty() {
        for requirement in &contract.interface.connection_requirements {
            if !contract.connection_recovery_support.iter().any(|support| {
                support.descriptor.requirement_type == requirement.requirement_type
                    && support.descriptor.contract == requirement.contract
            }) {
                return Err(CatalogIdentityError::InvalidInterface {
                    node_type: contract.interface.node_type.clone(),
                    message: format!(
                        "connection requirement {:?} has no executable recovery declaration",
                        requirement.contract
                    ),
                });
            }
        }
    }
    Ok(())
}

fn validate_portable_connections(
    contract: &ResolvedNodeContract,
) -> Result<(), CatalogIdentityError> {
    if contract
        .portable_connections
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(CatalogIdentityError::InvalidInterface {
            node_type: contract.interface.node_type.clone(),
            message: "portable connection requirements must be sorted and unique".to_string(),
        });
    }
    for requirement in &contract.portable_connections {
        validate_portable_connection_requirement(requirement)?;
        let descriptor = &requirement.descriptor;
        if !contract
            .interface
            .connection_requirements
            .iter()
            .any(|capability| {
                capability.requirement_type == descriptor.requirement_type
                    && capability.contract == descriptor.contract
            })
        {
            return Err(CatalogIdentityError::InvalidInterface {
                node_type: contract.interface.node_type.clone(),
                message: format!(
                    "portable connection {:?} is absent from the resolved interface",
                    descriptor.contract
                ),
            });
        }
        let supported = contract
            .connection_recovery_support
            .iter()
            .find(|support| support.descriptor == *descriptor)
            .is_some_and(|support| match requirement.recovery {
                PortableRecoveryClaim::NeverReplay => support
                    .supported_modes
                    .contains(&ExecutableConnectionRecoveryMode::NeverReplay),
                PortableRecoveryClaim::StableKeyDedupV1 { .. } => {
                    support.supported_modes.iter().any(|mode| {
                        matches!(
                            mode,
                            ExecutableConnectionRecoveryMode::IdempotentWithKey {
                                claim: ExecutableRecoveryClaim::StableKeyDedupV1,
                                key_propagation,
                            } if *key_propagation == descriptor.idempotency_key_injection
                        )
                    })
                }
            });
        if !supported {
            return Err(CatalogIdentityError::InvalidInterface {
                node_type: contract.interface.node_type.clone(),
                message: format!(
                    "portable recovery claim is unsupported for exact connection contract {:?}",
                    descriptor.contract
                ),
            });
        }
    }
    Ok(())
}

fn validate_portable_connection_requirement(
    requirement: &PortableConnectionRequirement,
) -> Result<(), CatalogIdentityError> {
    if requirement.requirement_version != PORTABLE_CONNECTION_REQUIREMENT_VERSION {
        return Err(CatalogIdentityError::InvalidDefinition {
            message: format!(
                "unsupported portable connection requirement version {:?}",
                requirement.requirement_version
            ),
        });
    }
    validate_connection_descriptor(&requirement.descriptor)?;
    match requirement.recovery {
        PortableRecoveryClaim::NeverReplay => {}
        PortableRecoveryClaim::StableKeyDedupV1 {
            minimum_retention_ms,
        } => {
            if minimum_retention_ms == 0 {
                return Err(CatalogIdentityError::InvalidDefinition {
                    message: "stable-key-dedup-v1 minimum-retention-ms must be positive"
                        .to_string(),
                });
            }
            let declares_claim = descriptor_declares_stable_key_dedup(&requirement.descriptor);
            if !declares_claim {
                return Err(CatalogIdentityError::InvalidDefinition {
                    message: "stable-key-dedup-v1 is absent from the connection descriptor"
                        .to_string(),
                });
            }
        }
    }
    Ok(())
}

fn descriptor_declares_stable_key_dedup(descriptor: &ConnectionTypeDescriptor) -> bool {
    descriptor.recovery_claims.iter().any(|claim| {
        matches!(
            claim,
            RecoveryClaimSchema::StableKeyDedupV1 { parameters, .. }
                if parameters == &[RecoveryClaimParameterSchema::MinimumRetentionMs]
        )
    })
}

fn validate_connection_descriptor(
    descriptor: &ConnectionTypeDescriptor,
) -> Result<(), CatalogIdentityError> {
    if descriptor.descriptor_version != CONNECTION_DESCRIPTOR_VERSION {
        return Err(CatalogIdentityError::InvalidDefinition {
            message: format!(
                "unsupported connection descriptor version {:?}",
                descriptor.descriptor_version
            ),
        });
    }
    validate_text(&descriptor.requirement_type, "connection-descriptor-type")?;
    validate_text(&descriptor.contract, "connection-descriptor-contract")?;
    let expected_prefix = format!("wamn:connection/{}@", descriptor.requirement_type);
    let Some(version) = descriptor.contract.strip_prefix(&expected_prefix) else {
        return Err(CatalogIdentityError::InvalidDefinition {
            message: "connection descriptor contract must match its requirement type".to_string(),
        });
    };
    if !is_three_part_numeric_version(version) {
        return Err(CatalogIdentityError::InvalidDefinition {
            message: "connection descriptor contract must carry an exact numeric version"
                .to_string(),
        });
    }

    let baseline = ConnectionTypeDescriptor::http_v1();
    if descriptor.requirement_type != baseline.requirement_type
        || descriptor.authority_model != baseline.authority_model
        || descriptor.field_ownership != baseline.field_ownership
        || descriptor.credential_injection != baseline.credential_injection
        || descriptor.idempotency_key_injection != baseline.idempotency_key_injection
        || descriptor.conservative_recovery != RecoveryClass::NeverReplay
        || descriptor.recovery_claims != baseline.recovery_claims
    {
        return Err(CatalogIdentityError::InvalidDefinition {
            message: "connection descriptor v1 has unsupported or non-canonical semantics"
                .to_string(),
        });
    }
    Ok(())
}

fn is_three_part_numeric_version(version: &str) -> bool {
    let mut parts = version.split('.');
    (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }) && parts.next().is_none()
}

fn validate_execution_bundle_inputs(
    builder: &ExecutionBundleIdentityBuilder,
) -> Result<(), CatalogIdentityError> {
    if builder.implementations.is_empty() || builder.plugs.is_empty() {
        return Err(CatalogIdentityError::InvalidDefinition {
            message: "execution bundle requires resolved nodes and executable plugs".to_string(),
        });
    }
    validate_implementation_order(&builder.implementations)?;
    let interfaces = builder
        .implementations
        .iter()
        .map(|implementation| implementation.interface().clone())
        .collect::<Vec<_>>();
    validate_interface_order(&interfaces)?;

    validate_named_order(
        builder.plugs.iter().map(ExecutionPlugManifest::identity),
        "execution-plug",
    )?;
    validate_named_order(
        builder.adapters.iter().map(ExecutionBundleInput::identity),
        "execution-adapter",
    )?;

    let mut packaged_nodes = BTreeSet::new();
    for plug in &builder.plugs {
        if builder.packaging == ExecutionBundlePackaging::ExactNode && plug.node_types.len() != 1 {
            return Err(CatalogIdentityError::InvalidDefinition {
                message: "exact-node plugs must contain exactly one node type".to_string(),
            });
        }
        for node_type in &plug.node_types {
            if !packaged_nodes.insert(node_type.as_str()) {
                return Err(CatalogIdentityError::InvalidDefinition {
                    message: format!("node type {node_type:?} appears in multiple plugs"),
                });
            }
        }
    }
    let resolved_node_count = interfaces.len();
    for interface in interfaces {
        if !packaged_nodes.contains(interface.node_type.as_str()) {
            return Err(CatalogIdentityError::InvalidDefinition {
                message: format!(
                    "resolved node type {:?} is absent from plug manifests",
                    interface.node_type
                ),
            });
        }
    }
    if builder.packaging == ExecutionBundlePackaging::ExactNode
        && packaged_nodes.len() != resolved_node_count
    {
        return Err(CatalogIdentityError::InvalidDefinition {
            message: "exact-node layout contains an unresolved node type".to_string(),
        });
    }
    Ok(())
}

fn validate_named_order<'a>(
    values: impl IntoIterator<Item = &'a str>,
    field: &'static str,
) -> Result<(), CatalogIdentityError> {
    let mut previous = None;
    for value in values {
        if previous.is_some_and(|candidate: &str| candidate >= value) {
            return Err(CatalogIdentityError::InvalidDefinition {
                message: format!("{field} values must be sorted and unique"),
            });
        }
        previous = Some(value);
    }
    Ok(())
}

fn validate_sorted_text(
    values: &[String],
    field: &'static str,
) -> Result<(), CatalogIdentityError> {
    let mut previous = None;
    for value in values {
        validate_text(value, field)?;
        if previous.is_some_and(|candidate: &str| candidate >= value.as_str()) {
            return Err(CatalogIdentityError::InvalidDefinition {
                message: format!("{field} values must be sorted and unique"),
            });
        }
        previous = Some(value.as_str());
    }
    Ok(())
}

fn validate_interface(interface: &ResolvedNodeInterface) -> Result<(), CatalogIdentityError> {
    validate_text(&interface.contract_version, "resolved-contract-version")?;
    validate_text(&interface.interface_contract, "interface-contract")?;
    if interface
        .capability_classes
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(CatalogIdentityError::InvalidInterface {
            node_type: interface.node_type.clone(),
            message: "capability classes must be sorted and unique".to_string(),
        });
    }
    for requirement in &interface.connection_requirements {
        validate_text(&requirement.requirement_type, "connection-requirement-type")?;
        validate_text(&requirement.contract, "connection-requirement-contract")?;
    }
    if interface
        .connection_requirements
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(CatalogIdentityError::InvalidInterface {
            node_type: interface.node_type.clone(),
            message: "connection requirements must be sorted and unique".to_string(),
        });
    }
    if interface.output_ports.is_empty() {
        return Err(CatalogIdentityError::InvalidInterface {
            node_type: interface.node_type.clone(),
            message: "at least one output port is required".to_string(),
        });
    }
    let mut previous = None;
    for port in &interface.output_ports {
        if port.is_empty() || port == "error" {
            return Err(CatalogIdentityError::InvalidInterface {
                node_type: interface.node_type.clone(),
                message: format!("invalid output port {port:?}"),
            });
        }
        if previous.is_some_and(|value: &str| value >= port.as_str()) {
            return Err(CatalogIdentityError::InvalidInterface {
                node_type: interface.node_type.clone(),
                message: "output ports must be sorted and unique".to_string(),
            });
        }
        previous = Some(port);
    }
    Ok(())
}

/// A source definition kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    Auth,
    CallerPolicy,
    Schedule,
}

/// A canonical immutable source definition.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    id: SourceId,
    kind: SourceKind,
    definition: CanonicalJson,
    canonical_bytes: Box<[u8]>,
}

impl Source {
    pub fn new(id: SourceId, kind: SourceKind, definition: CanonicalJson) -> Self {
        let kind_bytes = serde_json::to_vec(&kind).expect("source kind serializes");
        let canonical_bytes = frames(
            "source",
            [
                ("source-id", id.as_str().as_bytes()),
                ("kind", kind_bytes.as_slice()),
                ("definition", definition.as_bytes()),
            ],
        )
        .into_boxed_slice();
        Self {
            id,
            kind,
            definition,
            canonical_bytes,
        }
    }

    pub fn id(&self) -> &SourceId {
        &self.id
    }

    pub fn kind(&self) -> SourceKind {
        self.kind
    }

    pub fn definition(&self) -> &CanonicalJson {
        &self.definition
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }
}

/// An attachment exposure kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttachmentKind {
    Http,
    Internal,
    Studio,
    Cron,
}

/// Unresolved attachment input. Resolution is mandatory before hashing.
#[derive(Debug, Clone, PartialEq)]
pub struct AttachmentDraft {
    pub id: AttachmentId,
    pub kind: AttachmentKind,
    pub artifact_id: ArtifactId,
    pub source_ids: Vec<SourceId>,
    pub definition: CanonicalJson,
}

/// A canonical attachment definition with resolved sources.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    id: AttachmentId,
    kind: AttachmentKind,
    artifact: ArtifactIdentity,
    source_ids: Vec<SourceId>,
    resolved_sources: Vec<Source>,
    definition: CanonicalJson,
    definition_hash: DefinitionHash,
    canonical_bytes: Box<[u8]>,
}

impl Attachment {
    /// Resolve every source and compute the complete effective-contract hash.
    pub fn resolve(
        draft: AttachmentDraft,
        artifact: &Artifact,
        sources: &[Source],
    ) -> Result<Self, CatalogIdentityError> {
        if draft.artifact_id != *artifact.identity.id() {
            return Err(CatalogIdentityError::ArtifactMismatch);
        }
        validate_sorted_unique(&draft.source_ids, "source-ids", |id| {
            id.as_str().to_string()
        })?;
        let available: BTreeMap<_, _> = sources
            .iter()
            .map(|source| (source.id().clone(), source))
            .collect();
        let resolved: Vec<_> = draft
            .source_ids
            .iter()
            .map(|id| {
                available
                    .get(id)
                    .copied()
                    .ok_or_else(|| CatalogIdentityError::UnresolvedSource {
                        source_id: id.as_str().to_string(),
                    })
            })
            .collect::<Result<_, _>>()?;

        let kind_bytes = serde_json::to_vec(&draft.kind).expect("attachment kind serializes");
        let mut owned = vec![
            ("attachment-id", draft.id.as_str().as_bytes().to_vec()),
            ("kind", kind_bytes),
            ("artifact", artifact.identity.canonical_bytes()),
            ("definition", draft.definition.as_bytes().to_vec()),
        ];
        for source in &resolved {
            owned.push(("resolved-source", source.canonical_bytes().to_vec()));
        }
        let borrowed: Vec<_> = owned
            .iter()
            .map(|(tag, bytes)| (*tag, bytes.as_slice()))
            .collect();
        let canonical_bytes = frames("attachment-definition", borrowed).into_boxed_slice();
        let definition_hash = DefinitionHash(digest(&canonical_bytes));
        Ok(Self {
            id: draft.id,
            kind: draft.kind,
            artifact: artifact.identity.clone(),
            source_ids: draft.source_ids,
            resolved_sources: resolved.into_iter().cloned().collect(),
            definition: draft.definition,
            definition_hash,
            canonical_bytes,
        })
    }

    pub fn id(&self) -> &AttachmentId {
        &self.id
    }

    pub fn kind(&self) -> AttachmentKind {
        self.kind
    }

    pub fn artifact(&self) -> &ArtifactIdentity {
        &self.artifact
    }

    pub fn source_ids(&self) -> &[SourceId] {
        &self.source_ids
    }

    pub fn definition(&self) -> &CanonicalJson {
        &self.definition
    }

    pub fn definition_hash(&self) -> &DefinitionHash {
        &self.definition_hash
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }
}

/// A canonical immutable release and its fully resolved members.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    id: ReleaseId,
    artifacts: Vec<ArtifactIdentity>,
    sources: Vec<Source>,
    attachments: Vec<Attachment>,
    canonical_bytes: Box<[u8]>,
}

impl Release {
    pub fn new(
        id: ReleaseId,
        artifacts: Vec<ArtifactIdentity>,
        sources: Vec<Source>,
        attachments: Vec<Attachment>,
    ) -> Result<Self, CatalogIdentityError> {
        validate_sorted_unique(&artifacts, "artifacts", |artifact| {
            format!(
                "{}/{}/{:010}",
                artifact.id.tenant_id, artifact.id.flow_id, artifact.id.flow_version
            )
        })?;
        validate_sorted_unique(&sources, "sources", |source| source.id.to_string())?;
        validate_sorted_unique(&attachments, "attachments", |attachment| {
            attachment.id.to_string()
        })?;
        if artifacts
            .iter()
            .any(|artifact| artifact.id.tenant_id != id.tenant_id)
        {
            return Err(CatalogIdentityError::ArtifactMismatch);
        }

        let artifact_map: BTreeMap<_, _> = artifacts
            .iter()
            .map(|artifact| (artifact.id.clone(), artifact))
            .collect();
        let source_map: BTreeMap<_, _> = sources
            .iter()
            .map(|source| (source.id.clone(), source))
            .collect();
        for attachment in &attachments {
            let Some(artifact) = artifact_map.get(&attachment.artifact.id) else {
                return Err(CatalogIdentityError::ArtifactMismatch);
            };
            if *artifact != &attachment.artifact {
                return Err(CatalogIdentityError::ArtifactMismatch);
            }
            for (source_id, resolved_source) in attachment
                .source_ids
                .iter()
                .zip(&attachment.resolved_sources)
            {
                let Some(source) = source_map.get(source_id) else {
                    return Err(CatalogIdentityError::UnresolvedSource {
                        source_id: source_id.to_string(),
                    });
                };
                if *source != resolved_source {
                    return Err(CatalogIdentityError::SourceMismatch {
                        source_id: source_id.to_string(),
                    });
                }
            }
        }

        let id_bytes = canonical_json(&serde_json::to_value(&id).expect("release id serializes"));
        let mut owned = vec![("release-id", id_bytes)];
        for artifact in &artifacts {
            owned.push(("artifact", artifact.canonical_bytes()));
        }
        for source in &sources {
            owned.push(("source", source.canonical_bytes().to_vec()));
        }
        for attachment in &attachments {
            owned.push(("attachment", attachment.canonical_bytes().to_vec()));
        }
        let borrowed: Vec<_> = owned
            .iter()
            .map(|(tag, bytes)| (*tag, bytes.as_slice()))
            .collect();
        let canonical_bytes = frames("release", borrowed).into_boxed_slice();
        Ok(Self {
            id,
            artifacts,
            sources,
            attachments,
            canonical_bytes,
        })
    }

    pub fn id(&self) -> &ReleaseId {
        &self.id
    }

    pub fn artifacts(&self) -> &[ArtifactIdentity] {
        &self.artifacts
    }

    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    pub fn attachments(&self) -> &[Attachment] {
        &self.attachments
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }
}

fn validate_sorted_unique<T>(
    values: &[T],
    field: &'static str,
    key: impl Fn(&T) -> String,
) -> Result<(), CatalogIdentityError> {
    let mut previous: Option<String> = None;
    for value in values {
        let current = key(value);
        if let Some(previous) = &previous {
            match previous.cmp(&current) {
                Ordering::Equal => {
                    return Err(CatalogIdentityError::DuplicateMember { field, id: current });
                }
                Ordering::Greater => {
                    return Err(CatalogIdentityError::NonCanonicalMemberOrder {
                        field,
                        id: current,
                    });
                }
                Ordering::Less => {}
            }
        }
        previous = Some(current);
    }
    Ok(())
}

/// Current activation state for one attachment. It never participates in hashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentActivation {
    tenant_id: String,
    catalog_id: String,
    environment: String,
    attachment_id: AttachmentId,
    confirmed_definition_hash: DefinitionHash,
    enabled: bool,
}

impl AttachmentActivation {
    pub fn new(
        tenant_id: impl Into<String>,
        catalog_id: impl Into<String>,
        environment: impl Into<String>,
        attachment_id: AttachmentId,
        confirmed_definition_hash: DefinitionHash,
        enabled: bool,
    ) -> Result<Self, CatalogIdentityError> {
        let tenant_id = tenant_id.into();
        let catalog_id = catalog_id.into();
        let environment = environment.into();
        validate_text(&tenant_id, "tenant-id")?;
        validate_text(&catalog_id, "catalog-id")?;
        validate_text(&environment, "environment")?;
        Ok(Self {
            tenant_id,
            catalog_id,
            environment,
            attachment_id,
            confirmed_definition_hash,
            enabled,
        })
    }

    pub fn definition_is_live(&self, hash: &DefinitionHash) -> bool {
        self.enabled && self.confirmed_definition_hash == *hash
    }

    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    pub fn catalog_id(&self) -> &str {
        &self.catalog_id
    }

    pub fn environment(&self) -> &str {
        &self.environment
    }

    pub fn attachment_id(&self) -> &AttachmentId {
        &self.attachment_id
    }

    pub fn confirmed_definition_hash(&self) -> &DefinitionHash {
        &self.confirmed_definition_hash
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Stable applied-release head and lock identity for one environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogHead {
    tenant_id: String,
    catalog_id: String,
    environment: String,
    applied_version: u32,
}

impl CatalogHead {
    pub fn new(
        tenant_id: impl Into<String>,
        catalog_id: impl Into<String>,
        environment: impl Into<String>,
        applied_version: u32,
    ) -> Result<Self, CatalogIdentityError> {
        let tenant_id = tenant_id.into();
        let catalog_id = catalog_id.into();
        let environment = environment.into();
        validate_text(&tenant_id, "tenant-id")?;
        validate_text(&catalog_id, "catalog-id")?;
        validate_text(&environment, "environment")?;
        if applied_version == 0 {
            return Err(CatalogIdentityError::ZeroVersion {
                field: "applied-version",
            });
        }
        Ok(Self {
            tenant_id,
            catalog_id,
            environment,
            applied_version,
        })
    }

    pub fn applied_version(&self) -> u32 {
        self.applied_version
    }

    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    pub fn catalog_id(&self) -> &str {
        &self.catalog_id
    }

    pub fn environment(&self) -> &str {
        &self.environment
    }
}

fn frames<'a>(domain: &str, values: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Vec<u8> {
    let mut output = Vec::new();
    write_frame(&mut output, IDENTITY_FORMAT);
    write_frame(&mut output, domain.as_bytes());
    for (tag, value) in values {
        write_frame(&mut output, tag.as_bytes());
        write_frame(&mut output, value);
    }
    output
}

fn write_frame(output: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("identity field length fits u64");
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value);
}

fn canonical_json(value: &Value) -> Vec<u8> {
    let mut output = Vec::new();
    write_json(value, &mut output);
    output
}

fn canonical_serialized(value: &impl Serialize) -> Vec<u8> {
    canonical_json(&serde_json::to_value(value).expect("identity input serializes"))
}

fn write_json(value: &Value, output: &mut Vec<u8>) {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(true) => output.extend_from_slice(b"true"),
        Value::Bool(false) => output.extend_from_slice(b"false"),
        Value::Number(number) => {
            let number = number
                .as_f64()
                .expect("serde_json numbers are finite IEEE-754 values");
            output.extend_from_slice(ecma_number(number).as_bytes());
        }
        Value::String(value) => output.extend_from_slice(
            serde_json::to_string(value)
                .expect("string serializes")
                .as_bytes(),
        ),
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_json(value, output);
            }
            output.push(b']');
        }
        Value::Object(values) => {
            output.push(b'{');
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.encode_utf16().cmp(right.encode_utf16()));
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                output.extend_from_slice(
                    serde_json::to_string(key)
                        .expect("object key serializes")
                        .as_bytes(),
                );
                output.push(b':');
                write_json(value, output);
            }
            output.push(b'}');
        }
    }
}

fn ecma_number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    let negative = value.is_sign_negative();
    let wire = serde_json::to_string(&value.abs()).expect("finite number serializes");
    let (mantissa, exponent) =
        wire.split_once(['e', 'E'])
            .map_or((wire.as_str(), 0), |(mantissa, exponent)| {
                (
                    mantissa,
                    exponent
                        .parse::<i32>()
                        .expect("serializer emits a valid exponent"),
                )
            });
    let decimal = mantissa.find('.').unwrap_or(mantissa.len());
    let mut digits: String = mantissa
        .chars()
        .filter(|character| *character != '.')
        .collect();
    let mut point = i32::try_from(decimal).expect("number length fits i32") + exponent;
    let leading = digits.bytes().take_while(|byte| *byte == b'0').count();
    if leading != 0 {
        digits.drain(..leading);
        point -= i32::try_from(leading).expect("number length fits i32");
    }
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    let count = i32::try_from(digits.len()).expect("number length fits i32");
    let mut output = String::new();
    if negative {
        output.push('-');
    }
    if point > 0 && point <= 21 {
        if point >= count {
            output.push_str(&digits);
            for _ in 0..(point - count) {
                output.push('0');
            }
        } else {
            let point = usize::try_from(point).expect("positive point");
            output.push_str(&digits[..point]);
            output.push('.');
            output.push_str(&digits[point..]);
        }
    } else if point <= 0 && point > -6 {
        output.push_str("0.");
        for _ in 0..-point {
            output.push('0');
        }
        output.push_str(&digits);
    } else {
        output.push(digits.as_bytes()[0] as char);
        if digits.len() > 1 {
            output.push('.');
            output.push_str(&digits[1..]);
        }
        let exponent = point - 1;
        output.push('e');
        if exponent >= 0 {
            output.push('+');
        }
        output.push_str(&exponent.to_string());
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{digest, frames};

    #[test]
    fn named_removal_and_reordering_mutants_change_every_hash_frame() {
        for (domain, fields) in [
            (
                "artifact",
                vec![
                    ("artifact-id", b"id".as_slice()),
                    ("schema-version", b"schema".as_slice()),
                    ("graph", b"graph".as_slice()),
                    ("interface", b"interface".as_slice()),
                    ("supplied-component", b"component".as_slice()),
                ],
            ),
            (
                "attachment-definition",
                vec![
                    ("attachment-id", b"id".as_slice()),
                    ("kind", b"kind".as_slice()),
                    ("artifact", b"artifact".as_slice()),
                    ("definition", b"definition".as_slice()),
                    ("resolved-source", b"source".as_slice()),
                ],
            ),
        ] {
            let baseline = digest(&frames(domain, fields.iter().copied()));
            for (index, (tag, _)) in fields.iter().enumerate() {
                let removal: Vec<_> = fields
                    .iter()
                    .enumerate()
                    .filter(|(candidate, _)| *candidate != index)
                    .map(|(_, field)| *field)
                    .collect();
                assert_ne!(
                    baseline,
                    digest(&frames(domain, removal)),
                    "named removal mutant {domain}.{tag} survived"
                );

                let mut reordering = fields.clone();
                let field = reordering.remove(index);
                let destination = if index + 1 == fields.len() {
                    0
                } else {
                    index + 1
                };
                reordering.insert(destination, field);
                assert_ne!(
                    baseline,
                    digest(&frames(domain, reordering)),
                    "named reordering mutant {domain}.{tag} survived"
                );
            }
        }
    }
}

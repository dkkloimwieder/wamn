//! Canonical immutable catalog identities and definition hashing.
//!
//! This crate owns only the pure definition plane. Persistence, publication,
//! activation transitions, and compatibility readers live in effect crates.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use wamn_flow::{Flow, ResolvedInterfaces};
use wamn_node_manifest::{ResolvedComponent, ResolvedNodeInterface, ResolvedPurity};

const HASH_PREFIX: &str = "sha256:";
const HASH_HEX_LEN: usize = 64;
const IDENTITY_FORMAT: &[u8] = b"wamn.catalog.identity.v1";
const MODEL_OWNED_NODES: [&str; 5] = ["cron", "event", "fail", "request", "respond"];

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
    interface: ResolvedNodeInterface,
    component_digest: Option<String>,
}

impl NodeImplementation {
    pub fn platform(interface: ResolvedNodeInterface) -> Self {
        Self {
            interface,
            component_digest: None,
        }
    }

    pub fn supplied(
        interface: ResolvedNodeInterface,
        component_digest: impl Into<String>,
    ) -> Result<Self, CatalogIdentityError> {
        let component_digest = component_digest.into();
        validate_digest(&component_digest, "component-digest")?;
        Ok(Self {
            interface,
            component_digest: Some(component_digest),
        })
    }

    pub fn from_resolved_component(
        component: ResolvedComponent,
    ) -> Result<Self, CatalogIdentityError> {
        validate_digest(&component.component_digest, "component-digest")?;
        Ok(Self {
            interface: component.interface,
            component_digest: Some(component.component_digest),
        })
    }

    pub fn interface(&self) -> &ResolvedNodeInterface {
        &self.interface
    }

    pub fn component_digest(&self) -> Option<&str> {
        self.component_digest.as_deref()
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
    canonical_bytes: Box<[u8]>,
}

impl Artifact {
    /// Build an artifact from its graph and fully resolved implementations.
    pub fn new(
        tenant_id: impl Into<String>,
        flow: &Flow,
        implementations: Vec<NodeImplementation>,
    ) -> Result<Self, CatalogIdentityError> {
        let id = ArtifactId::new(tenant_id, flow.flow_id.clone(), flow.version)?;
        validate_text(&flow.schema_version, "schema-version")?;
        validate_implementations(flow, &implementations)?;

        let interfaces: Vec<_> = implementations
            .iter()
            .map(|implementation| implementation.interface.clone())
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
        for interface in &interfaces {
            owned.push((
                "interface",
                canonical_json(
                    &serde_json::to_value(interface).expect("resolved interface serializes"),
                ),
            ));
        }
        let supplied_components: Vec<_> = implementations
            .iter()
            .filter_map(|implementation| {
                implementation
                    .component_digest
                    .as_ref()
                    .map(|component_digest| ResolvedComponent {
                        interface: implementation.interface.clone(),
                        component_digest: component_digest.clone(),
                    })
            })
            .collect();
        for implementation in implementations
            .iter()
            .filter(|implementation| implementation.component_digest.is_some())
        {
            owned.push((
                "supplied-component",
                canonical_json(&serde_json::json!({
                    "component-digest": implementation.component_digest,
                    "node-type": implementation.interface.node_type,
                })),
            ));
        }
        let borrowed: Vec<_> = owned
            .iter()
            .map(|(tag, bytes)| (*tag, bytes.as_slice()))
            .collect();
        let canonical_bytes = frames("artifact", borrowed).into_boxed_slice();
        let artifact_hash = ArtifactHash(digest(&canonical_bytes));
        let interface_bundle = InterfaceBundle::new(interfaces)?;
        Ok(Self {
            identity: ArtifactIdentity { id, artifact_hash },
            schema_version: flow.schema_version.clone(),
            graph_hash,
            interface_bundle,
            supplied_components,
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

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }
}

/// Exact canonical JSON bytes and typed semantics for one resolved interface bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceBundle {
    interfaces: Box<[ResolvedNodeInterface]>,
    canonical_bytes: Box<[u8]>,
    hash: String,
}

impl InterfaceBundle {
    /// Build a canonical bundle from interfaces already resolved at publication.
    pub fn new(interfaces: Vec<ResolvedNodeInterface>) -> Result<Self, CatalogIdentityError> {
        validate_interface_order(&interfaces)?;
        let value =
            serde_json::to_value(&interfaces).expect("resolved interfaces serialize to JSON");
        let canonical_bytes = canonical_json(&value).into_boxed_slice();
        let hash = digest(&canonical_bytes);
        Ok(Self {
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
        let interfaces: Vec<ResolvedNodeInterface> = serde_json::from_value(value.clone())
            .map_err(|error| CatalogIdentityError::InvalidDefinition {
                message: format!("resolved interface bundle shape is invalid: {error}"),
            })?;
        let bundle = Self::new(interfaces)?;
        if bundle.canonical_bytes.as_ref() != input.as_bytes() {
            return Err(CatalogIdentityError::NonCanonicalJson);
        }
        Ok(bundle)
    }

    pub fn interfaces(&self) -> &[ResolvedNodeInterface] {
        &self.interfaces
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
}

/// A graph and resolved interfaces verified against one immutable artifact row.
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedArtifact {
    flow: Flow,
    interface_bundle: InterfaceBundle,
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
        let supplied_components: Vec<ResolvedComponent> =
            serde_json::from_str(component_digests_json).map_err(|error| {
                CatalogIdentityError::InvalidDefinition {
                    message: format!("supplied component digest bundle is invalid: {error}"),
                }
            })?;
        let mut components_by_node = BTreeMap::new();
        for component in supplied_components {
            let node_type = component.interface.node_type.clone();
            if components_by_node
                .insert(node_type.clone(), component)
                .is_some()
            {
                return Err(CatalogIdentityError::DuplicateInterface { node_type });
            }
        }
        let mut implementations = Vec::with_capacity(interface_bundle.interfaces().len());
        for interface in interface_bundle.interfaces() {
            let implementation =
                if let Some(component) = components_by_node.remove(&interface.node_type) {
                    if component.interface != *interface {
                        return Err(CatalogIdentityError::ArtifactHashMismatch);
                    }
                    NodeImplementation::supplied(interface.clone(), component.component_digest)?
                } else {
                    NodeImplementation::platform(interface.clone())
                };
            implementations.push(implementation);
        }
        if let Some((node_type, _)) = components_by_node.pop_first() {
            return Err(CatalogIdentityError::UnexpectedInterface { node_type });
        }
        let reconstructed = Artifact::new(expected_tenant_id, &flow, implementations)?;
        if reconstructed.identity().artifact_hash().as_str() != artifact_hash {
            return Err(CatalogIdentityError::ArtifactHashMismatch);
        }
        Ok(Self {
            flow,
            interface_bundle,
        })
    }

    pub fn flow(&self) -> &Flow {
        &self.flow
    }

    pub fn interface_bundle(&self) -> &InterfaceBundle {
        &self.interface_bundle
    }
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

fn validate_implementations(
    flow: &Flow,
    implementations: &[NodeImplementation],
) -> Result<(), CatalogIdentityError> {
    let mut resolved = BTreeSet::new();
    let interfaces: Vec<_> = implementations
        .iter()
        .map(|implementation| implementation.interface.clone())
        .collect();
    validate_interface_order(&interfaces)?;
    for implementation in implementations {
        let interface = &implementation.interface;
        resolved.insert(interface.node_type.clone());
    }

    let required: BTreeSet<_> = flow
        .nodes
        .iter()
        .map(|node| node.node_type.as_str())
        .filter(|node_type| !MODEL_OWNED_NODES.contains(node_type))
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

fn validate_interface(interface: &ResolvedNodeInterface) -> Result<(), CatalogIdentityError> {
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
    let recovery_matches = matches!(
        (interface.purity, interface.recovery_class),
        (
            ResolvedPurity::Pure,
            wamn_node_manifest::RecoveryClass::Replay
        ) | (
            ResolvedPurity::Effectful,
            wamn_node_manifest::RecoveryClass::NeverReplay
        )
    );
    if !recovery_matches {
        return Err(CatalogIdentityError::InvalidInterface {
            node_type: interface.node_type.clone(),
            message: "purity and recovery class disagree".to_string(),
        });
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

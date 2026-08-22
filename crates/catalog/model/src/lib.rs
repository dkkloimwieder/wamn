//! Canonical immutable catalog identities and definition hashing.
//!
//! MVP outcome: provisioning · publish · additive schema · tenant isolation (T1 minting).
//!
//! This crate owns the pure definition plane, and — since wamn-0h0g.18.2 — the
//! statement text of the wiring-activation verb over it. The pointer's write and
//! its env-hot read are one contract confirming one definition hash, and this is
//! the only crate the management driver and the serving plane both already
//! depend on. Persistence, publication, activation transitions, and
//! compatibility readers still live in effect crates: no connection,
//! transaction, or clock is reachable from here.

mod component_library;
mod execution_node_id;
mod execution_plan;
mod serving_manifest;
mod wiring;
mod wiring_activation;
mod wiring_compatibility;

pub use component_library::{
    AdmittedComponent, AdmittedComponentParameter, AdmittedComponentPort, ComponentCatalogScope,
    ComponentDeclaration, ComponentFactError, ComponentFactErrorKind,
    ComponentParameterDeclaration, ComponentPortDeclaration, ComponentSchema,
    normalize_component_fact, schema_digests_match,
};
pub use execution_node_id::{ExecutionNodeId, ExecutionNodeIdError};
pub use execution_plan::{
    CALLABLE_CONTRACT_VERSION, CallFlowInstruction, CallableContract, CallableEffectCeiling,
    CallableReturnContract, EXECUTION_PLAN_FORMAT_VERSION, ExecutionConnectionRequirement,
    ExecutionEffectPolicy, ExecutionPlanBody, ExecutionPlanEdge, ExecutionPlanHeader,
    ExecutionPlanNode, ExecutionPlanV2, ExecutionRuntimeRevision, ExecutionSourceMapEntry,
    HOST_EFFECT_CONTRACT_VERSION, PLAN_COMPILER_REVISION, RootTerminalBehavior,
    entry_input_schema_hash, execution_bundle_hash, read_execution_plan,
};
pub use serving_manifest::{
    MAX_SERVING_MANIFEST_BYTES, RELEASE_MANIFEST_CONFIGMAP_PREFIX, RELEASE_MANIFEST_FILE_NAME,
    RELEASE_MANIFEST_MOUNT_PATH, SERVING_MANIFEST_FORMAT_VERSION, ServingAttachment,
    ServingComponent, ServingManifest, ServingRegistration, ServingRelease, ServingWiring,
    UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL, release_manifest_configmap_name,
};
pub use wiring::{
    WIRING_DOCUMENT_FORMAT_VERSION, WiringDocument, WiringEdge, WiringNode, WiringTerminal,
};
pub use wiring_activation::{
    WIRING_ACTIVATION_CHANNEL, WiringActivationNotice, flip_activation,
    previous_confirmed_definition, record_activation_event, resolve_active_wiring,
};
pub use wiring_compatibility::{
    WiringCompatibilityError, WiringCompatibilityErrorKind, validate_wiring_compatibility,
};

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use wamn_flow::node_contract::NodeInterface;
use wamn_flow::{Flow, FlowPreimage, ResolvedInterfaces};

const HASH_PREFIX: &str = "sha256:";
const HASH_HEX_LEN: usize = 64;
const IDENTITY_FORMAT: &[u8] = b"wamn.catalog.identity.v0.1";
const MODEL_OWNED_NODES: [&str; 1] = ["call-flow"];

/// A catalog identity construction error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogIdentityError {
    EmptyIdentity {
        field: &'static str,
    },
    NonCanonicalIdentity {
        field: &'static str,
    },
    ZeroVersion {
        field: &'static str,
    },
    InvalidDigest {
        field: &'static str,
    },
    InvalidDefinition {
        message: String,
    },
    NonCanonicalJson,
    MutableIdentityInput {
        field: String,
    },
    DuplicateInterface {
        node_type: String,
    },
    NonCanonicalInterfaceOrder {
        node_type: String,
    },
    InvalidInterface {
        node_type: String,
        message: String,
    },
    GraphHashMismatch,
    DraftContentHashMismatch,
    ValidatedDraftIdentityMismatch,
    ArtifactIdMismatch,
    ArtifactHashMismatch,
    ExecutionBundleHashMismatch,
    UnresolvedInterface {
        node_type: String,
    },
    UnexpectedInterface {
        node_type: String,
    },
    FlowInvalid {
        codes: Vec<&'static str>,
    },
    DuplicateMember {
        field: &'static str,
        id: String,
    },
    NonCanonicalMemberOrder {
        field: &'static str,
        id: String,
    },
    ArtifactMismatch,
    UnresolvedSource {
        source_id: String,
    },
    SourceMismatch {
        source_id: String,
    },
    UnsupportedServingManifestVersion {
        requested: String,
    },
    UnresolvableManifestWiring {
        wiring_id: String,
        wiring_version: u32,
    },
    ManifestTooLarge {
        bytes: usize,
        limit: usize,
    },
    UnresolvedWiringNode {
        node_id: String,
    },
    UnresolvedWiringEntry {
        node_id: String,
    },
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
            Self::GraphHashMismatch => write!(formatter, "flow graph hash does not match"),
            Self::DraftContentHashMismatch => {
                write!(
                    formatter,
                    "draft content hash does not match draft document"
                )
            }
            Self::ValidatedDraftIdentityMismatch => {
                write!(
                    formatter,
                    "validated draft identity does not match its exact pins"
                )
            }
            Self::ArtifactIdMismatch => {
                write!(
                    formatter,
                    "flow graph does not match the pinned artifact id"
                )
            }
            Self::ArtifactHashMismatch => write!(formatter, "flow artifact hash does not match"),
            Self::ExecutionBundleHashMismatch => {
                write!(
                    formatter,
                    "execution bundle hash differs from its exact bytes"
                )
            }
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
            Self::UnsupportedServingManifestVersion { requested } => {
                write!(
                    formatter,
                    "{}: requested {requested}; supported version is {}",
                    UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL, SERVING_MANIFEST_FORMAT_VERSION
                )
            }
            Self::UnresolvableManifestWiring {
                wiring_id,
                wiring_version,
            } => {
                write!(
                    formatter,
                    "wiring {wiring_id:?} version {wiring_version} is absent from the serving manifest"
                )
            }
            Self::ManifestTooLarge { bytes, limit } => {
                write!(
                    formatter,
                    "serving manifest is {bytes} bytes, over the {limit}-byte delivery limit"
                )
            }
            Self::UnresolvedWiringNode { node_id } => {
                write!(
                    formatter,
                    "wiring edge names node {node_id:?}, which the document does not declare"
                )
            }
            Self::UnresolvedWiringEntry { node_id } => {
                write!(
                    formatter,
                    "wiring entry names node {node_id:?}, which the document does not declare"
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

/// The digest of a serving manifest's RFC 8785 canonical bytes.
///
/// This is the one *derived* identity in the family: it is never asserted by a
/// carrier and never read from an object name, only computed from verified
/// content by [`ServingManifest::from_canonical_bytes`]. From there it travels
/// further than any other hash in the system — through the claim-time run
/// recording, effect-authority equality, and the deployment attestation, across
/// two host processes and four manifest readers. Every flow-level hash it could
/// be mistaken for (`plan-hash`, `source-artifact`, `binding-base-artifact`,
/// `definition-hash`) is a bare `String`, so the type is what keeps them apart.
///
/// Like [`DefinitionHash`] and [`ArtifactHash`] it derives `Serialize` but *not*
/// `Deserialize`: a derived `Deserialize` would bypass [`Self::parse`] at exactly
/// the boundary where validation matters, which is the reasoning
/// [`ServingRelease`] already records for not reusing `ReleaseId` on the wire.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ManifestDigest(String);

impl ManifestDigest {
    /// Parse a canonical SHA-256 serving-manifest digest.
    pub fn parse(value: impl Into<String>) -> Result<Self, CatalogIdentityError> {
        let value = value.into();
        validate_digest(&value, "manifest-digest")?;
        Ok(Self(value))
    }

    /// The `sha256:<hex>` representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ManifestDigest {
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
        let bytes = wamn_flow::canonical_json_bytes(&value).into_boxed_slice();
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
    canonical_bytes: Box<[u8]>,
}

impl Artifact {
    /// Build an artifact from its graph and resolved public interfaces.
    pub fn new(
        tenant_id: impl Into<String>,
        flow: &Flow,
        interfaces: Vec<NodeInterface>,
    ) -> Result<Self, CatalogIdentityError> {
        let id = ArtifactId::new(tenant_id, flow.flow_id.clone(), flow.version)?;
        validate_text(&flow.schema_version, "schema-version")?;
        validate_interfaces(flow, &interfaces)?;
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
        let id_bytes = canonical_serialized(&id);
        owned.push(("artifact-id", id_bytes));
        owned.push(("schema-version", flow.schema_version.as_bytes().to_vec()));
        owned.push(("graph", graph_bytes));
        let borrowed: Vec<_> = owned
            .iter()
            .map(|(tag, bytes)| (*tag, bytes.as_slice()))
            .collect();
        let canonical_bytes = frames("artifact", borrowed).into_boxed_slice();
        let artifact_hash = ArtifactHash(digest(&canonical_bytes));
        Ok(Self {
            identity: ArtifactIdentity { id, artifact_hash },
            schema_version: flow.schema_version.clone(),
            graph_hash,
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

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }
}

/// Version-independent content address of a mutable flow document after validation.
///
/// This is an internal document/cache identity, not the executable draft-artifact
/// identity. Execution and publication use the ordinary exact [`ArtifactHash`],
/// including the draft's proposed runtime/publish version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct DraftContentHash(String);

impl DraftContentHash {
    /// Compute the version-independent content address for a parsed flow draft.
    ///
    /// Shares [`FlowPreimage`] with [`Flow::canonical_bytes`], so the W2 digest
    /// ordering rules apply identically to both; only `version` differs, and it
    /// is omitted here by construction rather than deleted from a serialized map.
    pub fn for_flow(flow: &Flow) -> Self {
        Self(digest(&frames(
            "flow-draft",
            [(
                "graph",
                canonical_serialized(&FlowPreimage::version_independent(flow)).as_slice(),
            )],
        )))
    }

    /// Parse and validate a persisted draft content address.
    pub fn parse(value: impl Into<String>) -> Result<Self, CatalogIdentityError> {
        let value = value.into();
        validate_digest(&value, "draft-content-hash")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exact inputs bound by one validated draft execution identity.
#[derive(Debug, Clone, Copy)]
pub struct ValidatedDraftIdentityInput<'a> {
    pub tenant_id: &'a str,
    pub draft_id: &'a str,
    pub draft_revision: u64,
    pub flow_id: &'a str,
    pub runtime_flow_version: u32,
    pub draft_content_hash: &'a str,
    pub draft_artifact_hash: &'a str,
    pub execution_bundle_hash: &'a str,
    pub catalog_id: &'a str,
    pub catalog_version: u32,
    pub environment: &'a str,
    pub binding_base_artifact_hash: &'a str,
}

/// Content address binding a validated graph to its exact executable and environment view.
///
/// The bundle and artifact hashes are independently reverified at load, then this identity
/// prevents either valid value from being transplanted onto a different validated draft.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ValidatedDraftIdentity(String);

impl ValidatedDraftIdentity {
    pub fn new(input: ValidatedDraftIdentityInput<'_>) -> Result<Self, CatalogIdentityError> {
        for (value, field) in [
            (input.tenant_id, "tenant-id"),
            (input.draft_id, "draft-id"),
            (input.flow_id, "flow-id"),
            (input.catalog_id, "catalog-id"),
            (input.environment, "environment"),
        ] {
            validate_text(value, field)?;
        }
        for (value, field) in [
            (input.draft_content_hash, "draft-content-hash"),
            (input.draft_artifact_hash, "draft-artifact-hash"),
            (input.execution_bundle_hash, "execution-bundle-hash"),
            (
                input.binding_base_artifact_hash,
                "binding-base-artifact-hash",
            ),
        ] {
            validate_digest(value, field)?;
        }
        if input.runtime_flow_version == 0 {
            return Err(CatalogIdentityError::ZeroVersion {
                field: "runtime-flow-version",
            });
        }
        if input.draft_revision == 0 {
            return Err(CatalogIdentityError::ZeroVersion {
                field: "draft-revision",
            });
        }
        if input.catalog_version == 0 {
            return Err(CatalogIdentityError::ZeroVersion {
                field: "catalog-version",
            });
        }
        let runtime_flow_version = input.runtime_flow_version.to_be_bytes();
        let draft_revision = input.draft_revision.to_be_bytes();
        let catalog_version = input.catalog_version.to_be_bytes();
        Ok(Self(digest(&frames(
            "validated-flow-draft",
            [
                ("tenant-id", input.tenant_id.as_bytes()),
                ("draft-id", input.draft_id.as_bytes()),
                ("draft-revision", draft_revision.as_slice()),
                ("flow-id", input.flow_id.as_bytes()),
                ("runtime-flow-version", runtime_flow_version.as_slice()),
                ("draft-content-hash", input.draft_content_hash.as_bytes()),
                ("draft-artifact-hash", input.draft_artifact_hash.as_bytes()),
                (
                    "execution-bundle-hash",
                    input.execution_bundle_hash.as_bytes(),
                ),
                ("catalog-id", input.catalog_id.as_bytes()),
                ("catalog-version", catalog_version.as_slice()),
                ("environment", input.environment.as_bytes()),
                (
                    "binding-base-artifact-hash",
                    input.binding_base_artifact_hash.as_bytes(),
                ),
            ],
        ))))
    }

    pub fn from_storage(
        expected_hash: &str,
        input: ValidatedDraftIdentityInput<'_>,
    ) -> Result<Self, CatalogIdentityError> {
        validate_digest(expected_hash, "validated-draft-identity")?;
        let identity = Self::new(input)?;
        if identity.as_str() != expected_hash {
            return Err(CatalogIdentityError::ValidatedDraftIdentityMismatch);
        }
        Ok(identity)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ValidatedDraftIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl fmt::Display for DraftContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One validated flow draft pinned to its exact executable bundle.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftArtifact {
    content_hash: DraftContentHash,
    artifact: Artifact,
    execution_plan: ExecutionPlanV2,
}

impl DraftArtifact {
    /// Validate a draft against resolved public interfaces and pin its bundle.
    pub fn new(
        tenant_id: impl Into<String>,
        flow: &Flow,
        interfaces: Vec<NodeInterface>,
        execution_plan: ExecutionPlanV2,
    ) -> Result<Self, CatalogIdentityError> {
        execution_plan.validate()?;
        let content_hash = DraftContentHash::for_flow(flow);
        let artifact = Artifact::new(tenant_id, flow, interfaces)?;
        if execution_plan.header.root_artifact_hash != artifact.identity().artifact_hash().as_str()
        {
            return Err(CatalogIdentityError::ArtifactMismatch);
        }
        Ok(Self {
            content_hash,
            artifact,
            execution_plan,
        })
    }

    pub fn content_hash(&self) -> &DraftContentHash {
        &self.content_hash
    }

    pub fn artifact(&self) -> &Artifact {
        &self.artifact
    }

    pub fn execution_plan(&self) -> &ExecutionPlanV2 {
        &self.execution_plan
    }
}

fn validate_interfaces(
    flow: &Flow,
    interfaces: &[NodeInterface],
) -> Result<(), CatalogIdentityError> {
    let mut by_type = BTreeMap::new();
    for interface in interfaces {
        validate_interface(interface)?;
        let node_type = interface.node_type.clone();
        if by_type.insert(node_type.clone(), interface).is_some() {
            return Err(CatalogIdentityError::DuplicateInterface { node_type });
        }
    }
    for node in &flow.nodes {
        if MODEL_OWNED_NODES.contains(&node.node_type.as_str()) {
            continue;
        }
        if !by_type.contains_key(&node.node_type) {
            return Err(CatalogIdentityError::UnresolvedInterface {
                node_type: node.node_type.clone(),
            });
        }
    }
    Ok(())
}

fn validate_interface(interface: &NodeInterface) -> Result<(), CatalogIdentityError> {
    validate_text(&interface.node_type, "node-type")?;
    if interface
        .output_ports
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(CatalogIdentityError::NonCanonicalInterfaceOrder {
            node_type: interface.node_type.clone(),
        });
    }
    Ok(())
}

/// An authored graph reverified against one immutable artifact row.
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedArtifact {
    flow: Flow,
}

impl PinnedArtifact {
    /// Verify the authored graph and public contract without compatibility readers.
    pub fn from_storage(
        expected_tenant_id: &str,
        expected_flow_id: &str,
        runtime_flow_version: u32,
        graph_json: &str,
        graph_hash: &str,
        artifact_hash: &str,
    ) -> Result<Self, CatalogIdentityError> {
        let flow = Flow::from_json(graph_json).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("flow graph JSON is invalid: {error}"),
            }
        })?;
        if flow.flow_id != expected_flow_id || flow.version != runtime_flow_version {
            return Err(CatalogIdentityError::ArtifactIdMismatch);
        }
        let graph_bytes = flow.canonical_bytes();
        if digest(&graph_bytes) != graph_hash {
            return Err(CatalogIdentityError::GraphHashMismatch);
        }
        let id = ArtifactId::new(expected_tenant_id, flow.flow_id.clone(), flow.version)?;
        let id_bytes = canonical_serialized(&id);
        let expected_artifact_hash = digest(&frames(
            "artifact",
            [
                ("artifact-id", id_bytes.as_slice()),
                ("schema-version", flow.schema_version.as_bytes()),
                ("graph", graph_bytes.as_slice()),
            ],
        ));
        if expected_artifact_hash != artifact_hash {
            return Err(CatalogIdentityError::ArtifactHashMismatch);
        }
        Ok(Self { flow })
    }

    pub fn flow(&self) -> &Flow {
        &self.flow
    }
}

/// Environment and binding-base pins needed to verify a persisted validated draft row.
#[derive(Debug, Clone, Copy)]
pub struct StoredValidatedDraftContext<'a> {
    pub expected_identity_hash: &'a str,
    pub draft_id: &'a str,
    pub draft_revision: u64,
    pub catalog_id: &'a str,
    pub catalog_version: u32,
    pub environment: &'a str,
    pub binding_base_artifact_hash: &'a str,
}

/// A persisted validated draft reverified at the execution boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedDraftArtifact {
    content_hash: DraftContentHash,
    artifact: PinnedArtifact,
    execution_plan: ExecutionPlanV2,
    validated_identity: ValidatedDraftIdentity,
}

impl PinnedDraftArtifact {
    /// Verify the graph, plan bytes, and complete draft identity together.
    #[expect(
        clippy::too_many_arguments,
        reason = "the immutable draft row's content and plan pins are verified together"
    )]
    pub fn from_storage(
        expected_tenant_id: &str,
        expected_flow_id: &str,
        runtime_flow_version: u32,
        draft_content_hash: &str,
        graph_json: &str,
        graph_hash: &str,
        draft_artifact_hash: &str,
        execution_plan_bytes: &[u8],
        execution_bundle_hash: &str,
        context: StoredValidatedDraftContext<'_>,
    ) -> Result<Self, CatalogIdentityError> {
        let artifact = PinnedArtifact::from_storage(
            expected_tenant_id,
            expected_flow_id,
            runtime_flow_version,
            graph_json,
            graph_hash,
            draft_artifact_hash,
        )?;
        let content_hash = DraftContentHash::parse(draft_content_hash)?;
        if content_hash != DraftContentHash::for_flow(artifact.flow()) {
            return Err(CatalogIdentityError::DraftContentHashMismatch);
        }
        let execution_plan = read_execution_plan(execution_bundle_hash, execution_plan_bytes)?;
        if execution_plan.header.root_artifact_hash != draft_artifact_hash {
            return Err(CatalogIdentityError::ArtifactMismatch);
        }
        let validated_identity = ValidatedDraftIdentity::from_storage(
            context.expected_identity_hash,
            ValidatedDraftIdentityInput {
                tenant_id: expected_tenant_id,
                draft_id: context.draft_id,
                draft_revision: context.draft_revision,
                flow_id: expected_flow_id,
                runtime_flow_version,
                draft_content_hash: content_hash.as_str(),
                draft_artifact_hash,
                execution_bundle_hash,
                catalog_id: context.catalog_id,
                catalog_version: context.catalog_version,
                environment: context.environment,
                binding_base_artifact_hash: context.binding_base_artifact_hash,
            },
        )?;
        Ok(Self {
            content_hash,
            artifact,
            execution_plan,
            validated_identity,
        })
    }

    pub fn artifact(&self) -> &PinnedArtifact {
        &self.artifact
    }

    pub fn execution_plan(&self) -> &ExecutionPlanV2 {
        &self.execution_plan
    }

    pub fn validated_identity(&self) -> &ValidatedDraftIdentity {
        &self.validated_identity
    }
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
///
/// `Deserialize` is derived so the frozen [`ServingAttachment`] wire shape can
/// name this kind instead of re-declaring one; the enum is fieldless, so the
/// derive adds no unvalidated construction path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

        let id_bytes = canonical_serialized(&id);
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
    frames_with_format(IDENTITY_FORMAT, domain, values)
}

fn frames_with_format<'a>(
    format: &[u8],
    domain: &str,
    values: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Vec<u8> {
    let mut output = Vec::new();
    write_frame(&mut output, format);
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

/// Canonical identity-frame bytes for one serializable identity input.
///
/// Routes through `wamn_flow::canonical_json_bytes`, the workspace's only RFC
/// 8785 producer. Until wamn-0h0g.15.63 this crate carried a second, `ryu-js`
/// based implementation of the same spec beside it, which left release identity
/// depending on *which* producer a call site happened to reach for.
fn canonical_serialized(value: &impl Serialize) -> Vec<u8> {
    wamn_flow::canonical_json_bytes(
        &serde_json::to_value(value).expect("identity input serializes"),
    )
}

#[cfg(test)]
mod tests {
    use super::{ManifestDigest, digest, frames};

    /// The invariant that retired `valid_manifest_digest` in the claim path: the
    /// newtype admits exactly the `sha256:<64 lowercase hex>` shape the run
    /// plane's `runs_release_record_check` admits, so a parsed value can never
    /// die on that CHECK inside a lease grant.
    #[test]
    fn a_manifest_digest_admits_exactly_the_run_plane_shape() {
        assert!(ManifestDigest::parse(format!("sha256:{}", "0".repeat(64))).is_ok());
        assert!(ManifestDigest::parse(format!("sha256:{}b", "af9".repeat(21))).is_ok());
        for rejected in [
            String::new(),
            "sha256:".to_string(),
            "deadbeef".to_string(),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(65)),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:{}", "g".repeat(64)),
            format!("SHA256:{}", "a".repeat(64)),
        ] {
            assert!(
                ManifestDigest::parse(rejected.clone()).is_err(),
                "accepted {rejected:?}"
            );
        }
    }

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

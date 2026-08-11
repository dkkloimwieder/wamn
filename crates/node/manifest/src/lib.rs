//! # wamn-node-manifest — the `wamn.node.manifest` OCI annotation (5.4)
//!
//! Design-note 8 (`docs/archive/execution/wamn-node-design-notes.md`): node metadata lives in an
//! OCI **annotation**, not a WIT export — a registry scan builds the node
//! palette with no instantiation. This crate is the annotation's canonical
//! model: types, structural validation, import/export, and the generated
//! language-neutral JSON Schema (`docs/archive/contracts/wamn-node-manifest.schema.json`, the
//! wamn-flow/wamn-schema-model pattern).
//!
//! Consumers: the builder (5.5) writes the annotation at push; the designer /
//! flow editor (3.3/5.8) scans it for the palette; the runner validates node
//! `config` against `config-schema` before dispatch (contract: nodes may
//! assume shape-valid config). Capability GRANTS are deliberately NOT here —
//! they are derived from the component's actual WIT imports (design-note 7),
//! never declared twice.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

mod http_operation_fingerprint;

#[doc(inline)]
pub use http_operation_fingerprint::{
    CanonicalHttpTarget, HTTP_OPERATION_FINGERPRINT_VERSION, HttpBodyDigest, HttpOperation,
    HttpOperationFingerprint, HttpOperationFingerprintError, HttpOperationFingerprintErrorKind,
    HttpSemanticHeader, PortableHttpTargetError, fingerprint_http_operation,
    is_http_operation_semantic_header, normalize_portable_http_target,
};

/// The OCI annotation key the manifest JSON is stored under.
pub const ANNOTATION_KEY: &str = "wamn.node.manifest";

/// The manifest schema version this crate reads/writes.
pub const SCHEMA_VERSION: &str = "0.1";

/// Shape version for the canonical resolved-node contract.
pub const RESOLVED_CONTRACT_VERSION: &str = "2";

/// Strict identity of the frozen zero-import node world.
pub const NODE_WORLD_INTERFACE: &str = "wamn:node/node@0.1.0";

/// Strict identity of the frozen P2 streamed-payload node world.
pub const STREAM_NODE_WORLD_INTERFACE: &str = "wamn:node/stream-node@0.1.0";

/// Exact P2 stream interface imported by [`STREAM_NODE_WORLD_INTERFACE`].
pub const PAYLOAD_STREAMS_INTERFACE: &str = "wasi:io/streams@0.2.12";

/// Frozen `wamn:node@0.1.0` worlds selectable by a resolved node contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeWorld {
    /// Zero-import transform world.
    Node,
    /// P2 payload-streaming world, including cooperative cancellation.
    StreamNode,
}

impl NodeWorld {
    /// Exact WIT world identity stored in resolved-node contracts.
    pub const fn interface_contract(self) -> &'static str {
        match self {
            Self::Node => NODE_WORLD_INTERFACE,
            Self::StreamNode => STREAM_NODE_WORLD_INTERFACE,
        }
    }

    /// Exact external WIT imports in the selected world's transitive closure.
    pub const fn external_imports(self) -> &'static [&'static str] {
        match self {
            Self::Node => &[],
            Self::StreamNode => &[PAYLOAD_STREAMS_INTERFACE],
        }
    }
}

/// Shape version for portable connection-type descriptors.
pub const CONNECTION_DESCRIPTOR_VERSION: &str = "1";

/// An ordering policy a node declares support for (design-note 2). The
/// runner's dispatch honors the flow's per-node choice among the node's
/// declared set; the node itself stays a pure function under all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum OrderingPolicy {
    /// Total order: one in-flight execution per node.
    Strict,
    /// Order per partition key, parallel across keys (the Kafka model).
    Partitioned,
    /// Free parallelism up to the concurrency limit.
    Unordered,
}

/// The only manifest-level purity assertion a custom node may make.
///
/// Absence is intentionally not represented by another wire value: an absent
/// declaration resolves to effectful semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Purity {
    /// The node has no externally observable effects and may be replayed.
    Pure,
}

/// Structural capability class used to specialize execution bundles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityClass {
    Pure,
    Http,
    Postgres,
}

/// A portable connection need. Environment instance data never enters this value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ConnectionRequirement {
    pub requirement_type: String,
    pub contract: String,
}

/// A field whose ownership is fixed by a connection-type descriptor.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionField {
    Method,
    RelativeTarget,
    Headers,
    Body,
    Authority,
    Tls,
    Redirect,
    Proxy,
    Credential,
    IdempotencyKey,
}

/// The principal allowed to supply one connection field.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionFieldOwner {
    Author,
    Environment,
    System,
}

/// Canonical ownership for one connection field.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ConnectionFieldOwnership {
    pub field: ConnectionField,
    pub owner: ConnectionFieldOwner,
}

/// The authority interpretation fixed by a connection type.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionAuthorityModel {
    HttpOrigin,
}

/// How environment-owned credentials enter a request.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialInjection {
    EnvironmentSelectedHttpHeader,
}

/// How the engine-owned stable key enters a request.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum IdempotencyKeyInjection {
    HttpIdempotencyKeyHeader,
}

/// Versioned portable semantics for one connection type.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ConnectionTypeDescriptor {
    pub descriptor_version: String,
    pub requirement_type: String,
    pub contract: String,
    pub authority_model: ConnectionAuthorityModel,
    pub field_ownership: Vec<ConnectionFieldOwnership>,
    pub credential_injection: CredentialInjection,
    pub idempotency_key_injection: IdempotencyKeyInjection,
}

impl ConnectionTypeDescriptor {
    /// The minimum HTTP descriptor settled by PLAN item 2B.
    pub fn http_v1() -> Self {
        Self {
            descriptor_version: CONNECTION_DESCRIPTOR_VERSION.to_string(),
            requirement_type: "http".to_string(),
            contract: "wamn:connection/http@0.1.0".to_string(),
            authority_model: ConnectionAuthorityModel::HttpOrigin,
            field_ownership: vec![
                ConnectionFieldOwnership {
                    field: ConnectionField::Method,
                    owner: ConnectionFieldOwner::Author,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::RelativeTarget,
                    owner: ConnectionFieldOwner::Author,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::Headers,
                    owner: ConnectionFieldOwner::Author,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::Body,
                    owner: ConnectionFieldOwner::Author,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::Authority,
                    owner: ConnectionFieldOwner::Environment,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::Tls,
                    owner: ConnectionFieldOwner::Environment,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::Redirect,
                    owner: ConnectionFieldOwner::Environment,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::Proxy,
                    owner: ConnectionFieldOwner::Environment,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::Credential,
                    owner: ConnectionFieldOwner::Environment,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::IdempotencyKey,
                    owner: ConnectionFieldOwner::System,
                },
            ],
            credential_injection: CredentialInjection::EnvironmentSelectedHttpHeader,
            idempotency_key_injection: IdempotencyKeyInjection::HttpIdempotencyKeyHeader,
        }
    }

    /// Stable bytes embedded in resolved-node, artifact, and bundle identities.
    pub fn identity_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("connection descriptor identity serializes")
    }
}

/// A publish-time node interface pin.
///
/// Output ports are sorted because they are a set for graph validation and
/// artifact identity. The engine-reserved `error` port is never present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ResolvedNodeInterface {
    /// Version of this resolved contract's serialized shape.
    pub contract_version: String,
    pub node_type: String,
    /// Exact component interface/WIT contract implemented by the executable.
    pub interface_contract: String,
    pub output_ports: Vec<String>,
    pub capability_classes: Vec<CapabilityClass>,
    pub connection_requirements: Vec<ConnectionRequirement>,
}

impl ResolvedNodeInterface {
    /// Build the minimum complete environment-independent interface contract.
    pub fn new(
        node_type: impl Into<String>,
        interface_contract: impl Into<String>,
        output_ports: Vec<String>,
        capability_classes: Vec<CapabilityClass>,
        connection_requirements: Vec<ConnectionRequirement>,
    ) -> Self {
        Self {
            contract_version: RESOLVED_CONTRACT_VERSION.to_string(),
            node_type: node_type.into(),
            interface_contract: interface_contract.into(),
            output_ports,
            capability_classes,
            connection_requirements,
        }
    }

    /// Whether the pinned interface permits a successful emission on `port`.
    pub fn permits_output_port(&self, port: &str) -> bool {
        self.output_ports
            .binary_search_by(|candidate| candidate.as_str().cmp(port))
            .is_ok()
    }
}

/// Exact executable bytes or platform revision selected for a resolved node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum ExecutableIdentity {
    Platform { revision: String },
    Component { digest: String },
}

/// Transitional custom-publish resolution retained until the custom plane is removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ResolvedNodeContract {
    pub interface: ResolvedNodeInterface,
    pub executable: ExecutableIdentity,
}

impl ResolvedNodeContract {
    /// Stable bytes used only by the transitional custom-publish plane.
    pub fn identity_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("resolved node contract identity serializes")
    }

    pub fn identity_hash(&self) -> String {
        sha256_identity(&self.identity_bytes())
    }
}

/// Transitional supplied-component resolution for the custom-publish plane.
///
/// Construction always requires both the resolved interface and the digest of
/// the supplied component bytes, so neither pin can be omitted accidentally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ResolvedComponent {
    pub contract: ResolvedNodeContract,
}

impl ResolvedComponent {
    /// Stable identity bytes for this resolved supplied component.
    pub fn identity_bytes(&self) -> Vec<u8> {
        self.contract.identity_bytes()
    }

    /// SHA-256 of [`Self::identity_bytes`].
    pub fn identity_hash(&self) -> String {
        sha256_identity(&self.identity_bytes())
    }
}

fn sha256_identity(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hash = String::with_capacity("sha256:".len() + digest.len() * 2);
    hash.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut hash, "{byte:02x}").expect("writing to a string is infallible");
    }
    hash
}

fn default_ordering() -> Vec<OrderingPolicy> {
    vec![
        OrderingPolicy::Strict,
        OrderingPolicy::Partitioned,
        OrderingPolicy::Unordered,
    ]
}

fn default_output_ports() -> Vec<String> {
    vec!["main".to_string()]
}

fn is_default_ordering(v: &Vec<OrderingPolicy>) -> bool {
    *v == default_ordering()
}

fn is_default_output_ports(v: &Vec<String>) -> bool {
    *v == default_output_ports()
}

/// The `wamn.node.manifest` document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct NodeManifest {
    /// Manifest schema version ("0.1"); 0.1.x admits additive changes only.
    pub schema_version: String,
    /// The flow-graph node `type` this component implements — a lowercase
    /// slug (`[a-z0-9-]`, alphanumeric first/last), the flow-id rule.
    pub node_type: String,
    /// Display name for the editor palette.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The node's own version (mirrors its OCI tag).
    pub version: String,
    /// The `wamn:node` contract version the component was built against
    /// (e.g. "0.1.0"). The runner instantiates against it and supports
    /// current + previous major (versioning policy, design notes).
    pub contract: String,
    /// JSON Schema for the node's `config`; the runner validates config
    /// against it BEFORE dispatch. Absent = any config accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<Value>,
    /// JSON Schema for the input payload (editor assistance / 11.5 checks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    /// JSON Schema for the output payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// Ordering policies the node supports. Default: all three.
    #[serde(
        default = "default_ordering",
        skip_serializing_if = "is_default_ordering"
    )]
    pub ordering: Vec<OrderingPolicy>,
    /// Output ports the node can emit (edge affordances in the editor).
    /// Default `["main"]`. `"error"` is reserved for the engine's error
    /// routing and never emitted by a node.
    #[serde(
        default = "default_output_ports",
        skip_serializing_if = "is_default_output_ports"
    )]
    pub output_ports: Vec<String>,
    /// Trusted semantic override. Absent custom-node manifests resolve to
    /// effectful; only the typed value `pure` authorizes recomputation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purity: Option<Purity>,
}

/// A structural validation finding (the wamn-flow `Issue` shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub severity: Severity,
    pub code: &'static str,
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

fn err(
    issues: &mut Vec<Issue>,
    code: &'static str,
    path: impl Into<String>,
    msg: impl Into<String>,
) {
    issues.push(Issue {
        severity: Severity::Error,
        code,
        path: path.into(),
        message: msg.into(),
    });
}

/// The 5.1 flow-id slug rule, extended to node types (they embed in
/// idempotency keys and registry lookups the same way).
fn is_slug(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_alphanumeric()
        && b[b.len() - 1].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// A JSON Schema document is an object or a boolean (draft-07 forms).
fn is_json_schema_form(v: &Value) -> bool {
    v.is_object() || v.is_boolean()
}

fn is_semverish(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

impl NodeManifest {
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("manifest serializes")
    }

    pub fn issues(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        match self.schema_version.split('.').collect::<Vec<_>>()[..] {
            ["0", "1"] | ["0", "1", _] => {}
            _ => err(
                &mut issues,
                "unsupported-schema-version",
                "schema-version",
                format!(
                    "unsupported manifest schema version {:?}",
                    self.schema_version
                ),
            ),
        }
        if !is_slug(&self.node_type) {
            err(
                &mut issues,
                "invalid-node-type",
                "node-type",
                format!(
                    "node type {:?} must be a lowercase slug ([a-z0-9-], alphanumeric first/last)",
                    self.node_type
                ),
            );
        }
        if self.name.trim().is_empty() {
            err(&mut issues, "empty-name", "name", "display name is empty");
        }
        if self.version.trim().is_empty() {
            err(&mut issues, "empty-version", "version", "version is empty");
        }
        if !is_semverish(&self.contract) {
            err(
                &mut issues,
                "invalid-contract-version",
                "contract",
                format!(
                    "contract version {:?} must be MAJOR.MINOR.PATCH (e.g. \"0.1.0\")",
                    self.contract
                ),
            );
        } else if self.contract != "0.1.0" {
            err(
                &mut issues,
                "unsupported-contract-version",
                "contract",
                format!(
                    "contract version {:?} is unsupported; the active node ABI is frozen at 0.1.0",
                    self.contract
                ),
            );
        }
        for (field, schema) in [
            ("config-schema", &self.config_schema),
            ("input-schema", &self.input_schema),
            ("output-schema", &self.output_schema),
        ] {
            if let Some(v) = schema
                && !is_json_schema_form(v)
            {
                err(
                    &mut issues,
                    "invalid-json-schema",
                    field,
                    format!("{field} must be a JSON Schema (object or boolean)"),
                );
            }
        }
        if self.ordering.is_empty() {
            err(
                &mut issues,
                "empty-ordering",
                "ordering",
                "a node must support at least one ordering policy",
            );
        }
        let mut seen = Vec::new();
        for o in &self.ordering {
            if seen.contains(o) {
                err(
                    &mut issues,
                    "duplicate-ordering",
                    "ordering",
                    format!("ordering policy {o:?} listed twice"),
                );
            }
            seen.push(*o);
        }
        if self.output_ports.is_empty() {
            err(
                &mut issues,
                "empty-output-ports",
                "output-ports",
                "a node must declare at least one output port",
            );
        }
        let mut seen_ports: Vec<&str> = Vec::new();
        for p in &self.output_ports {
            if p.is_empty() {
                err(
                    &mut issues,
                    "empty-output-port",
                    "output-ports",
                    "an output port name is empty",
                );
            }
            if p == "error" {
                err(
                    &mut issues,
                    "reserved-output-port",
                    "output-ports",
                    "\"error\" is reserved for the engine's error routing; \
                     errors travel as node-error, never as an emitted port",
                );
            }
            if seen_ports.contains(&p.as_str()) {
                err(
                    &mut issues,
                    "duplicate-output-port",
                    "output-ports",
                    format!("output port {p:?} listed twice"),
                );
            }
            seen_ports.push(p);
        }
        issues
    }

    pub fn validate(&self) -> Result<(), Vec<Issue>> {
        let issues = self.issues();
        if issues.iter().any(|i| i.severity == Severity::Error) {
            Err(issues)
        } else {
            Ok(())
        }
    }

    pub fn is_valid(&self) -> bool {
        self.validate().is_ok()
    }

    /// Resolve the manifest into the interface pinned by a flow artifact.
    pub fn resolved_interface(&self) -> Result<ResolvedNodeInterface, Vec<Issue>> {
        self.resolved_interface_for_world(NodeWorld::Node)
    }

    /// Resolve the manifest against one exact frozen WIT world.
    pub fn resolved_interface_for_world(
        &self,
        world: NodeWorld,
    ) -> Result<ResolvedNodeInterface, Vec<Issue>> {
        self.validate()?;
        let mut output_ports = self.output_ports.clone();
        output_ports.sort();
        Ok(ResolvedNodeInterface::new(
            self.node_type.clone(),
            world.interface_contract(),
            output_ports,
            if self.purity == Some(Purity::Pure) {
                vec![CapabilityClass::Pure]
            } else {
                Vec::new()
            },
            Vec::new(),
        ))
    }

    /// Refuse a resolved bundle that does not exactly match this manifest.
    pub fn validate_resolved_interface(
        &self,
        resolved: &ResolvedNodeInterface,
    ) -> Result<(), Vec<Issue>> {
        self.validate_resolved_interface_for_world(NodeWorld::Node, resolved)
    }

    /// Refuse a resolved bundle that does not match the selected frozen WIT world.
    pub fn validate_resolved_interface_for_world(
        &self,
        world: NodeWorld,
        resolved: &ResolvedNodeInterface,
    ) -> Result<(), Vec<Issue>> {
        let expected = self.resolved_interface_for_world(world)?;
        if expected == *resolved {
            Ok(())
        } else {
            Err(vec![Issue {
                severity: Severity::Error,
                code: "resolved-interface-mismatch",
                path: "resolved-interface".to_string(),
                message: format!(
                    "resolved interface {resolved:?} does not match manifest interface {expected:?}"
                ),
            }])
        }
    }

    /// Resolve a supplied component for the transitional custom-publish plane.
    pub fn resolved_component(
        &self,
        component_digest: impl Into<String>,
    ) -> Result<ResolvedComponent, Vec<Issue>> {
        self.resolved_component_for_world(NodeWorld::Node, component_digest)
    }

    /// Resolve component bytes against one exact frozen WIT world.
    pub fn resolved_component_for_world(
        &self,
        world: NodeWorld,
        component_digest: impl Into<String>,
    ) -> Result<ResolvedComponent, Vec<Issue>> {
        let interface = self.resolved_interface_for_world(world)?;
        let component_digest = component_digest.into();
        if !is_sha256_digest(&component_digest) {
            return Err(vec![Issue {
                severity: Severity::Error,
                code: "invalid-component-digest",
                path: "component-digest".to_string(),
                message: "component digest must be sha256:<64 lowercase hex digits>".to_string(),
            }]);
        }
        Ok(ResolvedComponent {
            contract: ResolvedNodeContract {
                interface,
                executable: ExecutableIdentity::Component {
                    digest: component_digest,
                },
            },
        })
    }
}

fn is_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

/// The language-neutral JSON Schema for the manifest, generated from these
/// types (single source of truth).
pub fn json_schema() -> Value {
    let schema = schemars::schema_for!(NodeManifest);
    serde_json::to_value(schema).expect("schema serializes")
}

/// [`json_schema`] as canonical pretty JSON with a trailing newline — the
/// exact bytes of `docs/archive/contracts/wamn-node-manifest.schema.json`.
pub fn json_schema_string() -> String {
    let mut s = serde_json::to_string_pretty(&json_schema()).expect("schema serializes");
    s.push('\n');
    s
}

#[derive(schemars::JsonSchema)]
#[serde(untagged)]
#[expect(
    dead_code,
    reason = "schema-only variants combine the two portable document roots"
)]
enum ConnectionContractDocument {
    Descriptor(ConnectionTypeDescriptor),
}

/// The language-neutral JSON Schema for portable connection descriptors and
/// requirements, generated from their canonical Rust types.
pub fn connection_contract_json_schema() -> Value {
    let schema = schemars::schema_for!(ConnectionContractDocument);
    serde_json::to_value(schema).expect("connection contract schema serializes")
}

/// [`connection_contract_json_schema`] as canonical pretty JSON with a
/// trailing newline.
pub fn connection_contract_json_schema_string() -> String {
    let mut schema = serde_json::to_string_pretty(&connection_contract_json_schema())
        .expect("connection contract schema serializes");
    schema.push('\n');
    schema
}

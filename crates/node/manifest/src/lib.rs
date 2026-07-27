//! # wamn-node-manifest — the `wamn.node.manifest` OCI annotation (5.4)
//!
//! Design-note 8 (`docs/wamn-node-design-notes.md`): node metadata lives in an
//! OCI **annotation**, not a WIT export — a registry scan builds the node
//! palette with no instantiation. This crate is the annotation's canonical
//! model: types, structural validation, import/export, and the generated
//! language-neutral JSON Schema (`docs/wamn-node-manifest.schema.json`, the
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

/// The OCI annotation key the manifest JSON is stored under.
pub const ANNOTATION_KEY: &str = "wamn.node.manifest";

/// The manifest schema version this crate reads/writes.
pub const SCHEMA_VERSION: &str = "0.1";

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
/// declaration resolves to effectful, never-replay semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Purity {
    /// The node has no externally observable effects and may be replayed.
    Pure,
}

/// The semantic purity pinned in a resolved interface bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResolvedPurity {
    Pure,
    Effectful,
}

/// The recovery class authorized by the resolved manifest semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryClass {
    Replay,
    NeverReplay,
}

/// A publish-time node interface pin.
///
/// Output ports are sorted because they are a set for graph validation and
/// artifact identity. The engine-reserved `error` port is never present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ResolvedNodeInterface {
    pub node_type: String,
    pub output_ports: Vec<String>,
    pub purity: ResolvedPurity,
    pub recovery_class: RecoveryClass,
}

impl ResolvedNodeInterface {
    /// Whether the pinned interface permits a successful emission on `port`.
    pub fn permits_output_port(&self, port: &str) -> bool {
        self.output_ports
            .binary_search_by(|candidate| candidate.as_str().cmp(port))
            .is_ok()
    }
}

/// The supplied-component input to immutable flow-artifact identity.
///
/// Construction always requires both the resolved interface and the digest of
/// the supplied component bytes, so neither pin can be omitted accidentally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ResolvedComponent {
    pub interface: ResolvedNodeInterface,
    pub component_digest: String,
}

impl ResolvedComponent {
    /// Stable identity bytes for this resolved supplied component.
    pub fn identity_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("resolved component identity serializes")
    }

    /// SHA-256 of [`Self::identity_bytes`].
    pub fn identity_hash(&self) -> String {
        let digest = Sha256::digest(self.identity_bytes());
        let mut hash = String::with_capacity("sha256:".len() + digest.len() * 2);
        hash.push_str("sha256:");
        for byte in digest {
            use std::fmt::Write as _;
            write!(&mut hash, "{byte:02x}").expect("writing to a string is infallible");
        }
        hash
    }
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
    /// effectful + never-replay; only the typed value `pure` authorizes replay.
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
        self.validate()?;
        let mut output_ports = self.output_ports.clone();
        output_ports.sort();
        let (purity, recovery_class) = match self.purity {
            Some(Purity::Pure) => (ResolvedPurity::Pure, RecoveryClass::Replay),
            None => (ResolvedPurity::Effectful, RecoveryClass::NeverReplay),
        };
        Ok(ResolvedNodeInterface {
            node_type: self.node_type.clone(),
            output_ports,
            purity,
            recovery_class,
        })
    }

    /// Refuse a resolved bundle that does not exactly match this manifest.
    pub fn validate_resolved_interface(
        &self,
        resolved: &ResolvedNodeInterface,
    ) -> Result<(), Vec<Issue>> {
        let expected = self.resolved_interface()?;
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

    /// Resolve a supplied component for inclusion in flow-artifact identity.
    pub fn resolved_component(
        &self,
        component_digest: impl Into<String>,
    ) -> Result<ResolvedComponent, Vec<Issue>> {
        let interface = self.resolved_interface()?;
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
            interface,
            component_digest,
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
/// exact bytes of `docs/wamn-node-manifest.schema.json`.
pub fn json_schema_string() -> String {
    let mut s = serde_json::to_string_pretty(&json_schema()).expect("schema serializes");
    s.push('\n');
    s
}

//! # wamn-node-manifest — the `wamn.node.manifest` OCI annotation (5.4)
//!
//! Design-note 8 (`docs/wamn-node-design-notes.md`): node metadata lives in an
//! OCI **annotation**, not a WIT export — a registry scan builds the node
//! palette with no instantiation. This crate is the annotation's canonical
//! model: types, structural validation, import/export, and the generated
//! language-neutral JSON Schema (`docs/wamn-node-manifest.schema.json`, the
//! wamn-flow/wamn-catalog pattern).
//!
//! Consumers: the builder (5.5) writes the annotation at push; the designer /
//! flow editor (3.3/5.8) scans it for the palette; the runner validates node
//! `config` against `config-schema` before dispatch (contract: nodes may
//! assume shape-valid config). Capability GRANTS are deliberately NOT here —
//! they are derived from the component's actual WIT imports (design-note 7),
//! never declared twice.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

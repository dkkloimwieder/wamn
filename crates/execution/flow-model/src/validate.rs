//! Structural validation of a [`Flow`] against pinned node interfaces.

use std::collections::{BTreeMap, HashMap, HashSet};

use boon::{Compiler, Draft, Schemas};
use serde::Deserialize;
use serde_json::Value;
use wamn_node_manifest::{ConnectionTypeDescriptor, normalize_portable_http_target};

use crate::types::{
    CallFlowConfig, ERROR_PORT, EntryKind, FailConfig, Flow, InvokeFlowConfig, MAIN_PORT, Node,
    Ordering, RequestConfig, RespondConfig, SCHEMA_VERSION,
};

/// Completion ports keyed by resolved node type.
///
/// The `error` port is engine-reserved and must not appear here. Entry,
/// `respond`, and `fail` interfaces are model-owned and are ignored if supplied.
pub type ResolvedInterfaces = BTreeMap<String, Vec<String>>;

/// Severity of a validation [`Issue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A single validation finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub severity: Severity,
    /// Stable machine code, e.g. `duplicate-node-id`.
    pub code: &'static str,
    /// JSON-ish path to the offending element, e.g. `nodes[2].credential`.
    pub path: String,
    pub message: String,
}

impl Issue {
    fn error(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(
            formatter,
            "{severity} [{}] {}: {}",
            self.code, self.path, self.message
        )
    }
}

/// Every structural issue for a flow, in stable traversal order.
pub fn validate(flow: &Flow, resolved_interfaces: &ResolvedInterfaces) -> Vec<Issue> {
    let mut issues = Vec::new();
    validate_identity(flow, &mut issues);

    let mut node_ids = HashSet::new();
    let mut nodes_by_id = HashMap::new();
    for (index, node) in flow.nodes.iter().enumerate() {
        if node.id.trim().is_empty() {
            issues.push(Issue::error(
                "empty-node-id",
                format!("nodes[{index}].id"),
                "node id is required",
            ));
        } else if !node_ids.insert(node.id.as_str()) {
            issues.push(Issue::error(
                "duplicate-node-id",
                format!("nodes[{index}].id"),
                format!("node id {:?} is not unique", node.id),
            ));
        } else {
            nodes_by_id.insert(node.id.as_str(), node);
        }
        if node.node_type.trim().is_empty() {
            issues.push(Issue::error(
                "empty-node-type",
                format!("nodes[{index}].type"),
                "node type is required",
            ));
        }
    }
    if flow.nodes.is_empty() {
        issues.push(Issue::error(
            "no-nodes",
            "nodes",
            "a flow needs at least one node",
        ));
    }

    validate_credentials(flow, &mut issues);
    validate_allowed_hosts(flow, &mut issues);
    validate_connections(flow, &mut issues);

    let entries: Vec<(usize, &Node, EntryKind)> = flow
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| node.entry_kind().map(|kind| (index, node, kind)))
        .collect();
    match entries.len() {
        0 => issues.push(Issue::error(
            "no-entry-node",
            "nodes",
            "a flow needs exactly one request, cron, or event entry node",
        )),
        1 => {}
        _ => issues.push(Issue::error(
            "multiple-entry-nodes",
            "nodes",
            format!(
                "found {} entry nodes; exactly one is required",
                entries.len()
            ),
        )),
    }

    validate_edges(flow, &node_ids, resolved_interfaces, &mut issues);
    validate_reserved_nodes(flow, &mut issues);
    validate_ordering(flow, &mut issues);

    if let Some((_, entry, entry_kind)) = entries.first().copied() {
        if flow.edges.iter().any(|edge| edge.to == entry.id) {
            issues.push(Issue::error(
                "entry-has-incoming-edge",
                format!("nodes[{}].id", entries[0].0),
                format!("entry node {:?} has an incoming edge", entry.id),
            ));
        }

        let reachable = reachable_from(flow, entry.id.as_str(), false);
        for (index, node) in flow.nodes.iter().enumerate() {
            if !reachable.contains(node.id.as_str()) {
                issues.push(Issue::error(
                    "unreachable-node",
                    format!("nodes[{index}].id"),
                    format!(
                        "node {:?} is not reachable from entry {:?}",
                        node.id, entry.id
                    ),
                ));
            }
        }

        match entry_kind {
            EntryKind::Request => {
                validate_request_graph(flow, entry, resolved_interfaces, &nodes_by_id, &mut issues)
            }
            EntryKind::Cron | EntryKind::Event => {
                for (index, node) in flow.nodes.iter().enumerate() {
                    if node.node_type == "respond" {
                        issues.push(Issue::error(
                            "respond-without-request-entry",
                            format!("nodes[{index}].type"),
                            "respond is only legal in a request-entry flow",
                        ));
                    }
                }
            }
        }
    }

    issues
}

fn validate_identity(flow: &Flow, issues: &mut Vec<Issue>) {
    match compatible(&flow.schema_version) {
        Compat::Ok => {}
        Compat::Unparsable => issues.push(Issue::error(
            "bad-schema-version",
            "schema-version",
            format!("{:?} is not a MAJOR.MINOR version", flow.schema_version),
        )),
        Compat::Unsupported => issues.push(Issue::error(
            "unsupported-schema-version",
            "schema-version",
            format!(
                "{:?} is newer than this implementation ({SCHEMA_VERSION})",
                flow.schema_version
            ),
        )),
    }
    if flow.flow_id.trim().is_empty() {
        issues.push(Issue::error(
            "empty-flow-id",
            "flow-id",
            "flow-id is required",
        ));
    } else if !is_slug(&flow.flow_id) {
        issues.push(Issue::error(
            "invalid-flow-id",
            "flow-id",
            format!(
                "flow-id {:?} must be a lowercase slug: [a-z0-9-], starting and ending alphanumeric",
                flow.flow_id
            ),
        ));
    }
    if flow.version == 0 {
        issues.push(Issue::error(
            "bad-version",
            "version",
            "version must be >= 1",
        ));
    }
}

fn validate_credentials(flow: &Flow, issues: &mut Vec<Issue>) {
    let mut names = HashSet::new();
    for (index, credential) in flow.credentials.iter().enumerate() {
        if !names.insert(credential.name.as_str()) {
            issues.push(Issue::error(
                "duplicate-credential",
                format!("credentials[{index}].name"),
                format!("credential name {:?} is not unique", credential.name),
            ));
        }
    }
    for (index, node) in flow.nodes.iter().enumerate() {
        if let Some(credential) = &node.credential
            && !names.contains(credential.as_str())
        {
            issues.push(Issue::error(
                "unknown-credential",
                format!("nodes[{index}].credential"),
                format!("references undeclared credential {credential:?}"),
            ));
        }
    }

    if flow
        .credentials
        .windows(2)
        .any(|pair| pair[0].name >= pair[1].name)
    {
        issues.push(Issue::error(
            "unsorted-credentials",
            "credentials",
            "credentials must be sorted by unique logical name",
        ));
    }
}

fn validate_allowed_hosts(flow: &Flow, issues: &mut Vec<Issue>) {
    let mut hosts = HashSet::new();
    for (index, host) in flow.allowed_hosts.iter().enumerate() {
        if host.is_empty() || host.chars().any(char::is_whitespace) {
            issues.push(Issue::error(
                "invalid-allowed-host",
                format!("allowed-hosts[{index}]"),
                format!("allowed host {host:?} is empty or contains whitespace"),
            ));
        } else if !hosts.insert(host.as_str()) {
            issues.push(Issue::error(
                "duplicate-allowed-host",
                format!("allowed-hosts[{index}]"),
                format!("allowed host {host:?} is not unique"),
            ));
        }
    }

    if flow.allowed_hosts.windows(2).any(|pair| pair[0] >= pair[1]) {
        issues.push(Issue::error(
            "unsorted-allowed-hosts",
            "allowed-hosts",
            "allowed hosts must be sorted and unique",
        ));
    }
}

fn validate_connections(flow: &Flow, issues: &mut Vec<Issue>) {
    let mut names = HashSet::new();
    for (index, named) in flow.connection_requirements.iter().enumerate() {
        if !is_slug(&named.name) {
            issues.push(Issue::error(
                "invalid-connection-requirement-name",
                format!("connection-requirements[{index}].name"),
                format!(
                    "connection requirement {:?} must be a lowercase slug: [a-z0-9-], starting and ending alphanumeric",
                    named.name
                ),
            ));
        } else if !names.insert(named.name.as_str()) {
            issues.push(Issue::error(
                "duplicate-connection-requirement",
                format!("connection-requirements[{index}].name"),
                format!("connection requirement {:?} is not unique", named.name),
            ));
        }
        if named.requirement != ConnectionTypeDescriptor::http_v1() {
            issues.push(Issue::error(
                "unsupported-connection-requirement",
                format!("connection-requirements[{index}].requirement"),
                "the initial flow contract accepts only the exact portable HTTP 0.1 requirement",
            ));
        }
    }

    if flow
        .connection_requirements
        .windows(2)
        .any(|pair| pair[0].name >= pair[1].name)
    {
        issues.push(Issue::error(
            "unsorted-connection-requirements",
            "connection-requirements",
            "connection requirements must be sorted by unique logical name",
        ));
    }

    let connection_backed_http = flow
        .nodes
        .iter()
        .any(|node| node.node_type == "http-request" && node.connection.is_some());
    if connection_backed_http && !flow.allowed_hosts.is_empty() {
        issues.push(Issue::error(
            "connection-http-has-allowed-hosts",
            "allowed-hosts",
            "connection-backed HTTP authority comes from the environment binding, not allowed-hosts",
        ));
    }

    for (index, node) in flow.nodes.iter().enumerate() {
        if let Some(connection) = node.connection.as_deref() {
            if !names.contains(connection) {
                issues.push(Issue::error(
                    "unknown-connection-requirement",
                    format!("nodes[{index}].connection"),
                    format!("references undeclared connection requirement {connection:?}"),
                ));
            }
            if matches!(
                node.node_type.as_str(),
                "request" | "cron" | "event" | "respond" | "fail" | "call-flow" | "invoke-flow"
            ) {
                issues.push(Issue::error(
                    "control-node-has-connection",
                    format!("nodes[{index}].connection"),
                    format!(
                        "control node type {:?} cannot consume a connection",
                        node.node_type
                    ),
                ));
            }
        }

        if node.node_type == "http-request" {
            validate_http_request_connection(flow, index, node, issues);
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct HttpRequestConfig {
    path_and_query: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: serde_json::Map<String, Value>,
    #[serde(default)]
    body: Option<String>,
}

fn validate_http_request_connection(
    flow: &Flow,
    index: usize,
    node: &Node,
    issues: &mut Vec<Issue>,
) {
    let connection = match node.connection.as_deref() {
        Some(connection) => Some(connection),
        None => {
            issues.push(Issue::error(
                "http-request-missing-connection",
                format!("nodes[{index}].connection"),
                "http-request requires one artifact-local connection",
            ));
            None
        }
    };
    if node.credential.is_some() {
        issues.push(Issue::error(
            "connection-http-has-credential",
            format!("nodes[{index}].credential"),
            "connection-backed HTTP credentials come from the environment binding",
        ));
    }

    if let Some(named) = connection.and_then(|connection| {
        flow.connection_requirements
            .iter()
            .find(|named| named.name == connection)
    }) && named.requirement != ConnectionTypeDescriptor::http_v1()
    {
        issues.push(Issue::error(
            "http-request-wrong-connection-type",
            format!("nodes[{index}].connection"),
            format!(
                "connection requirement {:?} is not exact HTTP 0.1",
                named.name
            ),
        ));
    }

    let config = match serde_json::from_value::<HttpRequestConfig>(node.config.clone()) {
        Ok(config) => config,
        Err(error) => {
            issues.push(Issue::error(
                "invalid-http-request-config",
                format!("nodes[{index}].config"),
                format!(
                    "connection-backed HTTP accepts only method, path-and-query, headers, and body: {error}"
                ),
            ));
            return;
        }
    };
    let target = config.path_and_query.as_str();
    if let Err(error) = normalize_portable_http_target(target) {
        issues.push(Issue::error(
            "http-request-target-not-relative",
            format!("nodes[{index}].config.path-and-query"),
            format!("HTTP path-and-query {target:?} is not portable: {error}"),
        ));
    }
    if config
        .method
        .as_deref()
        .is_some_and(|method| method.trim().is_empty())
    {
        issues.push(Issue::error(
            "invalid-http-request-method",
            format!("nodes[{index}].config.method"),
            "HTTP method cannot be empty",
        ));
    }
    for header in config.headers.keys() {
        match header.to_ascii_lowercase().as_str() {
            "authorization" | "proxy-authorization" | "host" => issues.push(Issue::error(
                "http-request-environment-header",
                format!("nodes[{index}].config.headers[{header:?}]"),
                format!("HTTP header {header:?} is owned by the environment connection"),
            )),
            "idempotency-key" => issues.push(Issue::error(
                "http-request-system-header",
                format!("nodes[{index}].config.headers[{header:?}]"),
                "HTTP Idempotency-Key is injected by the platform from the durable attempt",
            )),
            _ => {}
        }
    }
    let _ = config.body;
}

fn validate_edges(
    flow: &Flow,
    node_ids: &HashSet<&str>,
    resolved_interfaces: &ResolvedInterfaces,
    issues: &mut Vec<Issue>,
) {
    let mut wires = HashSet::new();
    for (index, edge) in flow.edges.iter().enumerate() {
        // The stable edge key is also the graph preimage's edge sort key
        // (`FlowPreimage`), so a repeat would make that ordering ambiguous.
        if !wires.insert((
            edge.from.as_str(),
            edge.from_port.as_str(),
            edge.to.as_str(),
            edge.to_port.as_deref(),
        )) {
            issues.push(Issue::error(
                "duplicate-edge",
                format!("edges[{index}]"),
                format!(
                    "edge {:?} -> {:?} on port {:?} is declared more than once",
                    edge.from, edge.to, edge.from_port
                ),
            ));
        }
        if !node_ids.contains(edge.from.as_str()) {
            issues.push(Issue::error(
                "unknown-edge-source",
                format!("edges[{index}].from"),
                format!("edge source {:?} is not a node id", edge.from),
            ));
        }
        if !node_ids.contains(edge.to.as_str()) {
            issues.push(Issue::error(
                "unknown-edge-target",
                format!("edges[{index}].to"),
                format!("edge target {:?} is not a node id", edge.to),
            ));
        }
        if edge.from == edge.to {
            issues.push(Issue::error(
                "self-loop",
                format!("edges[{index}]"),
                format!("node {:?} has an edge to itself", edge.from),
            ));
        }
        if edge.from_port != ERROR_PORT
            && let Some(source) = flow.nodes.iter().find(|node| node.id == edge.from)
            && let Some(ports) = owned_completion_ports(source, resolved_interfaces)
            && !ports.iter().any(|port| *port == edge.from_port)
        {
            issues.push(Issue::error(
                "unknown-output-port",
                format!("edges[{index}].from-port"),
                format!(
                    "node type {:?} has no completion port {:?}",
                    source.node_type, edge.from_port
                ),
            ));
        }
    }
    for node in &flow.nodes {
        let count = flow
            .edges
            .iter()
            .filter(|edge| edge.from == node.id && edge.from_port == ERROR_PORT)
            .count();
        if count > 1 {
            issues.push(Issue::error(
                "multiple-error-edges",
                format!("nodes[{:?}]", node.id),
                format!("node {:?} has {count} error edges", node.id),
            ));
        }
    }

    for (index, node) in flow.nodes.iter().enumerate() {
        let Some(ports) = owned_completion_ports(node, resolved_interfaces) else {
            issues.push(Issue::error(
                "unresolved-output-ports",
                format!("nodes[{index}].type"),
                format!(
                    "node type {:?} has no pinned output-port interface",
                    node.node_type
                ),
            ));
            continue;
        };
        let mut seen = HashSet::new();
        for port in ports {
            if port.is_empty() {
                issues.push(Issue::error(
                    "empty-output-port",
                    format!("interfaces[{:?}]", node.node_type),
                    "completion port names cannot be empty",
                ));
            } else if port == ERROR_PORT {
                issues.push(Issue::error(
                    "reserved-output-port",
                    format!("interfaces[{:?}]", node.node_type),
                    "error is engine-routed and cannot be a declared completion port",
                ));
            } else if !seen.insert(port) {
                issues.push(Issue::error(
                    "duplicate-output-port",
                    format!("interfaces[{:?}]", node.node_type),
                    format!("completion port {port:?} is declared more than once"),
                ));
            }
        }
    }
}

fn owned_completion_ports<'a>(
    node: &'a Node,
    resolved_interfaces: &'a ResolvedInterfaces,
) -> Option<Vec<&'a str>> {
    match node.node_type.as_str() {
        "request" | "cron" | "event" | "respond" | "call-flow" | "invoke-flow" => {
            Some(vec![MAIN_PORT])
        }
        "fail" => Some(Vec::new()),
        node_type => resolved_interfaces
            .get(node_type)
            .map(|ports| ports.iter().map(String::as_str).collect()),
    }
}

fn validate_reserved_nodes(flow: &Flow, issues: &mut Vec<Issue>) {
    for (index, node) in flow.nodes.iter().enumerate() {
        match node.node_type.as_str() {
            "request" => validate_request_config(index, &node.config, issues),
            "cron" | "event" => {
                if !is_empty_config(&node.config) {
                    issues.push(Issue::error(
                        "entry-has-source-config",
                        format!("nodes[{index}].config"),
                        format!(
                            "{} source configuration belongs to its attachment, not the flow artifact",
                            node.node_type
                        ),
                    ));
                }
            }
            "respond" => {
                match serde_json::from_value::<RespondConfig>(node.config.clone()) {
                    Ok(config) if !(200..=599).contains(&config.status) => {
                        issues.push(Issue::error(
                            "invalid-respond-status",
                            format!("nodes[{index}].config.status"),
                            "respond status must be in 200..=599",
                        ));
                    }
                    Ok(_) => {}
                    Err(error) => issues.push(Issue::error(
                        "invalid-respond-config",
                        format!("nodes[{index}].config"),
                        error.to_string(),
                    )),
                }
                let outgoing: Vec<_> = flow
                    .edges
                    .iter()
                    .filter(|edge| edge.from == node.id)
                    .collect();
                if outgoing.len() > 1
                    || outgoing
                        .first()
                        .is_some_and(|edge| edge.from_port != MAIN_PORT)
                {
                    issues.push(Issue::error(
                        "invalid-respond-successors",
                        format!("nodes[{index}]"),
                        "respond has zero or one outgoing main edge",
                    ));
                }
            }
            "call-flow" => {
                match serde_json::from_value::<CallFlowConfig>(node.config.clone()) {
                    Ok(config) => {
                        if config.flow_id.trim().is_empty() {
                            issues.push(Issue::error(
                                "empty-call-flow-id",
                                format!("nodes[{index}].config.flow-id"),
                                "call-flow flow-id is required",
                            ));
                        } else if !is_slug(&config.flow_id) {
                            issues.push(Issue::error(
                                "invalid-call-flow-id",
                                format!("nodes[{index}].config.flow-id"),
                                format!(
                                    "call-flow flow-id {:?} must be a lowercase slug",
                                    config.flow_id
                                ),
                            ));
                        }
                    }
                    Err(error) => issues.push(Issue::error(
                        "invalid-call-flow-config",
                        format!("nodes[{index}].config"),
                        error.to_string(),
                    )),
                }
                if node.credential.is_some() {
                    issues.push(Issue::error(
                        "call-flow-has-credential",
                        format!("nodes[{index}].credential"),
                        "call-flow is internal invocation, not credentialed egress",
                    ));
                }
            }
            "invoke-flow" => {
                match serde_json::from_value::<InvokeFlowConfig>(node.config.clone()) {
                    Ok(config) => {
                        if config.flow_id.trim().is_empty() {
                            issues.push(Issue::error(
                                "empty-invoke-flow-id",
                                format!("nodes[{index}].config.flow-id"),
                                "invoke-flow flow-id is required",
                            ));
                        }
                        if config.attachment_id.trim().is_empty() {
                            issues.push(Issue::error(
                                "empty-invoke-attachment-id",
                                format!("nodes[{index}].config.attachment-id"),
                                "invoke-flow attachment-id is required",
                            ));
                        }
                    }
                    Err(error) => issues.push(Issue::error(
                        "invalid-invoke-flow-config",
                        format!("nodes[{index}].config"),
                        error.to_string(),
                    )),
                }
                if node.credential.is_some() {
                    issues.push(Issue::error(
                        "invoke-flow-has-credential",
                        format!("nodes[{index}].credential"),
                        "invoke-flow is internal invocation, not credentialed egress",
                    ));
                }
            }
            "fail" => {
                match serde_json::from_value::<FailConfig>(node.config.clone()) {
                    Ok(config) => {
                        if config.code.trim().is_empty() {
                            issues.push(Issue::error(
                                "empty-fail-code",
                                format!("nodes[{index}].config.code"),
                                "fail code is required",
                            ));
                        }
                        if !(400..=599).contains(&config.status) {
                            issues.push(Issue::error(
                                "invalid-fail-status",
                                format!("nodes[{index}].config.status"),
                                "fail status must be in 400..=599",
                            ));
                        }
                    }
                    Err(error) => issues.push(Issue::error(
                        "invalid-fail-config",
                        format!("nodes[{index}].config"),
                        error.to_string(),
                    )),
                }
                if flow.edges.iter().any(|edge| edge.from == node.id) {
                    issues.push(Issue::error(
                        "fail-has-outgoing-edge",
                        format!("nodes[{index}]"),
                        "fail ends the run and cannot have an outgoing edge",
                    ));
                }
            }
            _ => {}
        }
    }
}

fn validate_request_config(index: usize, config: &Value, issues: &mut Vec<Issue>) {
    let request = match serde_json::from_value::<RequestConfig>(config.clone()) {
        Ok(request) => request,
        Err(error) => {
            issues.push(Issue::error(
                "invalid-request-config",
                format!("nodes[{index}].config"),
                error.to_string(),
            ));
            return;
        }
    };
    if let Some(schema) = request.input_schema.get("$schema")
        && schema.as_str() != Some("https://json-schema.org/draft/2020-12/schema")
    {
        issues.push(Issue::error(
            "wrong-input-schema-draft",
            format!("nodes[{index}].config.input-schema.$schema"),
            "request input-schema must use JSON Schema draft 2020-12",
        ));
        return;
    }
    if has_remote_ref(&request.input_schema) {
        issues.push(Issue::error(
            "remote-input-schema-ref",
            format!("nodes[{index}].config.input-schema"),
            "request input-schema may only use document-local $ref values",
        ));
        return;
    }

    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    let mut schemas = Schemas::new();
    if let Err(error) =
        compiler.add_resource("mem://request-input-schema.json", request.input_schema)
    {
        issues.push(Issue::error(
            "invalid-input-schema",
            format!("nodes[{index}].config.input-schema"),
            error.to_string(),
        ));
        return;
    }
    if let Err(error) = compiler.compile("mem://request-input-schema.json", &mut schemas) {
        issues.push(Issue::error(
            "invalid-input-schema",
            format!("nodes[{index}].config.input-schema"),
            error.to_string(),
        ));
    }
}

fn has_remote_ref(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(has_remote_ref),
        Value::Object(values) => values.iter().any(|(key, value)| {
            (key == "$ref"
                && value
                    .as_str()
                    .is_none_or(|reference| !reference.starts_with('#')))
                || has_remote_ref(value)
        }),
        _ => false,
    }
}

fn is_empty_config(config: &Value) -> bool {
    config.is_null() || config.as_object().is_some_and(serde_json::Map::is_empty)
}

fn validate_ordering(flow: &Flow, issues: &mut Vec<Issue>) {
    if let Ordering::Partitioned { partition_key } = &flow.ordering {
        if partition_key.trim().is_empty() {
            issues.push(Issue::error(
                "empty-partition-key",
                "ordering.partition-key",
                "partitioned ordering needs a partition-key expression",
            ));
        } else if let Err(error) = jmespath::compile(partition_key) {
            issues.push(Issue::error(
                "invalid-partition-key",
                "ordering.partition-key",
                format!("partition-key {partition_key:?} is not valid JMESPath: {error}"),
            ));
        }
    }
}

fn validate_request_graph(
    flow: &Flow,
    entry: &Node,
    resolved_interfaces: &ResolvedInterfaces,
    nodes_by_id: &HashMap<&str, &Node>,
    issues: &mut Vec<Issue>,
) {
    let region = reachable_from(flow, entry.id.as_str(), true);
    let stoppers: HashSet<&str> = region
        .iter()
        .copied()
        .filter(|node_id| {
            nodes_by_id
                .get(node_id)
                .is_some_and(|node| is_stopper(node))
        })
        .collect();
    let responders: HashSet<&str> = stoppers
        .iter()
        .copied()
        .filter(|node_id| nodes_by_id[node_id].node_type == "respond")
        .collect();

    for (index, node) in flow.nodes.iter().enumerate() {
        if !region.contains(node.id.as_str()) || is_stopper(node) {
            continue;
        }
        let Some(ports) = owned_completion_ports(node, resolved_interfaces) else {
            continue;
        };
        for port in ports {
            let outgoing = flow
                .edges
                .iter()
                .filter(|edge| edge.from == node.id && edge.from_port == port)
                .count();
            if outgoing == 0 {
                issues.push(Issue::error(
                    "unanswered-port",
                    format!("nodes[{index}].ports[{port:?}]"),
                    format!(
                        "completion port {port:?} on node {:?} has no outgoing edge before release",
                        node.id
                    ),
                ));
            } else if outgoing > 1 {
                issues.push(Issue::error(
                    "same-port-fanout-before-release",
                    format!("nodes[{index}].ports[{port:?}]"),
                    format!(
                        "completion port {port:?} on node {:?} has {outgoing} outgoing edges before release",
                        node.id
                    ),
                ));
            }
        }
    }

    let can_answer = reverse_reachable_within(flow, &stoppers, &region);
    for (index, node) in flow.nodes.iter().enumerate() {
        if region.contains(node.id.as_str()) && !can_answer.contains(node.id.as_str()) {
            issues.push(Issue::error(
                "unanswerable-path",
                format!("nodes[{index}].id"),
                format!("node {:?} cannot reach respond or fail", node.id),
            ));
        }
    }

    let caller_gone = reachable_from_successors(flow, &responders);
    for (index, node) in flow.nodes.iter().enumerate() {
        if node.node_type == "respond" && caller_gone.contains(node.id.as_str()) {
            issues.push(Issue::error(
                "double-release",
                format!("nodes[{index}].type"),
                format!(
                    "respond node {:?} is reachable after caller release",
                    node.id
                ),
            ));
        } else if region.contains(node.id.as_str()) && caller_gone.contains(node.id.as_str()) {
            issues.push(Issue::error(
                "region-re-entry",
                format!("nodes[{index}].id"),
                format!(
                    "node {:?} is reachable both before and after release",
                    node.id
                ),
            ));
        }
    }
    if responders.is_empty() {
        issues.push(Issue::error(
            "no-response-node",
            "nodes",
            "a request graph needs a reachable respond node",
        ));
    }
}

fn is_stopper(node: &Node) -> bool {
    matches!(node.node_type.as_str(), "respond" | "fail")
}

fn reachable_from<'a>(flow: &'a Flow, start: &'a str, stop_at_release: bool) -> HashSet<&'a str> {
    let nodes: HashMap<&str, &Node> = flow
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &flow.edges {
        if nodes.contains_key(edge.from.as_str()) && nodes.contains_key(edge.to.as_str()) {
            adjacency
                .entry(edge.from.as_str())
                .or_default()
                .push(edge.to.as_str());
        }
    }
    let mut seen = HashSet::new();
    let mut stack = vec![start];
    while let Some(node_id) = stack.pop() {
        if !seen.insert(node_id) {
            continue;
        }
        if stop_at_release && nodes.get(node_id).is_some_and(|node| is_stopper(node)) {
            continue;
        }
        if let Some(next) = adjacency.get(node_id) {
            stack.extend(next.iter().copied());
        }
    }
    seen
}

fn reverse_reachable_within<'a>(
    flow: &'a Flow,
    starts: &HashSet<&'a str>,
    allowed: &HashSet<&'a str>,
) -> HashSet<&'a str> {
    let mut reverse: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &flow.edges {
        if allowed.contains(edge.from.as_str()) && allowed.contains(edge.to.as_str()) {
            reverse
                .entry(edge.to.as_str())
                .or_default()
                .push(edge.from.as_str());
        }
    }
    let mut seen = HashSet::new();
    let mut stack: Vec<_> = starts.iter().copied().collect();
    while let Some(node_id) = stack.pop() {
        if seen.insert(node_id)
            && let Some(previous) = reverse.get(node_id)
        {
            stack.extend(previous.iter().copied());
        }
    }
    seen
}

fn reachable_from_successors<'a>(
    flow: &'a Flow,
    responders: &HashSet<&'a str>,
) -> HashSet<&'a str> {
    let mut seen = HashSet::new();
    let mut stack: Vec<&str> = flow
        .edges
        .iter()
        .filter(|edge| responders.contains(edge.from.as_str()))
        .map(|edge| edge.to.as_str())
        .collect();
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &flow.edges {
        adjacency
            .entry(edge.from.as_str())
            .or_default()
            .push(edge.to.as_str());
    }
    while let Some(node_id) = stack.pop() {
        if seen.insert(node_id)
            && let Some(next) = adjacency.get(node_id)
        {
            stack.extend(next.iter().copied());
        }
    }
    seen
}

enum Compat {
    Ok,
    Unparsable,
    Unsupported,
}

fn compatible(version: &str) -> Compat {
    let parse = |value: &str| -> Option<(u32, u32)> {
        let (major, minor) = value.split_once('.')?;
        Some((major.parse().ok()?, minor.parse().ok()?))
    };
    let (Some((major, minor)), Some((supported_major, supported_minor))) =
        (parse(version), parse(SCHEMA_VERSION))
    else {
        return Compat::Unparsable;
    };
    if major != supported_major || minor > supported_minor {
        Compat::Unsupported
    } else {
        Compat::Ok
    }
}

fn is_slug(id: &str) -> bool {
    let alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let bytes = id.as_bytes();
    bytes
        .iter()
        .all(|byte| alphanumeric(*byte) || *byte == b'-')
        && bytes.first().copied().is_some_and(alphanumeric)
        && bytes.last().copied().is_some_and(alphanumeric)
}

impl Flow {
    /// All validation issues against the resolved public interfaces.
    pub fn issues(&self, resolved_interfaces: &ResolvedInterfaces) -> Vec<Issue> {
        validate(self, resolved_interfaces)
    }

    /// `true` if the flow has no error-severity issues.
    pub fn is_valid(&self, resolved_interfaces: &ResolvedInterfaces) -> bool {
        !validate(self, resolved_interfaces)
            .iter()
            .any(|issue| issue.severity == Severity::Error)
    }

    /// `Ok` if valid, otherwise every error-severity issue.
    pub fn validate(&self, resolved_interfaces: &ResolvedInterfaces) -> Result<(), Vec<Issue>> {
        let errors: Vec<Issue> = validate(self, resolved_interfaces)
            .into_iter()
            .filter(|issue| issue.severity == Severity::Error)
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use wamn_node_manifest::ConnectionTypeDescriptor;

    use crate::types::{
        Capture, Edge, Flow, FlowConnectionRequirement, Node, Ordering, PartitionPolicy,
    };

    use super::ResolvedInterfaces;

    fn node(id: &str, node_type: &str) -> Node {
        Node {
            id: id.into(),
            node_type: node_type.into(),
            label: None,
            config: json!({}),
            connection: None,
            credential: None,
        }
    }

    fn edge(from: &str, from_port: &str, to: &str) -> Edge {
        Edge {
            from: from.into(),
            from_port: from_port.into(),
            to: to.into(),
            to_port: None,
            ordinal: None,
        }
    }

    fn interfaces() -> ResolvedInterfaces {
        BTreeMap::from([
            ("http-request".into(), vec!["main".into()]),
            ("split".into(), vec!["left".into(), "right".into()]),
            ("step".into(), vec!["main".into()]),
        ])
    }

    fn request_flow() -> Flow {
        let mut request = node("in", "request");
        request.config = json!({
            "input-schema": {
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object"
            }
        });
        let mut respond = node("out", "respond");
        respond.config = json!({"status": 200});
        Flow {
            schema_version: "0.1".into(),
            flow_id: "f".into(),
            version: 1,
            name: None,
            nodes: vec![request, node("work", "step"), respond],
            edges: vec![edge("in", "main", "work"), edge("work", "main", "out")],
            connection_requirements: vec![],
            credentials: vec![],
            allowed_hosts: vec![],
            partition_policy: PartitionPolicy::default(),
            ordering: Ordering::default(),
            capture: Capture::default(),
        }
    }

    fn codes(flow: &Flow) -> Vec<&'static str> {
        flow.issues(&interfaces())
            .into_iter()
            .map(|issue| issue.code)
            .collect()
    }

    fn http_requirement(name: &str) -> FlowConnectionRequirement {
        FlowConnectionRequirement {
            name: name.into(),
            requirement: ConnectionTypeDescriptor::http_v1(),
        }
    }

    fn connection_http_flow() -> Flow {
        let mut flow = request_flow();
        let node = &mut flow.nodes[1];
        node.node_type = "http-request".into();
        node.connection = Some("erp".into());
        node.config = json!({
            "method": "POST",
            "path-and-query": "/dispositions?source={{source}}",
            "headers": {"content-type": "application/json"},
            "body": "@"
        });
        flow.connection_requirements = vec![http_requirement("erp")];
        flow
    }

    #[test]
    fn portable_http_connection_is_valid_for_standard_and_custom_integrate_nodes() {
        let standard = connection_http_flow();
        assert_eq!(codes(&standard), Vec::<&str>::new());

        let mut custom = standard.clone();
        custom.nodes[1].node_type = "step".into();
        assert_eq!(codes(&custom), Vec::<&str>::new());
        assert_eq!(
            standard.connection_requirements, custom.connection_requirements,
            "standard and custom integrate nodes share one portable requirement shape"
        );
    }

    #[test]
    fn legacy_absolute_http_config_is_explicitly_refused() {
        let mut flow = connection_http_flow();
        flow.nodes[1].connection = None;
        flow.nodes[1].config = json!({"method": "POST", "url": "https://erp.example/x"});
        let codes = codes(&flow);
        assert!(codes.contains(&"http-request-missing-connection"));
        assert!(codes.contains(&"invalid-http-request-config"));
    }

    #[test]
    fn http_config_refuses_environment_owned_fields_and_headers() {
        for (field, value) in [
            ("url", json!("https://erp.example/x")),
            ("authority", json!("erp.example")),
            ("proxy", json!("http://proxy.example")),
            ("credential", json!("secret")),
            ("tls", json!({"insecure": true})),
            ("idempotency-key", json!(true)),
        ] {
            let mut flow = connection_http_flow();
            flow.nodes[1]
                .config
                .as_object_mut()
                .unwrap()
                .insert(field.into(), value);
            assert!(
                codes(&flow).contains(&"invalid-http-request-config"),
                "environment/system-owned field {field:?} was accepted"
            );
        }

        for header in ["Host", "Authorization", "Proxy-Authorization"] {
            let mut flow = connection_http_flow();
            flow.nodes[1].config["headers"] = json!({header: "injected"});
            assert!(
                codes(&flow).contains(&"http-request-environment-header"),
                "environment-owned header {header:?} was accepted"
            );
        }

        let mut system = connection_http_flow();
        system.nodes[1].config["headers"] = json!({"Idempotency-Key": "author-value"});
        assert!(codes(&system).contains(&"http-request-system-header"));
    }

    #[test]
    fn http_target_must_be_connection_relative() {
        for target in [
            "https://erp.example/x",
            "//erp.example/x",
            "custom:authority",
            "holds",
            "/safe#fragment",
            "",
        ] {
            let mut flow = connection_http_flow();
            flow.nodes[1].config["path-and-query"] = json!(target);
            assert!(
                codes(&flow).contains(&"http-request-target-not-relative"),
                "non-relative target {target:?} was accepted"
            );
        }
    }

    #[test]
    fn connection_references_are_declared_sorted_and_absent_from_control_nodes() {
        let mut unknown = connection_http_flow();
        unknown.nodes[1].connection = Some("missing".into());
        assert!(codes(&unknown).contains(&"unknown-connection-requirement"));

        let mut unsorted = connection_http_flow();
        unsorted
            .connection_requirements
            .insert(0, http_requirement("z-last"));
        assert!(codes(&unsorted).contains(&"unsorted-connection-requirements"));

        let mut control = connection_http_flow();
        control.nodes[0].connection = Some("erp".into());
        assert!(codes(&control).contains(&"control-node-has-connection"));
    }

    #[test]
    fn http_requirement_descriptor_version_and_contract_are_exact() {
        let mut bad_version = connection_http_flow();
        bad_version.connection_requirements[0]
            .requirement
            .descriptor_version = "2".into();
        assert!(codes(&bad_version).contains(&"unsupported-connection-requirement"));

        let mut bad_contract = connection_http_flow();
        bad_contract.connection_requirements[0].requirement.contract =
            "wamn:connection/http@0.2.0".into();
        assert!(codes(&bad_contract).contains(&"unsupported-connection-requirement"));
        assert!(codes(&bad_contract).contains(&"http-request-wrong-connection-type"));
    }

    #[test]
    fn artifact_identity_covers_connection_requirement_and_refuses_environment_fields() {
        let baseline = connection_http_flow();
        let mut changed = baseline.clone();
        changed.connection_requirements[0]
            .requirement
            .descriptor_version = "changed".into();
        assert_ne!(baseline.graph_hash(), changed.graph_hash());

        let baseline_value = serde_json::to_value(&baseline).unwrap();
        for field in [
            "environment",
            "instance",
            "generation",
            "endpoint",
            "credential-set-handle",
            "proxy",
        ] {
            let mut mutant = baseline_value.clone();
            mutant["connection-requirements"][0][field] = json!("dev-owned");
            assert!(
                serde_json::from_value::<Flow>(mutant).is_err(),
                "environment field {field:?} entered portable artifact bytes"
            );
        }
    }

    #[test]
    fn connection_backed_http_rejects_legacy_credential_and_allowed_hosts() {
        let mut flow = connection_http_flow();
        flow.nodes[1].credential = Some("erp-secret".into());
        flow.credentials.push(crate::CredentialRef {
            name: "erp-secret".into(),
            kind: Some("api-key".into()),
            description: None,
        });
        flow.allowed_hosts.push("erp.example".into());
        let codes = codes(&flow);
        assert!(codes.contains(&"connection-http-has-credential"));
        assert!(codes.contains(&"connection-http-has-allowed-hosts"));
    }

    #[test]
    fn t0_request_entry_respond_and_resolved_ports_are_valid() {
        let flow = request_flow();
        assert_eq!(flow.entry_node().map(|node| node.id.as_str()), Some("in"));
        assert!(
            flow.is_valid(&interfaces()),
            "issues: {:?}",
            flow.issues(&interfaces())
        );
    }

    #[test]
    fn t0_entry_cardinality_rejects_zero_and_multiple_entries() {
        let mut none = request_flow();
        none.nodes[0].node_type = "step".into();
        assert!(codes(&none).contains(&"no-entry-node"));

        let mut multiple = request_flow();
        multiple.nodes.push(node("tick", "cron"));
        multiple.edges.push(edge("in", "main", "tick"));
        assert!(codes(&multiple).contains(&"multiple-entry-nodes"));
        assert!(multiple.entry_node().is_none());
    }

    #[test]
    fn t0_request_all_fail_is_no_response_node() {
        let mut flow = request_flow();
        flow.nodes.pop();
        let mut fail = node("failed", "fail");
        fail.config = json!({"code": "nope"});
        flow.nodes.push(fail);
        flow.edges[1].to = "failed".into();
        let codes = codes(&flow);
        assert!(codes.contains(&"no-response-node"), "{codes:?}");
    }

    #[test]
    fn t0_request_predicates_have_named_counterexamples() {
        let mut unanswered = request_flow();
        unanswered.edges.pop();
        assert!(codes(&unanswered).contains(&"unanswered-port"));

        let mut fanout = request_flow();
        let mut fail = node("failed", "fail");
        fail.config = json!({"code": "nope"});
        fanout.nodes.push(fail);
        fanout.edges.push(edge("work", "main", "failed"));
        assert!(
            codes(&fanout).contains(&"same-port-fanout-before-release"),
            "{:?}",
            fanout.issues(&interfaces())
        );

        let mut trapped = request_flow();
        trapped.nodes.insert(2, node("loop-a", "step"));
        trapped.nodes.insert(3, node("loop-b", "step"));
        trapped.edges.push(edge("in", "error", "loop-a"));
        trapped.edges.push(edge("loop-a", "main", "loop-b"));
        trapped.edges.push(edge("loop-b", "main", "loop-a"));
        assert!(codes(&trapped).contains(&"unanswerable-path"));

        let mut double = request_flow();
        let mut second = node("second", "respond");
        second.config = json!({"status": 202});
        double.nodes.push(second);
        double.edges.push(edge("out", "main", "second"));
        assert!(codes(&double).contains(&"double-release"));

        let mut reentry = request_flow();
        reentry.nodes.insert(2, node("shared", "step"));
        reentry.edges[1] = edge("work", "main", "shared");
        reentry.edges.push(edge("shared", "main", "out"));
        reentry.edges.push(edge("out", "main", "shared"));
        assert!(codes(&reentry).contains(&"region-re-entry"));
    }

    #[test]
    fn t0_cron_event_reject_respond_but_allow_work_and_fail() {
        for entry_type in ["cron", "event"] {
            let mut flow = request_flow();
            flow.nodes[0].node_type = entry_type.into();
            flow.nodes[0].config = json!({});
            flow.nodes[1].node_type = "step".into();
            let mut fail = node("out", "fail");
            fail.config = json!({"code": "stopped", "status": 503});
            flow.nodes[2] = fail;
            assert!(
                flow.is_valid(&interfaces()),
                "{entry_type}: {:?}",
                flow.issues(&interfaces())
            );

            let mut with_response = flow;
            with_response.nodes[2].node_type = "respond".into();
            with_response.nodes[2].config = json!({"status": 200});
            assert!(codes(&with_response).contains(&"respond-without-request-entry"));
        }
    }

    #[test]
    fn t0_general_integrity_rejects_unreachable_fail_edges_and_bad_ports() {
        let mut unreachable = request_flow();
        unreachable.nodes.push(node("orphan", "step"));
        assert!(codes(&unreachable).contains(&"unreachable-node"));

        let mut fail_edge = request_flow();
        let mut fail = node("failed", "fail");
        fail.config = json!({"code": "nope"});
        fail_edge.nodes.push(fail);
        fail_edge.edges.push(edge("failed", "main", "out"));
        assert!(codes(&fail_edge).contains(&"fail-has-outgoing-edge"));

        let mut bad_port = request_flow();
        bad_port.edges[1].from_port = "missing".into();
        assert!(codes(&bad_port).contains(&"unknown-output-port"));

        let mut unresolved = request_flow();
        unresolved.nodes[1].node_type = "custom".into();
        assert!(codes(&unresolved).contains(&"unresolved-output-ports"));

        let mut incoming = request_flow();
        incoming.edges.push(edge("out", "main", "in"));
        assert!(codes(&incoming).contains(&"entry-has-incoming-edge"));

        let mut multiple_errors = request_flow();
        let mut failed = node("failed", "fail");
        failed.config = json!({"code": "nope"});
        multiple_errors.nodes.push(failed);
        multiple_errors.edges.push(edge("work", "error", "out"));
        multiple_errors.edges.push(edge("work", "error", "failed"));
        assert!(codes(&multiple_errors).contains(&"multiple-error-edges"));
    }

    #[test]
    fn request_input_schema_is_2020_12_local_and_compilable() {
        let mut flow = request_flow();
        flow.nodes[0].config = json!({
            "input-schema": {"$schema": "http://json-schema.org/draft-07/schema#"}
        });
        assert!(codes(&flow).contains(&"wrong-input-schema-draft"));

        flow.nodes[0].config = json!({
            "input-schema": {"$ref": "https://example.test/schema"}
        });
        assert!(codes(&flow).contains(&"remote-input-schema-ref"));

        flow.nodes[0].config = json!({
            "input-schema": {"type": 7}
        });
        assert!(codes(&flow).contains(&"invalid-input-schema"));
    }

    #[test]
    fn reserved_node_configs_enforce_contracts() {
        let mut flow = request_flow();
        flow.nodes[2].config = json!({"status": 199});
        assert!(codes(&flow).contains(&"invalid-respond-status"));

        let mut fail = node("failed", "fail");
        fail.config = json!({"code": "", "status": 399});
        flow.nodes.push(fail);
        flow.edges.push(edge("in", "error", "failed"));
        let fail_codes = codes(&flow);
        assert!(fail_codes.contains(&"empty-fail-code"));
        assert!(fail_codes.contains(&"invalid-fail-status"));

        let mut call_flow = node("callee", "call-flow");
        call_flow.config = json!({"flow-id": ""});
        call_flow.credential = Some("secret".into());
        flow.nodes.push(call_flow);
        let call_codes = codes(&flow);
        assert!(call_codes.contains(&"empty-call-flow-id"));
        assert!(call_codes.contains(&"call-flow-has-credential"));

        let mut invoke = node("child", "invoke-flow");
        invoke.config = json!({
            "flow-id": "",
            "attachment-id": "",
            "actor-mode": "service"
        });
        invoke.credential = Some("secret".into());
        flow.nodes.push(invoke);
        let invoke_codes = codes(&flow);
        assert!(invoke_codes.contains(&"empty-invoke-flow-id"));
        assert!(invoke_codes.contains(&"empty-invoke-attachment-id"));
        assert!(invoke_codes.contains(&"invoke-flow-has-credential"));
    }

    #[test]
    fn unknown_fields_are_rejected_at_flow_and_reserved_config_boundaries() {
        let mut request = request_flow();
        request.nodes[0].config["source"] = json!("webhook");
        assert!(codes(&request).contains(&"invalid-request-config"));

        let mut respond = request_flow();
        respond.nodes[2].config["body"] = json!("configured");
        assert!(codes(&respond).contains(&"invalid-respond-config"));

        let mut invoke = request_flow();
        let mut child = node("child", "invoke-flow");
        child.config = json!({
            "flow-id": "callee",
            "attachment-id": "callee-internal",
            "actor-mode": "root"
        });
        invoke.nodes.push(child);
        assert!(codes(&invoke).contains(&"invalid-invoke-flow-config"));

        let mut call_flow = request_flow();
        let mut child = node("child", "call-flow");
        child.config = json!({
            "flow-id": "callee",
            "attachment-id": "callee-internal"
        });
        call_flow.nodes.push(child);
        assert!(codes(&call_flow).contains(&"invalid-call-flow-config"));

        let unknown_flow_field = request_flow().to_json().replacen(
            "\"version\": 1,",
            "\"version\": 1,\n  \"entry\": \"in\",",
            1,
        );
        assert!(Flow::from_json(&unknown_flow_field).is_err());
    }

    #[test]
    fn partitioned_requires_a_compilable_jmespath_key() {
        let mut flow = request_flow();
        flow.ordering = Ordering::Partitioned {
            partition_key: "payload.[".into(),
        };
        assert!(codes(&flow).contains(&"invalid-partition-key"));
    }
}

//! Structural validation of a [`Flow`].
//!
//! This checks graph *well-formedness* — unique ids, referential integrity of
//! edges/entry/credentials, trigger fields, reachability. It deliberately does
//! NOT validate per-node-type `config`; that is the node library's job (5.3),
//! which contributes config schemas keyed by `node_type`.

use std::collections::{HashMap, HashSet};

use crate::types::{Flow, SCHEMA_VERSION, Trigger};

/// Severity of a validation [`Issue`]. Only [`Severity::Error`] makes a flow
/// invalid; warnings surface editor-fixable smells (e.g. dead nodes).
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

    fn warning(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{sev} [{}] {}: {}", self.code, self.path, self.message)
    }
}

/// Every issue (errors and warnings) for a flow, in a stable order.
pub fn validate(flow: &Flow) -> Vec<Issue> {
    let mut issues = Vec::new();

    // --- schema-format version ----------------------------------------------
    match compatible(&flow.schema_version) {
        Compat::Ok => {}
        Compat::Unparsable => issues.push(Issue::error(
            "bad-schema-version",
            "schema_version",
            format!("{:?} is not a MAJOR.MINOR version", flow.schema_version),
        )),
        Compat::Unsupported => issues.push(Issue::error(
            "unsupported-schema-version",
            "schema_version",
            format!(
                "{:?} is newer than this implementation ({SCHEMA_VERSION})",
                flow.schema_version
            ),
        )),
    }

    // --- identity -----------------------------------------------------------
    if flow.flow_id.trim().is_empty() {
        issues.push(Issue::error(
            "empty-flow-id",
            "flow_id",
            "flow_id is required",
        ));
    }
    if flow.version < 1 {
        issues.push(Issue::error(
            "bad-version",
            "version",
            "version must be >= 1",
        ));
    }

    // --- nodes: unique, non-empty ids ---------------------------------------
    let mut node_ids: HashSet<&str> = HashSet::new();
    for (i, node) in flow.nodes.iter().enumerate() {
        if node.id.trim().is_empty() {
            issues.push(Issue::error(
                "empty-node-id",
                format!("nodes[{i}].id"),
                "node id is required",
            ));
        } else if !node_ids.insert(node.id.as_str()) {
            issues.push(Issue::error(
                "duplicate-node-id",
                format!("nodes[{i}].id"),
                format!("node id {:?} is not unique", node.id),
            ));
        }
        if node.node_type.trim().is_empty() {
            issues.push(Issue::error(
                "empty-node-type",
                format!("nodes[{i}].type"),
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

    // --- credentials: unique names, resolvable refs -------------------------
    let mut cred_names: HashSet<&str> = HashSet::new();
    for (i, c) in flow.credentials.iter().enumerate() {
        if !cred_names.insert(c.name.as_str()) {
            issues.push(Issue::error(
                "duplicate-credential",
                format!("credentials[{i}].name"),
                format!("credential name {:?} is not unique", c.name),
            ));
        }
    }
    for (i, node) in flow.nodes.iter().enumerate() {
        if let Some(cred) = &node.credential
            && !cred_names.contains(cred.as_str())
        {
            issues.push(Issue::error(
                "unknown-credential",
                format!("nodes[{i}].credential"),
                format!("references undeclared credential {cred:?}"),
            ));
        }
    }

    // --- entry --------------------------------------------------------------
    if !node_ids.contains(flow.entry.as_str()) {
        issues.push(Issue::error(
            "unknown-entry",
            "entry",
            format!("entry {:?} is not a node id", flow.entry),
        ));
    }

    // --- edges: endpoints exist, no self-loop -------------------------------
    for (i, e) in flow.edges.iter().enumerate() {
        if !node_ids.contains(e.from.as_str()) {
            issues.push(Issue::error(
                "unknown-edge-source",
                format!("edges[{i}].from"),
                format!("edge source {:?} is not a node id", e.from),
            ));
        }
        if !node_ids.contains(e.to.as_str()) {
            issues.push(Issue::error(
                "unknown-edge-target",
                format!("edges[{i}].to"),
                format!("edge target {:?} is not a node id", e.to),
            ));
        }
        if e.from == e.to {
            issues.push(Issue::error(
                "self-loop",
                format!("edges[{i}]"),
                format!("node {:?} has an edge to itself", e.from),
            ));
        }
    }

    // --- trigger fields -----------------------------------------------------
    match &flow.trigger {
        Trigger::Cron { schedule } if schedule.trim().is_empty() => issues.push(Issue::error(
            "empty-cron-schedule",
            "trigger.schedule",
            "cron trigger needs a schedule",
        )),
        Trigger::RowEvent { table, .. } if table.trim().is_empty() => issues.push(Issue::error(
            "empty-row-event-table",
            "trigger.table",
            "row-event trigger needs a table",
        )),
        _ => {}
    }

    // --- reachability (warning) — dead nodes are editor smells --------------
    if node_ids.contains(flow.entry.as_str()) {
        let reachable = reachable_from(flow);
        for (i, node) in flow.nodes.iter().enumerate() {
            if !reachable.contains(node.id.as_str()) {
                issues.push(Issue::warning(
                    "unreachable-node",
                    format!("nodes[{i}].id"),
                    format!(
                        "node {:?} is not reachable from entry {:?}",
                        node.id, flow.entry
                    ),
                ));
            }
        }
    }

    issues
}

/// Node ids reachable from `flow.entry` by following edges.
fn reachable_from(flow: &Flow) -> HashSet<&str> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &flow.edges {
        adj.entry(e.from.as_str()).or_default().push(e.to.as_str());
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut stack = vec![flow.entry.as_str()];
    while let Some(n) = stack.pop() {
        if seen.insert(n)
            && let Some(next) = adj.get(n)
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

/// A flow's `schema_version` is compatible if its MAJOR matches and its MINOR is
/// not newer than what this crate implements (additive-within-major, per the
/// `0.1.x` freeze rule).
fn compatible(v: &str) -> Compat {
    let parse = |s: &str| -> Option<(u32, u32)> {
        let (maj, min) = s.split_once('.')?;
        Some((maj.parse().ok()?, min.parse().ok()?))
    };
    let (Some((maj, min)), Some((smaj, smin))) = (parse(v), parse(SCHEMA_VERSION)) else {
        return Compat::Unparsable;
    };
    if maj != smaj || min > smin {
        Compat::Unsupported
    } else {
        Compat::Ok
    }
}

impl Flow {
    /// All validation issues (errors and warnings).
    pub fn issues(&self) -> Vec<Issue> {
        validate(self)
    }

    /// `true` if the flow has no error-severity issues (warnings are allowed).
    pub fn is_valid(&self) -> bool {
        !validate(self).iter().any(|i| i.severity == Severity::Error)
    }

    /// `Ok` if valid, else the error-severity issues.
    pub fn validate(&self) -> Result<(), Vec<Issue>> {
        let errors: Vec<Issue> = validate(self)
            .into_iter()
            .filter(|i| i.severity == Severity::Error)
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
    use crate::types::{Edge, Flow, Node, Trigger};
    use serde_json::json;

    fn node(id: &str, ty: &str) -> Node {
        Node {
            id: id.into(),
            node_type: ty.into(),
            label: None,
            config: json!({}),
            credential: None,
        }
    }

    fn edge(from: &str, to: &str) -> Edge {
        Edge {
            from: from.into(),
            from_port: "main".into(),
            to: to.into(),
            to_port: None,
        }
    }

    /// A minimal valid flow: entry, one node, no edges.
    fn minimal() -> Flow {
        Flow {
            schema_version: "0.1".into(),
            flow_id: "f".into(),
            version: 1,
            name: None,
            trigger: Trigger::Manual,
            entry: "a".into(),
            nodes: vec![node("a", "respond")],
            edges: vec![],
            credentials: vec![],
        }
    }

    fn codes(flow: &Flow) -> Vec<&'static str> {
        flow.issues().into_iter().map(|i| i.code).collect()
    }

    #[test]
    fn minimal_flow_is_valid() {
        let f = minimal();
        assert!(f.is_valid(), "issues: {:?}", f.issues());
        assert!(f.validate().is_ok());
    }

    #[test]
    fn duplicate_node_id_is_error() {
        let mut f = minimal();
        f.nodes.push(node("a", "transform"));
        assert!(codes(&f).contains(&"duplicate-node-id"));
        assert!(!f.is_valid());
    }

    #[test]
    fn unknown_entry_is_error() {
        let mut f = minimal();
        f.entry = "nope".into();
        assert!(codes(&f).contains(&"unknown-entry"));
    }

    #[test]
    fn dangling_edge_and_self_loop_are_errors() {
        let mut f = minimal();
        f.nodes.push(node("b", "transform"));
        f.edges.push(edge("a", "ghost")); // unknown target
        f.edges.push(edge("b", "b")); // self-loop
        let c = codes(&f);
        assert!(c.contains(&"unknown-edge-target"));
        assert!(c.contains(&"self-loop"));
    }

    #[test]
    fn unknown_credential_ref_is_error() {
        let mut f = minimal();
        f.nodes[0].credential = Some("missing".into());
        assert!(codes(&f).contains(&"unknown-credential"));
    }

    #[test]
    fn empty_cron_schedule_is_error() {
        let mut f = minimal();
        f.trigger = Trigger::Cron {
            schedule: "  ".into(),
        };
        assert!(codes(&f).contains(&"empty-cron-schedule"));
    }

    #[test]
    fn unreachable_node_is_warning_not_error() {
        let mut f = minimal();
        f.nodes.push(node("orphan", "transform")); // no edge to it
        assert!(f.is_valid(), "orphan should warn, not error");
        assert!(codes(&f).contains(&"unreachable-node"));
    }

    #[test]
    fn future_major_schema_version_is_unsupported() {
        let mut f = minimal();
        f.schema_version = "1.0".into();
        assert!(codes(&f).contains(&"unsupported-schema-version"));
    }
}

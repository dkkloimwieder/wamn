//! Structural validation of a [`Flow`].
//!
//! This checks graph *well-formedness* — unique ids, referential integrity of
//! edges/entry/credentials, trigger fields, reachability. It deliberately does
//! NOT validate per-node-type `config`; that is the node library's job (5.3),
//! which contributes config schemas keyed by `node_type`.

use std::collections::{HashMap, HashSet};

use crate::types::{Flow, Ordering, SCHEMA_VERSION, Trigger};

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
    } else if !is_slug(&flow.flow_id) {
        issues.push(Issue::error(
            "invalid-flow-id",
            "flow_id",
            format!(
                "flow_id {:?} must be a lowercase slug: [a-z0-9-], starting and \
                 ending alphanumeric (trigger run ids embed the flow id with ':' \
                 separators — {}:cron:{{tick}} / {}:evt:{{stream-seq}} — so the charset \
                 keeps those ids unambiguous to parse and collation-stable to sort)",
                flow.flow_id, flow.flow_id, flow.flow_id
            ),
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

    // --- allowed-hosts: unique, structurally plausible (fqg.11) -------------
    // The authoritative grammar lives host-side (the runner's AllowedHost
    // parser); here we catch only what is wrong in ANY grammar. A host-side
    // parse failure drops the entry fail-closed, so a typo surfaces as
    // egress-denied at run time, never as wider access.
    let mut hosts: HashSet<&str> = HashSet::new();
    for (i, h) in flow.allowed_hosts.iter().enumerate() {
        if h.is_empty() || h.chars().any(char::is_whitespace) {
            issues.push(Issue::error(
                "invalid-allowed-host",
                format!("allowed-hosts[{i}]"),
                format!("allowed host {h:?} is empty or contains whitespace"),
            ));
        } else if !hosts.insert(h.as_str()) {
            issues.push(Issue::error(
                "duplicate-allowed-host",
                format!("allowed-hosts[{i}]"),
                format!("allowed host {h:?} is not unique"),
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

    // --- ordering: partitioned needs a compilable JMESPath key (5.11) -------
    // strict/unordered carry no key by construction (the type has no field), so
    // "must not carry a key" is enforced structurally; only the partitioned key
    // needs a grammar check. Full JMESPath authority is the eval path's
    // (`Ordering::partition_key_for`); here we reject a key that can never
    // compile, so a mis-authored expression fails validation, not silently at
    // fire() (where it would degrade to the flow-wide stream).
    if let Ordering::Partitioned { partition_key } = &flow.ordering {
        if partition_key.trim().is_empty() {
            issues.push(Issue::error(
                "empty-partition-key",
                "ordering.partition-key",
                "partitioned ordering needs a partition-key expression",
            ));
        } else if let Err(e) = jmespath::compile(partition_key) {
            issues.push(Issue::error(
                "invalid-partition-key",
                "ordering.partition-key",
                format!("partition-key {partition_key:?} is not a valid JMESPath: {e}"),
            ));
        }
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

/// A flow id is a lowercase slug: `[a-z0-9-]`, starting and ending
/// alphanumeric. Flow ids are embedded verbatim into deterministic trigger run
/// ids (`{flow}:cron:{tick}` / `{flow}:evt:{stream_seq}`, 5.14 + D19 §5), so the charset
/// guarantees `:` terminates the flow-id prefix (unambiguous exact-prefix
/// parse) and keeps id ordering collation-independent (every byte is ASCII).
fn is_slug(id: &str) -> bool {
    let alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let bytes = id.as_bytes();
    bytes.iter().all(|&b| alnum(b) || b == b'-')
        && bytes.first().copied().is_some_and(alnum)
        && bytes.last().copied().is_some_and(alnum)
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
    use crate::types::{Edge, Flow, Node, Ordering, PartitionPolicy, Trigger};
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
            allowed_hosts: vec![],
            partition_policy: PartitionPolicy::default(),
            ordering: Ordering::default(),
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
    fn flow_id_must_be_a_lowercase_slug() {
        // The poison shapes the 5.14 cron-lens review proved dangerous: a ':'
        // inside a flow id would make `{flow}:cron:{tick}` ambiguous to parse.
        for bad in [
            "my:cron:5",
            "up:down",
            "Poc-Receipt", // uppercase
            "poc_receipt", // underscore
            "poc receipt", // whitespace
            "poc.receipt", // dot
            "-leading",
            "trailing-",
            "-",
            "béta", // non-ASCII
        ] {
            let mut f = minimal();
            f.flow_id = bad.into();
            assert!(
                codes(&f).contains(&"invalid-flow-id"),
                "{bad:?} should be rejected"
            );
            assert!(!f.is_valid());
        }
        // Existing-style ids (and a single-char id) all pass.
        for good in ["f", "poc-receipt", "b-nightly", "lin4", "s3-demo", "0x9"] {
            let mut f = minimal();
            f.flow_id = good.into();
            assert!(
                f.is_valid(),
                "{good:?} should be accepted: {:?}",
                f.issues()
            );
        }
        // Empty stays its own, earlier error (charset check is skipped).
        let mut f = minimal();
        f.flow_id = "".into();
        let c = codes(&f);
        assert!(c.contains(&"empty-flow-id"));
        assert!(!c.contains(&"invalid-flow-id"));
    }

    #[test]
    fn duplicate_node_id_is_error() {
        let mut f = minimal();
        f.nodes.push(node("a", "transform"));
        assert!(codes(&f).contains(&"duplicate-node-id"));
        assert!(!f.is_valid());
    }

    #[test]
    fn allowed_hosts_validated() {
        // Plausible entries in the runner grammar all pass.
        let mut f = minimal();
        f.allowed_hosts = vec![
            "notify.example".into(),
            "api.example:8443".into(),
            "https://hooks.example".into(),
            "*.internal.example".into(),
        ];
        assert!(f.is_valid(), "{:?}", f.issues());

        // Empty and whitespace entries are structural errors in any grammar.
        for bad in ["", "two words", "tab\thost"] {
            let mut f = minimal();
            f.allowed_hosts = vec![bad.into()];
            assert!(
                codes(&f).contains(&"invalid-allowed-host"),
                "{bad:?} should be rejected"
            );
        }

        // Duplicates are errors.
        let mut f = minimal();
        f.allowed_hosts = vec!["notify.example".into(), "notify.example".into()];
        assert!(codes(&f).contains(&"duplicate-allowed-host"));
        assert!(!f.is_valid());
    }

    #[test]
    fn partition_policy_defaults_to_blocking_and_round_trips() {
        // Absent field = the blocking default; the default is omitted on export
        // so flows round-trip minimal (D20: choosing partitioned IS opting into
        // ordering — leapfrog is the explicit opt-out).
        let f = minimal();
        assert_eq!(f.partition_policy, PartitionPolicy::Blocking);
        assert!(!f.to_json().contains("partition-policy"));

        let mut f = minimal();
        f.partition_policy = PartitionPolicy::Leapfrog;
        let json = f.to_json();
        assert!(json.contains("\"partition-policy\": \"leapfrog\""));
        assert_eq!(
            Flow::from_json(&json).unwrap().partition_policy,
            PartitionPolicy::Leapfrog
        );

        // An unknown policy value is a parse error, not a silent default.
        let bad = json.replace("leapfrog", "yolo");
        assert!(Flow::from_json(&bad).is_err());
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

    #[test]
    fn ordering_defaults_to_unordered_and_round_trips() {
        // Absent = unordered; the default is omitted on export so flows
        // round-trip minimal (like partition-policy).
        let f = minimal();
        assert_eq!(f.ordering, Ordering::Unordered);
        assert!(f.is_valid());
        assert!(!f.to_json().contains("ordering"));

        // strict is a unit variant — no key to carry, by construction.
        let mut f = minimal();
        f.ordering = Ordering::Strict;
        let json = f.to_json();
        assert!(json.contains("\"ordering\""));
        assert!(json.contains("\"mode\": \"strict\""));
        assert_eq!(Flow::from_json(&json).unwrap().ordering, Ordering::Strict);
        assert!(f.is_valid(), "{:?}", f.issues());

        // partitioned round-trips with its kebab-case partition-key.
        let mut f = minimal();
        f.ordering = Ordering::Partitioned {
            partition_key: "payload.customer".into(),
        };
        let json = f.to_json();
        assert!(json.contains("\"mode\": \"partitioned\""));
        assert!(json.contains("\"partition-key\": \"payload.customer\""));
        assert_eq!(
            Flow::from_json(&json).unwrap().ordering,
            Ordering::Partitioned {
                partition_key: "payload.customer".into()
            }
        );
        assert!(f.is_valid(), "{:?}", f.issues());

        // An unknown mode is a parse error, not a silent default.
        let bad = json.replace("partitioned", "sideways");
        assert!(Flow::from_json(&bad).is_err());
    }

    #[test]
    fn partitioned_requires_a_compilable_jmespath_key() {
        // A syntactically broken JMESPath is rejected at validation, not left to
        // degrade silently at fire().
        let mut f = minimal();
        f.ordering = Ordering::Partitioned {
            partition_key: "payload.[".into(), // unbalanced bracket
        };
        assert!(codes(&f).contains(&"invalid-partition-key"));
        assert!(!f.is_valid());

        // An empty key is its own, more specific error.
        let mut f = minimal();
        f.ordering = Ordering::Partitioned {
            partition_key: "   ".into(),
        };
        let c = codes(&f);
        assert!(c.contains(&"empty-partition-key"));
        assert!(!c.contains(&"invalid-partition-key"));
        assert!(!f.is_valid());

        // A valid key is clean.
        let mut f = minimal();
        f.ordering = Ordering::Partitioned {
            partition_key: "payload.customer".into(),
        };
        assert!(f.is_valid(), "{:?}", f.issues());
    }

    #[test]
    fn partition_key_for_folds_the_input_to_a_stream_key() {
        let input = json!({"payload": {"customer": "acme", "count": 42, "vip": true}});

        // Unordered → None (the global claim), strict → the flow id.
        assert_eq!(Ordering::Unordered.partition_key_for("f", &input), None);
        assert_eq!(
            Ordering::Strict.partition_key_for("f", &input),
            Some("f".to_string())
        );

        // Partitioned scalars: string verbatim, number/bool stringified exactly.
        let key = |expr: &str| {
            Ordering::Partitioned {
                partition_key: expr.into(),
            }
            .partition_key_for("f", &input)
        };
        assert_eq!(key("payload.customer"), Some("acme".to_string()));
        assert_eq!(key("payload.count"), Some("42".to_string()));
        assert_eq!(key("payload.vip"), Some("true".to_string()));

        // Missing path / non-scalar result → the flow-wide stream (flow id),
        // NEVER None: a partitioned flow must not escape to the unordered claim.
        assert_eq!(key("payload.missing"), Some("f".to_string()));
        assert_eq!(key("payload"), Some("f".to_string())); // an object is not a key
    }
}

//! Check graph authoring against the application bound to this workload.

use std::collections::{HashMap, HashSet};

use crate::{GraphError, GraphNode, GraphTrigger, MAIN_PORT, WorkflowGraph, invalid};
use wamn_package::canonical_name::CanonicalName;

/// Entry and write requirements derived during graph authoring.
#[derive(Debug)]
pub struct Checked<'a> {
    pub entry: &'a GraphTrigger,
    pub can_write: bool,
}

/// Check trigger rules, call availability and terminating paths.
///
/// The lookup returns whether a declared export can write, or `None` when absent.
///
/// # Errors
/// Returns a graph refusal when a trigger or call violates the authoring rules.
pub fn check<'a>(
    graph: &'a WorkflowGraph,
    app: &str,
    role: Option<&str>,
    export_can_write: impl Fn(&CanonicalName, &CanonicalName) -> Option<bool>,
) -> Result<Checked<'a>, GraphError> {
    let mut entries = graph.nodes.iter().filter_map(|(id, node)| match node {
        GraphNode::Entry(entry) => Some((id, entry)),
        _ => None,
    });
    let Some((entry_id, entry)) = entries.next().filter(|_| entries.next().is_none()) else {
        return Err(invalid("workflow must have exactly one entry node"));
    };
    let request = matches!(entry, GraphTrigger::Request { .. });
    if role.is_some_and(|role| role.trim().is_empty()) || (!request && role.is_none()) {
        return Err(invalid("workflow requires a nonempty execution_role"));
    }
    match entry {
        GraphTrigger::Request { path } => {
            if !valid_segment(app)
                || !valid_segment(graph.graph_id.as_str())
                || !valid_exact_path(path)
            {
                return Err(invalid(
                    "Request workflow needs an exact path and valid permission identifiers",
                ));
            }
        }
        GraphTrigger::Schedule { cron } => {
            super::schedule::parse_workflow_schedule(cron)?;
        }
        GraphTrigger::DataChange {
            package,
            entity,
            ops,
            condition,
        } => {
            if ![package.as_str(), entity.as_str()]
                .into_iter()
                .all(wamn_event::valid_subject_token)
                || ops.is_empty()
                || ops
                    .iter()
                    .map(|op| op.as_str())
                    .collect::<HashSet<_>>()
                    .len()
                    != ops.len()
            {
                return Err(invalid(
                    "data-change workflow needs a source and distinct row operations",
                ));
            }
            if let Some(condition) = condition {
                wamn_event::matching::validate_condition(condition)
                    .map_err(|error| invalid(format!("invalid data-change condition: {error}")))?;
            }
        }
    }

    let mut writers = HashSet::new();
    for (id, node) in &graph.nodes {
        match node {
            GraphNode::Response { .. } if !request => {
                return Err(invalid("a response is only allowed in a Request workflow"));
            }
            GraphNode::Call {
                sibling, export, ..
            } => {
                let Some(can_write) = export_can_write(sibling, export) else {
                    return Err(invalid(format!(
                        "node {id} names an unavailable sibling export"
                    )));
                };
                if can_write {
                    writers.insert(id);
                }
            }
            _ => {}
        }
    }

    let mut outgoing: HashMap<&CanonicalName, Vec<&CanonicalName>> = HashMap::new();
    for edge in &graph.edges {
        if edge.from == *entry_id && edge.from_port != MAIN_PORT {
            return Err(invalid("an entry has only a main output"));
        }
        outgoing.entry(&edge.from).or_default().push(&edge.to);
    }
    let mut pending = vec![entry_id];
    let mut visited = HashSet::new();
    let mut can_write = false;
    let mut reached_response = false;
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        can_write |= writers.contains(id);
        if let Some(successors) = outgoing.get(id) {
            pending.extend(successors);
        } else if matches!(graph.nodes[id], GraphNode::Response { .. }) {
            reached_response = true;
        } else if request {
            return Err(invalid(format!(
                "Request workflow path ends at {id} without a response"
            )));
        }
    }
    if request && !reached_response {
        return Err(invalid("Request workflow needs a reachable response"));
    }
    Ok(Checked { entry, can_write })
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
}

fn valid_exact_path(path: &str) -> bool {
    path.strip_prefix('/').is_some_and(|path| {
        path.split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
    }) && !path.contains(['?', '#', '{', '}', '\\'])
        && path.parse::<http::uri::PathAndQuery>().is_ok()
}

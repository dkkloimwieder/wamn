//! Check graph authoring against the application bound to this workload.

use std::collections::{HashMap, HashSet};

use wamn_catalog::application_config::{ApplicationConfig, ApplicationOperationType};
use wamn_catalog::canonical_name::CanonicalName;
use wamn_catalog::{CatalogIdentityError, WiringDocument, WiringEntry, WiringNode};

pub(super) struct Checked<'a> {
    pub entry: &'a WiringEntry,
    pub can_write: bool,
}

pub(super) fn check<'a>(
    graph: &'a WiringDocument,
    application: &ApplicationConfig,
    role: Option<&str>,
) -> Result<Checked<'a>, CatalogIdentityError> {
    let mut entries = graph.nodes.iter().filter_map(|(id, node)| match node {
        WiringNode::Entry(entry) => Some((id, entry)),
        _ => None,
    });
    let Some((entry_id, entry)) = entries.next().filter(|_| entries.next().is_none()) else {
        return Err(invalid("workflow must have exactly one entry node"));
    };
    let request = matches!(entry, WiringEntry::Request { .. });
    if role.is_some_and(|role| role.trim().is_empty()) || (!request && role.is_none()) {
        return Err(invalid("workflow requires a nonempty execution_role"));
    }
    match entry {
        WiringEntry::Request { path } => {
            if !valid_segment(application.application.as_str())
                || !valid_segment(graph.wiring_id.as_str())
                || !valid_exact_path(path)
            {
                return Err(invalid(
                    "Request workflow needs an exact path and valid permission identifiers",
                ));
            }
        }
        WiringEntry::Schedule { cron } => {
            wamn_catalog::parse_workflow_schedule(cron)?;
        }
        WiringEntry::DataChange {
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
                jmespath::compile(condition)
                    .map_err(|error| invalid(format!("invalid data-change condition: {error}")))?;
            }
        }
    }

    let mut writers = HashSet::new();
    for (id, node) in &graph.nodes {
        match node {
            WiringNode::Response { .. } if !request => {
                return Err(invalid("a response is only allowed in a Request workflow"));
            }
            WiringNode::Call {
                sibling, export, ..
            } => {
                let Some((component, operation)) = application
                    .components
                    .iter()
                    .find(|component| component.name == sibling.as_str())
                    .and_then(|component| Some((component, component.operations.get(export)?)))
                else {
                    return Err(invalid(format!(
                        "node {id} names an unavailable sibling export"
                    )));
                };
                let palette = component.package.as_str() == wamn_catalog::PALETTE_PACKAGE_ID;
                let can_write = match operation.type_ {
                    Some(ApplicationOperationType::Write | ApplicationOperationType::Delete) => {
                        true
                    }
                    type_ if !palette => type_.is_none(),
                    _ => !matches!(
                        component.name.as_str(),
                        "wamn_palette-jsonata"
                            | "wamn_palette-transform"
                            | "wamn_palette-label-render"
                    ),
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
        if edge.from == *entry_id && edge.from_port != wamn_catalog::MAIN_PORT {
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
        } else if matches!(graph.nodes[id], WiringNode::Response { .. }) {
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
        && path.parse::<hyper::http::uri::PathAndQuery>().is_ok()
}

fn invalid(message: impl Into<String>) -> CatalogIdentityError {
    CatalogIdentityError::InvalidDefinition {
        message: message.into(),
    }
}

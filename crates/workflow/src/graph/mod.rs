//! The private graph walk and its invocation outcomes.
mod outcome;
mod retry;
mod terminal;
mod walk;
mod wiring;

pub(crate) use outcome::{ErrorDetail, NodeError, NodeOutcome, RateLimitDetail};
pub use terminal::Verdict;
#[cfg(test)]
pub(crate) use walk::NodeCall;
pub(crate) use walk::{Delivery, Step};
pub use walk::{Failure, FailureType, WalkStatus};
pub(crate) use wiring::Wiring;

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub status: WalkStatus,
    pub result: serde_json::Value,
    pub failure: Option<Failure>,
    pub hops: u64,
    pub verdict: Option<Verdict>,
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod route_tests;
#[cfg(test)]
#[path = "terminal_tests.rs"]
mod terminal_tests;
#[cfg(test)]
#[path = "walk_tests.rs"]
mod walk_tests;

#[cfg(test)]
fn components(
    document: &wamn_catalog::WiringDocument,
) -> std::collections::BTreeMap<String, wamn_catalog::AdmittedComponent> {
    use std::collections::BTreeMap;
    use wamn_catalog::{
        AdmittedComponent, AdmittedComponentOperation, AdmittedComponentPort,
        ComponentPackageCoordinate, ComponentSchema,
    };
    let project_env_key = "t1--dev--project".parse().unwrap();
    document
        .nodes
        .iter()
        .map(|(id, node)| {
            let mut ports = BTreeMap::from([("input".to_owned(), serde_json::json!({}))]);
            for edge in document.edges.iter().filter(|edge| edge.to == *id) {
                ports.insert(
                    edge.to_port.as_deref().unwrap_or("input").to_owned(),
                    serde_json::json!({}),
                );
            }
            let operation = AdmittedComponentOperation {
                registered_operation: None,
                host_transaction: false,
                committed_result_schema: None,
                input_ports: ports
                    .into_iter()
                    .map(|(name, schema)| AdmittedComponentPort {
                        name,
                        schema: ComponentSchema { schema },
                    })
                    .collect(),
                output_ports: Vec::new(),
                parameters: Vec::new(),
            };
            (
                id.clone(),
                AdmittedComponent {
                    target: ComponentPackageCoordinate::new(
                        &project_env_key,
                        "test".parse().unwrap(),
                        "1.0.0".parse().unwrap(),
                    )
                    .unwrap(),
                    component: node.component.clone(),
                    interface_version: node.interface_version.clone(),
                    operations: BTreeMap::from([(node.operation.clone(), operation)]),
                    component_digest: format!("sha256:{}", "a".repeat(64)).parse().unwrap(),
                    imports: Vec::new(),
                    imports_fingerprint: format!("sha256:{}", "b".repeat(64)).parse().unwrap(),
                    effects: Vec::new(),
                },
            )
        })
        .collect()
}

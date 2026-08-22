//! Standard-node palette metadata retained for flow authoring.
//!
//! This crate exposes no runnable node implementation.

use std::sync::LazyLock;

use wamn_flow::MAIN_PORT;
use wamn_flow::node_contract::{Capability, ConnectionRequirement, EffectPolicy, NodeInterface};

const NODE_TYPES: [&str; 8] = [
    "request",
    "event",
    "fail",
    "transform",
    "conditional",
    "http-request",
    "postgres-query",
    "respond",
];

static INTERFACES: LazyLock<[NodeInterface; 8]> = LazyLock::new(|| {
    [
        pure_interface(NODE_TYPES[0], &[MAIN_PORT]),
        pure_interface(NODE_TYPES[1], &[MAIN_PORT]),
        pure_interface(NODE_TYPES[2], &[MAIN_PORT]),
        pure_interface(NODE_TYPES[3], &[MAIN_PORT]),
        pure_interface(NODE_TYPES[4], &["false", "true"]),
        http_interface(NODE_TYPES[5]),
        postgres_interface(NODE_TYPES[6], &[Capability::Postgres, Capability::RawSql]),
        pure_interface(NODE_TYPES[7], &[MAIN_PORT]),
    ]
});

fn node_interface(
    node_type: &str,
    output_ports: &[&str],
    capabilities: &[Capability],
    connection_requirements: Vec<ConnectionRequirement>,
    effect_policy: EffectPolicy,
) -> NodeInterface {
    NodeInterface {
        node_type: node_type.to_string(),
        output_ports: output_ports
            .iter()
            .map(|port| (*port).to_string())
            .collect(),
        capabilities: capabilities.to_vec(),
        connection_requirements,
        effect_policy,
    }
}

fn pure_interface(node_type: &str, output_ports: &[&str]) -> NodeInterface {
    node_interface(node_type, output_ports, &[], Vec::new(), EffectPolicy::Pure)
}

fn effectful_interface(
    node_type: &str,
    capabilities: &[Capability],
    connection_requirements: Vec<ConnectionRequirement>,
) -> NodeInterface {
    node_interface(
        node_type,
        &[MAIN_PORT],
        capabilities,
        connection_requirements,
        EffectPolicy::Effectful,
    )
}

fn http_interface(node_type: &str) -> NodeInterface {
    effectful_interface(
        node_type,
        &[Capability::HttpEgress],
        vec![ConnectionRequirement {
            requirement_type: "http".to_string(),
            contract: "wamn:connection/http@0.1.0".to_string(),
        }],
    )
}

fn postgres_interface(node_type: &str, capabilities: &[Capability]) -> NodeInterface {
    effectful_interface(
        node_type,
        capabilities,
        vec![ConnectionRequirement {
            requirement_type: "postgres".to_string(),
            contract: "wamn:connection/postgres@0.1.0".to_string(),
        }],
    )
}

/// Returns the retained interface metadata for a standard node type.
pub fn describe_interface(node_type: &str) -> Option<&'static NodeInterface> {
    INTERFACES
        .iter()
        .find(|interface| interface.node_type == node_type)
}

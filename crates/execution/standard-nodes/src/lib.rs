//! # wamn-standard-nodes — the standard node library v1 (5.3)
//!
//! MVP outcome: M0 node set.
//!
//! The production node vocabulary, authored against the flow model's pure
//! node contract — **never the runner crate** (the purity rule, enforced
//! mechanically by this crate's `purity_lint` test over `cargo metadata`).
//! Every effect flows through the contract's [`NodeCtx`] capability facade, gated
//! by each node's [`NodeInterface`] plus [`dispatch`]'s grant check and the
//! internal gated context.
//!
//! | node type        | capabilities            | what it does |
//! |------------------|-------------------------|--------------|
//! | `request`        | —                       | emit the admitted request payload unchanged |
//! | `event`          | —                       | emit the externally admitted event payload unchanged |
//! | `fail`           | —                       | return the authored terminal failure detail |
//! | `transform`      | —                       | reshape the payload with a JMESPath expression |
//! | `conditional`    | —                       | branch `true`/`false` on a JMESPath predicate |
//! | `http-request`   | `HttpEgress`            | one outbound HTTP call, taxonomy-classified |
//! | `postgres`       | `Postgres`              | catalog-derived entity ops via the audited 4.1 surface |
//! | `postgres-query` | `Postgres` + `RawSql`   | author-written SQL, `$n`-bound — D8 flag, DEFAULT OFF |
//! | `respond`        | —                       | webhook-response terminal (status via [`respond::status_for`]) |
//!
//! Event and request admission remain outside their capability-free node data
//! paths. Loops are STRUCTURAL (cycles + `conditional` express them); dedicated
//! split/merge nodes require a separate frame/join design. `email`/`notify`
//! wait for an email egress capability decision.
//! Expression power is the JMESPath spec plus the single `context()` reader;
//! standard nodes may attach a whole context replacement with config `ctx`.

mod conditional;
mod event;
mod expr;
mod fail;
mod http;
mod policy;
mod postgres;
mod request;
mod template;
mod transform;

pub mod respond;

pub use conditional::{FALSE_PORT, TRUE_PORT};
pub use http::prepare_http_request;
pub use policy::{GRANTS_DEFAULT, GRANTS_WITH_RAW_SQL, granted_for};
use std::sync::LazyLock;

use serde_json::Value;
use wamn_flow::MAIN_PORT;
use wamn_flow::node_contract::{
    Capability, ConnectionRequirement, EffectPolicy, Emission, ErrorDetail, Node, NodeCtx,
    NodeError, NodeInterface, RunContext,
};

/// Every node type this library implements (drift-guarded by docs + tests).
pub const NODE_TYPES: [&str; 9] = [
    "request",
    "event",
    "fail",
    "transform",
    "conditional",
    "http-request",
    "postgres",
    "postgres-query",
    "respond",
];

static REQUEST: request::Request = request::Request;
static EVENT: event::Event = event::Event;
static FAIL: fail::Fail = fail::Fail;
static TRANSFORM: transform::Transform = transform::Transform;
static CONDITIONAL: conditional::Conditional = conditional::Conditional;
static HTTP_REQUEST: http::HttpRequestNode = http::HttpRequestNode;
static POSTGRES: postgres::PostgresEntity = postgres::PostgresEntity;
static POSTGRES_QUERY: postgres::PostgresQuery = postgres::PostgresQuery;
static RESPOND: respond::Respond = respond::Respond;

/// The implementation behind a standard node type, if this library ships it.
///
/// C2-3 (wamn-bd5): this is **`pub(crate)`**, not `pub`. Handing out a runnable
/// `&dyn Node` bypasses the dispatch-time capability gate ([`dispatch`]'s grant
/// check + the narrowing [`policy::GatedCtx`]) — an external caller could
/// `node(t).run(unnarrowed_ctx, ..)` and reach a capability the node never
/// declared. So the ONLY way out of this crate to *run* a standard node is
/// [`dispatch`]; callers that merely need to know a type exists or what it may
/// use take the interface surface ([`describe_interface`] / [`is_standard`] /
/// [`required_capabilities`]), which cannot run anything.
pub(crate) fn node(node_type: &str) -> Option<&'static dyn Node> {
    match node_type {
        "request" => Some(&REQUEST),
        "event" => Some(&EVENT),
        "fail" => Some(&FAIL),
        "transform" => Some(&TRANSFORM),
        "conditional" => Some(&CONDITIONAL),
        "http-request" => Some(&HTTP_REQUEST),
        "postgres" => Some(&POSTGRES),
        "postgres-query" => Some(&POSTGRES_QUERY),
        "respond" => Some(&RESPOND),
        _ => None,
    }
}

static INTERFACES: LazyLock<[NodeInterface; 9]> = LazyLock::new(|| {
    [
        pure_interface(NODE_TYPES[0], &[MAIN_PORT]),
        pure_interface(NODE_TYPES[1], &[MAIN_PORT]),
        pure_interface(NODE_TYPES[2], &[MAIN_PORT]),
        pure_interface(NODE_TYPES[3], &[MAIN_PORT]),
        pure_interface(NODE_TYPES[4], &["false", "true"]),
        http_interface(NODE_TYPES[5]),
        postgres_interface(NODE_TYPES[6], &[Capability::Postgres]),
        postgres_interface(NODE_TYPES[7], &[Capability::Postgres, Capability::RawSql]),
        pure_interface(NODE_TYPES[8], &[MAIN_PORT]),
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

/// The interface for a shipped standard node. No runnable handle is exposed.
pub fn describe_interface(node_type: &str) -> Option<&'static NodeInterface> {
    INTERFACES
        .iter()
        .find(|interface| interface.node_type == node_type)
}

/// Whether this library ships `node_type` — the existence check the flow-runner
/// makes before treating a step as a standard node. A non-running replacement
/// for the old `node(t).is_some()` leak (C2-3).
pub fn is_standard(node_type: &str) -> bool {
    describe_interface(node_type).is_some()
}

/// The capability policy row for a node type — what a dispatch of it may use.
pub fn required_capabilities(node_type: &str) -> Option<&'static [Capability]> {
    describe_interface(node_type).map(|interface| interface.capabilities.as_slice())
}

/// Dispatch one standard node under the policy table:
///
/// 1. the node type must exist (`Terminal("unknown-node-type")` otherwise);
/// 2. its declared capability row must be covered by `granted`
///    (`Terminal("capability-denied")` otherwise — this is where a
///    `postgres-query` dispatch dies when the D8 flag is off);
/// 3. the node runs against a ctx NARROWED to its declared row, so even a
///    buggy implementation cannot reach an undeclared capability.
pub fn dispatch(
    node_type: &str,
    granted: &[Capability],
    ctx: &mut dyn NodeCtx,
    run: &RunContext<'_>,
    input: &Value,
) -> Result<Emission, NodeError> {
    let Some(node) = node(node_type) else {
        return Err(NodeError::Terminal(ErrorDetail::coded(
            "unknown-node-type",
            format!("no standard node type {node_type:?}"),
        )));
    };
    let replacement_expr = match run.config.get("ctx") {
        None => None,
        Some(Value::String(expr)) => Some(expr.as_str()),
        Some(_) => {
            return Err(NodeError::Terminal(ErrorDetail::coded(
                "invalid-config",
                "standard-node \"ctx\" must be a JMESPath expression string",
            )));
        }
    };
    let declared = describe_interface(node_type)
        .expect("a runnable standard node has one complete interface")
        .capabilities
        .as_slice();
    policy::check_grants(node_type, declared, granted)?;
    let mut gated = policy::GatedCtx {
        inner: ctx,
        allowed: declared,
    };
    let mut emission = node.run(&mut gated, run, input)?;
    if let Some(replacement_expr) = replacement_expr {
        let replacement = expr::eval_to_value(replacement_expr, &emission.payload, run.context)?;
        if !replacement.is_object() {
            return Err(NodeError::Terminal(ErrorDetail::coded(
                "invalid-context",
                format!(
                    "standard-node \"ctx\" expression {replacement_expr:?} must yield an object, got {replacement}"
                ),
            )));
        }
        emission.ctx = Some(replacement);
    }
    Ok(emission)
}

#[cfg(test)]
mod interface_tests {
    use super::*;

    #[test]
    fn private_implementations_match_their_interface_capabilities() {
        for interface in INTERFACES.iter() {
            let implementation =
                node(&interface.node_type).expect("interface has an implementation");
            assert_eq!(
                implementation.capabilities(),
                interface.capabilities,
                "interface is the retained public authority for {}",
                interface.node_type
            );
        }
    }
}

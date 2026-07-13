//! # wamn-nodes — the standard node library v1 (5.3)
//!
//! The production node vocabulary, authored against the `wamn-node-sdk`
//! contract ONLY — **never the runner crate** (the 5.13 purity rule, enforced
//! mechanically by this crate's `purity_lint` test over `cargo metadata`).
//! Every effect flows through the SDK's [`NodeCtx`] capability facade, gated
//! by the dispatch-time policy table ([`required_capabilities`] +
//! [`dispatch`]'s grant check + the internal gated context).
//!
//! | node type        | capabilities            | what it does |
//! |------------------|-------------------------|--------------|
//! | `transform`      | —                       | reshape the payload with a JMESPath expression |
//! | `conditional`    | —                       | branch `true`/`false` on a JMESPath predicate |
//! | `http-request`   | `HttpEgress`            | one outbound HTTP call, taxonomy-classified |
//! | `postgres`       | `Postgres`              | catalog-derived entity ops via the audited 4.1 surface |
//! | `postgres-query` | `Postgres` + `RawSql`   | author-written SQL, `$n`-bound — D8 flag, DEFAULT OFF |
//! | `respond`        | —                       | webhook-response terminal (status via [`respond::status_for`]) |
//!
//! Deliberately NOT here (v1 scope decisions, wamn-3xa): `delay` and the
//! trigger entry are runner-intrinsic (parking and trigger payloads are engine
//! concerns, not node effects); loops are STRUCTURAL (cycles + `conditional`
//! express them; dedicated split/merge nodes land with the 5.11 ordering
//! semantics); `email`/`notify` wait for an email egress capability decision.
//! Expression power is exactly the JMESPath spec — off the shelf, no language
//! of our own to maintain.

mod conditional;
mod expr;
mod http;
mod policy;
mod postgres;
mod template;
mod transform;

pub mod respond;

pub use conditional::{FALSE_PORT, TRUE_PORT};
pub use policy::{GRANTS_DEFAULT, GRANTS_WITH_RAW_SQL, granted_for};
pub use wamn_node_sdk::{
    Capability, Emission, ErrorDetail, HttpCapError, HttpRequest, HttpResponse, Node, NodeCtx,
    NodeError, PgCapError, PgRows, PgValue, RateLimitDetail, RunContext,
};

use serde_json::Value;

/// Every node type this library implements (drift-guarded by docs + tests).
pub const NODE_TYPES: [&str; 6] = [
    "transform",
    "conditional",
    "http-request",
    "postgres",
    "postgres-query",
    "respond",
];

static TRANSFORM: transform::Transform = transform::Transform;
static CONDITIONAL: conditional::Conditional = conditional::Conditional;
static HTTP_REQUEST: http::HttpRequestNode = http::HttpRequestNode;
static POSTGRES: postgres::PostgresEntity = postgres::PostgresEntity;
static POSTGRES_QUERY: postgres::PostgresQuery = postgres::PostgresQuery;
static RESPOND: respond::Respond = respond::Respond;

/// The implementation behind a standard node type, if this library ships it.
pub fn node(node_type: &str) -> Option<&'static dyn Node> {
    match node_type {
        "transform" => Some(&TRANSFORM),
        "conditional" => Some(&CONDITIONAL),
        "http-request" => Some(&HTTP_REQUEST),
        "postgres" => Some(&POSTGRES),
        "postgres-query" => Some(&POSTGRES_QUERY),
        "respond" => Some(&RESPOND),
        _ => None,
    }
}

/// The capability policy row for a node type — what a dispatch of it may use.
pub fn required_capabilities(node_type: &str) -> Option<&'static [Capability]> {
    node(node_type).map(|n| n.capabilities())
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
    let declared = node.capabilities();
    policy::check_grants(node_type, declared, granted)?;
    let mut gated = policy::GatedCtx {
        inner: ctx,
        allowed: declared,
    };
    node.run(&mut gated, run, input)
}

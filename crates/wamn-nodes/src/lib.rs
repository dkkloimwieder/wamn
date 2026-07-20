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
    Capability, CredentialCapError, Emission, ErrorDetail, HttpCapError, HttpRequest, HttpResponse,
    Node, NodeCtx, NodeError, PgCapError, PgRows, PgValue, RateLimitDetail, RunContext,
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
///
/// C2-3 (wamn-bd5): this is **`pub(crate)`**, not `pub`. Handing out a runnable
/// `&dyn Node` bypasses the dispatch-time capability gate ([`dispatch`]'s grant
/// check + the narrowing [`policy::GatedCtx`]) — an external caller could
/// `node(t).run(unnarrowed_ctx, ..)` and reach a capability the node never
/// declared. So the ONLY way out of this crate to *run* a standard node is
/// [`dispatch`]; callers that merely need to know a type exists or what it may
/// use take the descriptor surface ([`describe`] / [`is_standard`] /
/// [`required_capabilities`]), which cannot run anything.
pub(crate) fn node(node_type: &str) -> Option<&'static dyn Node> {
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

/// What a standard node type IS, without a handle that can run it (C2-3): the
/// type name and its declared capability row. This is the public resolution
/// surface — a caller inspecting the library (does this type exist? what may it
/// touch?) gets a descriptor, never a runnable `&dyn Node`. To actually execute
/// a standard node, go through [`dispatch`], which gates on the grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeDescriptor {
    /// The node type this describes (one of [`NODE_TYPES`]).
    pub node_type: &'static str,
    /// The capabilities a dispatch of it may use (its policy row).
    pub capabilities: &'static [Capability],
}

/// The descriptor for a standard node type, or `None` if this library does not
/// ship it. The runnable node stays behind the [`dispatch`] gate (C2-3).
pub fn describe(node_type: &str) -> Option<NodeDescriptor> {
    NODE_TYPES
        .iter()
        .find(|t| **t == node_type)
        .and_then(|t| node(t).map(|n| (t, n)))
        .map(|(t, n)| NodeDescriptor {
            node_type: t,
            capabilities: n.capabilities(),
        })
}

/// Whether this library ships `node_type` — the existence check the flow-runner
/// makes before treating a step as a standard node. A non-running replacement
/// for the old `node(t).is_some()` leak (C2-3).
pub fn is_standard(node_type: &str) -> bool {
    describe(node_type).is_some()
}

/// The capability policy row for a node type — what a dispatch of it may use.
pub fn required_capabilities(node_type: &str) -> Option<&'static [Capability]> {
    describe(node_type).map(|d| d.capabilities)
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

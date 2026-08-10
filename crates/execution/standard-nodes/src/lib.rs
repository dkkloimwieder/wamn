//! # wamn-standard-nodes — the standard node library v1 (5.3)
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
//! Deliberately NOT here (v1 scope decisions, wamn-3xa): `delay` is
//! runner-intrinsic (parking is an engine concern, not a node effect); event and
//! request admission remain outside their capability-free node data paths. Loops are
//! STRUCTURAL (cycles + `conditional`
//! express them; dedicated split/merge nodes land with the 5.11 ordering
//! semantics); `email`/`notify` wait for an email egress capability decision.
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
use std::fmt;
use std::sync::LazyLock;

use serde_json::Value;
use wamn_flow::node_contract::{
    Capability, ConnectionRequirement, EffectPolicy, Emission, ErrorDetail, Node, NodeCtx,
    NodeError, NodeInterface, RunContext,
};
use wamn_node_manifest::{
    CapabilityClass, ConnectionRecoverySupport,
    ConnectionRequirement as LegacyConnectionRequirement, ConnectionTypeDescriptor,
    ExecutableConnectionRecoveryMode, ExecutableIdentity, ExecutableRecoveryClaim,
    ExecutableRecoveryContract, IdempotencyKeyInjection, NODE_WORLD_INTERFACE,
    PortableConnectionRequirement, ResolvedNodeContract, ResolvedNodeInterface,
};

/// Temporary shape version for the E3-owned legacy standard-node descriptor.
pub const STANDARD_NODE_DESCRIPTOR_VERSION: &str = "1";

/// Temporary executable revision used by the E3-owned legacy descriptor.
pub const STANDARD_NODE_PLATFORM_REVISION: &str = "wamn-standard-nodes@0.1.0";

const STANDARD_NODE_INTERFACE: &str = NODE_WORLD_INTERFACE;

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
/// use take the descriptor surface ([`describe`] / [`is_standard`] /
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

/// E3-owned compatibility descriptor for the custom publication plane.
///
/// Retained execution and publication code must use [`describe_interface`].
/// This type and [`resolve_descriptor`] remain unchanged until E3 deletes
/// their remaining consumers with the custom-node plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeDescriptor {
    pub descriptor_version: String,
    pub node_type: String,
    pub interface_contract: String,
    pub output_ports: Vec<String>,
    pub capability_classes: Vec<CapabilityClass>,
    pub connection_requirements: Vec<LegacyConnectionRequirement>,
    pub platform_revision: String,
    pub executable_recovery: ExecutableRecoveryContract,
    pub connection_recovery_support: Vec<ConnectionRecoverySupport>,
    pub portable_connections: Vec<PortableConnectionRequirement>,
    pub dispatch_capabilities: &'static [Capability],
}

/// A legacy standard descriptor could not be converted without fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescriptorError {
    message: String,
}

impl fmt::Display for DescriptorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DescriptorError {}

static DESCRIPTORS: LazyLock<[NodeDescriptor; 9]> = LazyLock::new(|| {
    [
        pure_descriptor(NODE_TYPES[0], &["main"]),
        pure_descriptor(NODE_TYPES[1], &["main"]),
        pure_descriptor(NODE_TYPES[2], &["main"]),
        pure_descriptor(NODE_TYPES[3], &["main"]),
        pure_descriptor(NODE_TYPES[4], &["false", "true"]),
        http_descriptor(NODE_TYPES[5]),
        postgres_descriptor(NODE_TYPES[6], &[Capability::Postgres]),
        postgres_descriptor(NODE_TYPES[7], &[Capability::Postgres, Capability::RawSql]),
        pure_descriptor(NODE_TYPES[8], &["main"]),
    ]
});

fn descriptor(
    node_type: &str,
    output_ports: &[&str],
    capability_classes: Vec<CapabilityClass>,
    executable_recovery: ExecutableRecoveryContract,
    dispatch_capabilities: &'static [Capability],
) -> NodeDescriptor {
    NodeDescriptor {
        descriptor_version: STANDARD_NODE_DESCRIPTOR_VERSION.to_string(),
        node_type: node_type.to_string(),
        interface_contract: STANDARD_NODE_INTERFACE.to_string(),
        output_ports: output_ports
            .iter()
            .map(|port| (*port).to_string())
            .collect(),
        capability_classes,
        connection_requirements: Vec::new(),
        platform_revision: STANDARD_NODE_PLATFORM_REVISION.to_string(),
        executable_recovery,
        connection_recovery_support: Vec::new(),
        portable_connections: Vec::new(),
        dispatch_capabilities,
    }
}

fn pure_descriptor(node_type: &str, output_ports: &[&str]) -> NodeDescriptor {
    descriptor(
        node_type,
        output_ports,
        vec![CapabilityClass::Pure],
        ExecutableRecoveryContract::pure(),
        &[],
    )
}

fn effectful_descriptor(
    node_type: &str,
    dispatch_capabilities: &'static [Capability],
) -> NodeDescriptor {
    descriptor(
        node_type,
        &["main"],
        capability_classes(dispatch_capabilities),
        ExecutableRecoveryContract::effectful(false),
        dispatch_capabilities,
    )
}

fn http_descriptor(node_type: &str) -> NodeDescriptor {
    let connection = ConnectionTypeDescriptor::http_v1();
    let mut descriptor = effectful_descriptor(node_type, &[Capability::HttpEgress]);
    descriptor.executable_recovery = ExecutableRecoveryContract::effectful(true);
    descriptor.connection_requirements = vec![LegacyConnectionRequirement {
        requirement_type: connection.requirement_type.clone(),
        contract: connection.contract.clone(),
    }];
    descriptor.connection_recovery_support = vec![ConnectionRecoverySupport {
        descriptor: connection.clone(),
        supported_modes: vec![
            ExecutableConnectionRecoveryMode::NeverReplay,
            ExecutableConnectionRecoveryMode::IdempotentWithKey {
                claim: ExecutableRecoveryClaim::StableKeyDedupV1,
                key_propagation: IdempotencyKeyInjection::HttpIdempotencyKeyHeader,
            },
        ],
    }];
    descriptor.portable_connections = vec![
        PortableConnectionRequirement::never_replay(connection.clone()),
        PortableConnectionRequirement::stable_key_dedup_v1(connection, 86_400_000),
    ];
    descriptor
}

fn postgres_descriptor(
    node_type: &str,
    dispatch_capabilities: &'static [Capability],
) -> NodeDescriptor {
    let mut descriptor = effectful_descriptor(node_type, dispatch_capabilities);
    descriptor.connection_requirements = vec![LegacyConnectionRequirement {
        requirement_type: "postgres".to_string(),
        contract: "wamn:connection/postgres@0.1.0".to_string(),
    }];
    descriptor
}

fn capability_classes(capabilities: &[Capability]) -> Vec<CapabilityClass> {
    let mut classes = capabilities
        .iter()
        .map(|capability| match capability {
            Capability::HttpEgress => CapabilityClass::Http,
            Capability::Postgres | Capability::RawSql => CapabilityClass::Postgres,
        })
        .collect::<Vec<_>>();
    classes.sort();
    classes.dedup();
    classes
}

/// The temporary legacy descriptor for a shipped standard node.
pub fn describe(node_type: &str) -> Option<&'static NodeDescriptor> {
    DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.node_type == node_type)
}

/// Convert a temporary legacy descriptor into the custom publication contract.
pub fn resolve_descriptor(
    descriptor: &NodeDescriptor,
) -> Result<ResolvedNodeContract, DescriptorError> {
    if descriptor.descriptor_version != STANDARD_NODE_DESCRIPTOR_VERSION {
        return Err(DescriptorError {
            message: format!(
                "unsupported standard-node descriptor version {:?}",
                descriptor.descriptor_version
            ),
        });
    }
    let expected_classes = if descriptor.dispatch_capabilities.is_empty()
        && descriptor.executable_recovery.purity == wamn_node_manifest::ResolvedPurity::Pure
    {
        vec![CapabilityClass::Pure]
    } else {
        capability_classes(descriptor.dispatch_capabilities)
    };
    if descriptor.capability_classes != expected_classes {
        return Err(DescriptorError {
            message: "canonical capability classes disagree with the dispatch capability row"
                .to_string(),
        });
    }
    Ok(ResolvedNodeContract {
        interface: ResolvedNodeInterface::new(
            descriptor.node_type.clone(),
            descriptor.interface_contract.clone(),
            descriptor.output_ports.clone(),
            descriptor.capability_classes.clone(),
            descriptor.connection_requirements.clone(),
        ),
        executable: ExecutableIdentity::Platform {
            revision: descriptor.platform_revision.clone(),
        },
        executable_recovery: descriptor.executable_recovery.clone(),
        connection_recovery_support: descriptor.connection_recovery_support.clone(),
        portable_connections: descriptor.portable_connections.clone(),
    })
}

static INTERFACES: LazyLock<[NodeInterface; 9]> = LazyLock::new(|| {
    [
        pure_interface(NODE_TYPES[0], &["main"]),
        pure_interface(NODE_TYPES[1], &["main"]),
        pure_interface(NODE_TYPES[2], &["main"]),
        pure_interface(NODE_TYPES[3], &["main"]),
        pure_interface(NODE_TYPES[4], &["false", "true"]),
        http_interface(NODE_TYPES[5]),
        postgres_interface(NODE_TYPES[6], &[Capability::Postgres]),
        postgres_interface(NODE_TYPES[7], &[Capability::Postgres, Capability::RawSql]),
        pure_interface(NODE_TYPES[8], &["main"]),
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
        &["main"],
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

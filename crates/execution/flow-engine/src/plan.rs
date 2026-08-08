//! The compiled flow: a validated [`wamn_flow::Flow`] indexed for O(1) node
//! lookup and outgoing-edge routing. The reducer ([`crate::engine`]) borrows a
//! `Plan`; the driver compiles one per active flow version and swaps it on
//! hot-reload.

use std::collections::HashMap;

use wamn_flow::{Edge, Flow, Issue, Node, ResolvedInterfaces};

/// Why a flow could not be compiled into a runnable plan.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineError {
    /// The flow failed structural validation (`wamn_flow::validate`). The engine
    /// refuses to run a flow that has not passed 5.1 validation.
    Invalid(Vec<Issue>),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Invalid(issues) => {
                write!(f, "invalid flow ({} issue(s))", issues.len())?;
                for i in issues {
                    write!(f, "; {i}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for EngineError {}

/// Default per-invocation node-execution budget (see
/// [`Plan::set_dispatch_budget`]): generous for a legitimate loop, but bounds a
/// permitted cycle that never terminates (cjv.4 / review C2-1).
pub const DEFAULT_DISPATCH_BUDGET: u64 = 10_000;

/// A validated flow, indexed by node id and by `(from-node, from-port)` for edge
/// routing. Borrows the flow for its lifetime.
#[derive(Debug)]
pub struct Plan<'f> {
    flow: &'f Flow,
    by_id: HashMap<&'f str, &'f Node>,
    /// All edges leaving a node, grouped by the from-node id (filtered by port at
    /// lookup — a node has only a handful of outgoing edges).
    out: HashMap<&'f str, Vec<&'f Edge>>,
    /// Max node executions (including retries) `next` hands out per invocation
    /// before the run fails `RunawayBudget`. See [`Plan::set_dispatch_budget`].
    pub(crate) dispatch_budget: u64,
}

impl<'f> Plan<'f> {
    /// Validate (`Flow::validate` — errors only) against the release's resolved
    /// node interfaces and index the flow. On success the caller may rely on
    /// every structural guarantee 5.1 checks.
    pub fn compile(
        flow: &'f Flow,
        resolved_interfaces: &ResolvedInterfaces,
    ) -> Result<Plan<'f>, EngineError> {
        flow.validate(resolved_interfaces)
            .map_err(EngineError::Invalid)?;

        let mut by_id = HashMap::with_capacity(flow.nodes.len());
        for node in &flow.nodes {
            by_id.insert(node.id.as_str(), node);
        }

        let mut out: HashMap<&str, Vec<&Edge>> = HashMap::new();
        for edge in &flow.edges {
            out.entry(edge.from.as_str()).or_default().push(edge);
        }
        // Fan-out order is the explicit `Edge::ordinal`, not array position.
        // Ordinals are scoped to a `(from, from-port)` group, so sorting a whole
        // from-node group here and filtering by port in `successors` yields each
        // port's edges in ordinal order. Stable, so a flow built in Rust without
        // ordinals keeps the order its `edges` were built in.
        for edges in out.values_mut() {
            edges.sort_by_key(|edge| edge.ordinal.unwrap_or(0));
        }

        Ok(Plan {
            flow,
            by_id,
            out,
            dispatch_budget: DEFAULT_DISPATCH_BUDGET,
        })
    }

    /// Override the per-invocation node-execution budget (cjv.4). Cycles are a
    /// flow feature, so termination is bounded at runtime instead: once `next`
    /// has handed out this many dispatches (retries included) for one
    /// [`crate::ExecutionState`], the run fails with the terminal
    /// [`ExecutionFailureKind::RunawayBudget`](crate::ExecutionFailureKind::RunawayBudget) — never
    /// routed to an error path, which could itself be part of the loop.
    /// Reconstruction ([`Plan::resume`]) is exempt, so a parked-and-resumed
    /// long run never trips the budget on its recorded history.
    pub fn set_dispatch_budget(&mut self, budget: u64) {
        self.dispatch_budget = budget;
    }

    /// The per-invocation node-execution budget in force.
    pub fn dispatch_budget(&self) -> u64 {
        self.dispatch_budget
    }

    /// The flow this plan was compiled from.
    pub fn flow(&self) -> &'f Flow {
        self.flow
    }

    /// The flow's stable id.
    pub fn flow_id(&self) -> &'f str {
        &self.flow.flow_id
    }

    /// The flow version.
    pub fn version(&self) -> u32 {
        self.flow.version
    }

    /// The entry node (guaranteed present post-compile).
    pub fn entry(&self) -> &'f Node {
        self.flow
            .entry_node()
            .expect("validated plan has exactly one entry node")
    }

    /// Look up a node by id.
    pub fn node(&self, id: &str) -> Option<&'f Node> {
        self.by_id.get(id).copied()
    }

    /// The edges leaving `node` on `port` (empty if none). Order follows each
    /// edge's explicit [`wamn_flow::Edge::ordinal`] within its `(from, from-port)`
    /// group, so fan-out to several targets is deterministic.
    pub fn successors(&self, node: &str, port: &str) -> Vec<&'f Edge> {
        self.out
            .get(node)
            .map(|edges| {
                edges
                    .iter()
                    .copied()
                    .filter(|e| e.from_port == port)
                    .collect()
            })
            .unwrap_or_default()
    }
}

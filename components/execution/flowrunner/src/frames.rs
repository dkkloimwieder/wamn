//! Trusted, single-shot in-memory call frames over one verified resolution snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use boon::{Compiler, Draft, Schemas};
use serde_json::Value;
use wamn_catalog::{CallFlowInstruction, ExecutionEffectPolicy, ExecutionNodeId, ExecutionPlanV2};
use wamn_flow::node_contract::{ErrorDetail, NodeError};
use wamn_flow::{
    Edge, Flow, FlowConnectionRequirement, Node, ResolvedInterfaces, RespondConfig, SCHEMA_VERSION,
};
use wamn_runner::{Dispatch, ExecutionFailureKind, ExecutionStatus, NodeOutcome, Plan, Step};

use super::{VerifiedResolutionPlan, VerifiedResolutionSnapshot};

const CALL_INPUT_SCHEMA_URI: &str = "mem://call-flow-input-schema.json";
const CALL_INPUT_INVALID: &str = "call-input-invalid";

/// A stable category for a frame interpreter refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameExecutionErrorKind {
    /// The claimed root stack has already started its sole execution.
    RootAlreadyExecuted,
    /// A verified plan cannot be projected into the graph reducer.
    InvalidVerifiedPlan,
    /// A call-flow target is absent from the immutable run snapshot.
    MissingCallee,
    /// The root-local monotonic frame identity space was exhausted.
    FrameIdentityExhausted,
    /// An ordinary effectful node reached the pre-activation interpreter.
    EffectActivationUnavailable,
    /// The graph reducer refused an interpreter-owned transition.
    InvalidTransition,
    /// A callable plan terminated without its required response boundary.
    CalleeDidNotRespond,
}

/// Context-rich refusal from the trusted frame interpreter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameExecutionError {
    kind: FrameExecutionErrorKind,
    frame_id: u64,
    node_id: Option<String>,
    message: String,
}

impl FrameExecutionError {
    /// Stable refusal category.
    pub fn kind(&self) -> FrameExecutionErrorKind {
        self.kind
    }

    /// Active trusted frame at the refusal boundary.
    pub fn frame_id(&self) -> u64 {
        self.frame_id
    }

    /// Active local node, when the refusal belongs to one node.
    pub fn node_id(&self) -> Option<&str> {
        self.node_id.as_deref()
    }

    fn new(
        kind: FrameExecutionErrorKind,
        frame_id: u64,
        node_id: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            frame_id,
            node_id: node_id.map(str::to_string),
            message: message.into(),
        }
    }
}

impl fmt::Display for FrameExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "frame {}: {}", self.frame_id, self.message)?;
        if let Some(node_id) = &self.node_id {
            write!(formatter, " (node {node_id:?})")?;
        }
        Ok(())
    }
}

impl std::error::Error for FrameExecutionError {}

/// One graph failure retained at a root frame boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameFailure {
    node_id: String,
    kind: ExecutionFailureKind,
    detail: ErrorDetail,
}

impl FrameFailure {
    /// Local node whose unhandled failure ended this frame.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Reducer classification of the unhandled failure.
    pub fn kind(&self) -> ExecutionFailureKind {
        self.kind
    }

    /// Exact error detail emitted by the failed graph.
    pub fn detail(&self) -> &ErrorDetail {
        &self.detail
    }
}

/// Terminal result of executing one root frame.
#[derive(Debug, Clone, PartialEq)]
pub enum FrameCompletion {
    /// A request graph reached its response boundary.
    Responded { body: Value, status: u16 },
    /// An event graph exhausted its frontier successfully.
    FrontierExhausted { body: Value },
    /// The graph ended with an unhandled node failure.
    Failed(FrameFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameRecord {
    frame_id: u64,
    parent_frame_id: Option<u64>,
    call_site_id: Option<ExecutionNodeId>,
    flow_id: String,
    current_plan_hash: String,
}

/// Immutable trusted facts for the current frame or its immediate parent.
#[derive(Debug, Clone, Copy)]
pub struct TrustedFrameFacts<'snapshot> {
    run_id: &'snapshot str,
    root_plan_hash: &'snapshot str,
    record: &'snapshot FrameRecord,
    plan: &'snapshot VerifiedResolutionPlan,
}

impl<'snapshot> TrustedFrameFacts<'snapshot> {
    /// Root run under which every in-memory frame executes.
    pub fn run_id(self) -> &'snapshot str {
        self.run_id
    }

    /// Exact root plan hash anchoring every frame in this run.
    pub fn root_plan_hash(self) -> &'snapshot str {
        self.root_plan_hash
    }

    /// Root-run-local monotonic frame identity. The root frame is zero.
    pub fn frame_id(self) -> u64 {
        self.record.frame_id
    }

    /// Immediate parent identity, absent only for the root frame.
    pub fn parent_frame_id(self) -> Option<u64> {
        self.record.parent_frame_id
    }

    /// Call-flow site in the immediate parent, absent only for the root frame.
    pub fn call_site_id(self) -> Option<&'snapshot ExecutionNodeId> {
        self.record.call_site_id.as_ref()
    }

    /// Immutable flow name selected from the run resolution snapshot.
    pub fn flow_id(self) -> &'snapshot str {
        &self.record.flow_id
    }

    /// Exact execution bundle hash of this frame's plan.
    pub fn current_plan_hash(self) -> &'snapshot str {
        &self.record.current_plan_hash
    }

    /// Exact source artifact hash carried by this frame's verified plan.
    pub fn source_artifact_hash(self) -> &'snapshot str {
        self.plan.source_artifact_hash()
    }

    /// Exact hash-verified execution plan for this frame.
    pub fn plan(self) -> &'snapshot ExecutionPlanV2 {
        self.plan.plan()
    }
}

/// Trusted current-frame facts plus the exact immediate parent, if any.
#[derive(Debug, Clone, Copy)]
pub struct TrustedFrame<'snapshot> {
    current: TrustedFrameFacts<'snapshot>,
    parent: Option<TrustedFrameFacts<'snapshot>>,
}

impl<'snapshot> TrustedFrame<'snapshot> {
    /// Exact current-frame facts.
    pub fn current(self) -> TrustedFrameFacts<'snapshot> {
        self.current
    }

    /// Exact immediate-parent facts. No reconstructed ancestry is exposed.
    pub fn parent(self) -> Option<TrustedFrameFacts<'snapshot>> {
        self.parent
    }
}

/// The only ordinary-node seam exposed before effect activation lands.
///
/// The frame interpreter invokes this trait only for nodes whose compiled
/// policy is [`ExecutionEffectPolicy::Pure`]. Entry, response, fail, and
/// call-flow boundaries stay interpreter-owned; an ordinary effectful node is
/// refused before this method can run.
pub trait PureNodeDispatcher {
    fn dispatch(&mut self, frame: TrustedFrame<'_>, node: &Dispatch) -> NodeOutcome;
}

/// Authoritative in-memory frame stack for one claimed root run.
///
/// The caller supplies only the verified immutable snapshot and root run id.
/// Frame identities and ancestry are minted here, never accepted from a guest
/// payload or rebuilt from durable history.
pub struct FrameStack<'snapshot> {
    run_id: String,
    snapshot: &'snapshot VerifiedResolutionSnapshot,
    frames: Vec<FrameRecord>,
    next_frame_id: u64,
    started: bool,
}

impl<'snapshot> FrameStack<'snapshot> {
    /// Start one root-local stack at frame zero.
    pub fn new(run_id: impl Into<String>, snapshot: &'snapshot VerifiedResolutionSnapshot) -> Self {
        let root = snapshot.root();
        Self {
            run_id: run_id.into(),
            snapshot,
            frames: vec![FrameRecord {
                frame_id: 0,
                parent_frame_id: None,
                call_site_id: None,
                flow_id: root.flow_id().to_string(),
                current_plan_hash: root.execution_bundle_hash().to_string(),
            }],
            next_frame_id: 1,
            started: false,
        }
    }

    /// Execute the root graph using only the injected pure-node dispatcher.
    pub fn execute_root(
        &mut self,
        payload: Value,
        dispatcher: &mut impl PureNodeDispatcher,
    ) -> Result<FrameCompletion, FrameExecutionError> {
        if self.started {
            return Err(FrameExecutionError::new(
                FrameExecutionErrorKind::RootAlreadyExecuted,
                0,
                None,
                "claimed root frame execution has already started",
            ));
        }
        // Set before projection, dispatch, or a callee lookup: even a refused
        // first attempt cannot replay the same claimed root through this stack.
        self.started = true;
        self.execute_current(payload, dispatcher)
    }

    /// Current stack depth, including the root frame.
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    fn execute_current(
        &mut self,
        payload: Value,
        dispatcher: &mut impl PureNodeDispatcher,
    ) -> Result<FrameCompletion, FrameExecutionError> {
        let frame_id = self.current_record().frame_id;
        let flow_id = self.current_record().flow_id.clone();
        let verified = self.snapshot.plan(&flow_id).ok_or_else(|| {
            FrameExecutionError::new(
                FrameExecutionErrorKind::MissingCallee,
                frame_id,
                None,
                format!("verified resolution snapshot has no plan for {flow_id:?}"),
            )
        })?;
        let (flow, interfaces) = reducer_inputs(verified).map_err(|message| {
            FrameExecutionError::new(
                FrameExecutionErrorKind::InvalidVerifiedPlan,
                frame_id,
                None,
                message,
            )
        })?;
        let mut plan = Plan::compile(&flow, &interfaces).map_err(|error| {
            FrameExecutionError::new(
                FrameExecutionErrorKind::InvalidVerifiedPlan,
                frame_id,
                None,
                error.to_string(),
            )
        })?;
        // .3.8 owns the root-global depth and dispatch budgets. Do not substitute
        // the reducer's legacy per-plan default for that later contract.
        plan.set_dispatch_budget(u64::MAX);

        let mut state = plan.start(&self.run_id, payload);
        let mut now_ms = 0;
        let mut response_status = None;
        loop {
            match plan.next(&mut state, now_ms) {
                Step::Done(ExecutionStatus::Completed) => {
                    return Ok(match response_status {
                        Some(status) => FrameCompletion::Responded {
                            body: state.result().clone(),
                            status,
                        },
                        None => FrameCompletion::FrontierExhausted {
                            body: state.result().clone(),
                        },
                    });
                }
                Step::Done(ExecutionStatus::Failed) => {
                    let failure = state
                        .failure()
                        .expect("failed reducer state retains its failure");
                    return Ok(FrameCompletion::Failed(FrameFailure {
                        node_id: failure.node.clone(),
                        kind: failure.kind,
                        detail: failure.detail.clone(),
                    }));
                }
                Step::Done(ExecutionStatus::Running) => {
                    return Err(FrameExecutionError::new(
                        FrameExecutionErrorKind::InvalidTransition,
                        frame_id,
                        None,
                        "graph reducer returned a nonterminal done state",
                    ));
                }
                Step::Wait { until_ms, .. } => now_ms = until_ms,
                Step::Reserved(step) => {
                    plan.apply_reserved(&mut state, &step).map_err(|error| {
                        FrameExecutionError::new(
                            FrameExecutionErrorKind::InvalidTransition,
                            frame_id,
                            Some(step.node()),
                            error.to_string(),
                        )
                    })?;
                }
                Step::Dispatch(node) => {
                    let outcome = self.dispatch_node(&node, &mut response_status, dispatcher)?;
                    plan.apply(&mut state, &node, outcome, now_ms)
                        .map_err(|error| {
                            FrameExecutionError::new(
                                FrameExecutionErrorKind::InvalidTransition,
                                frame_id,
                                Some(&node.node),
                                error.to_string(),
                            )
                        })?;
                }
            }
        }
    }

    fn dispatch_node(
        &mut self,
        dispatch: &Dispatch,
        response_status: &mut Option<u16>,
        dispatcher: &mut impl PureNodeDispatcher,
    ) -> Result<NodeOutcome, FrameExecutionError> {
        match dispatch.node_type.as_str() {
            "request" | "event" => Ok(NodeOutcome::ok(dispatch.payload.clone())),
            "respond" => {
                let config = serde_json::from_value::<RespondConfig>(dispatch.config.clone())
                    .map_err(|error| self.invalid_plan_error(&dispatch.node, error.to_string()))?;
                *response_status = Some(config.status);
                Ok(NodeOutcome::ok(dispatch.payload.clone()))
            }
            "call-flow" => self.dispatch_call(dispatch, dispatcher),
            _ => {
                let plan_node = self.current_plan_node(&dispatch.node)?;
                if plan_node.effect_policy == ExecutionEffectPolicy::Effectful {
                    return Err(FrameExecutionError::new(
                        FrameExecutionErrorKind::EffectActivationUnavailable,
                        self.current_record().frame_id,
                        Some(&dispatch.node),
                        "effect activation is unavailable before wamn-0h0g.5.4",
                    ));
                }
                Ok(dispatcher.dispatch(self.trusted_current(), dispatch))
            }
        }
    }

    fn dispatch_call(
        &mut self,
        dispatch: &Dispatch,
        dispatcher: &mut impl PureNodeDispatcher,
    ) -> Result<NodeOutcome, FrameExecutionError> {
        // The reducer sees the canonical public-flow projection (`flow-id`
        // only). The trusted call boundary reads the original compiled
        // instruction, including its site identity, from the verified plan.
        let plan_node = self.current_plan_node(&dispatch.node)?;
        let instruction =
            serde_json::from_value::<CallFlowInstruction>(plan_node.config.clone())
                .map_err(|error| self.invalid_plan_error(&dispatch.node, error.to_string()))?;
        if instruction.site.as_str() != dispatch.node {
            return Err(self.invalid_plan_error(
                &dispatch.node,
                "call-flow instruction site does not match its active local node",
            ));
        }
        let callee = self.snapshot.plan(&instruction.flow_id).ok_or_else(|| {
            FrameExecutionError::new(
                FrameExecutionErrorKind::MissingCallee,
                self.current_record().frame_id,
                Some(&dispatch.node),
                format!(
                    "verified resolution snapshot has no call-flow target {:?}",
                    instruction.flow_id
                ),
            )
        })?;

        if let Some(error) = call_input_error(callee.plan(), &dispatch.payload)
            .map_err(|message| self.invalid_plan_error(&dispatch.node, message))?
        {
            return Ok(NodeOutcome::Error(NodeError::InvalidInput(error)));
        }

        let frame_id = self.push_callee(&instruction)?;
        let completion = self.execute_current(dispatch.payload.clone(), dispatcher);
        self.pop_callee(frame_id);
        match completion? {
            FrameCompletion::Responded { body, .. } => Ok(NodeOutcome::ok(body)),
            FrameCompletion::Failed(failure) => {
                Ok(NodeOutcome::Error(callee_failure_outcome(&failure)))
            }
            FrameCompletion::FrontierExhausted { .. } => Err(FrameExecutionError::new(
                FrameExecutionErrorKind::CalleeDidNotRespond,
                self.current_record().frame_id,
                Some(&dispatch.node),
                format!(
                    "call-flow target {:?} exhausted without responding",
                    instruction.flow_id
                ),
            )),
        }
    }

    fn push_callee(
        &mut self,
        instruction: &CallFlowInstruction,
    ) -> Result<u64, FrameExecutionError> {
        let parent_frame_id = self.current_record().frame_id;
        let callee = self
            .snapshot
            .plan(&instruction.flow_id)
            .expect("callee membership checked before push");
        let frame_id = self.next_frame_id;
        self.next_frame_id = self.next_frame_id.checked_add(1).ok_or_else(|| {
            FrameExecutionError::new(
                FrameExecutionErrorKind::FrameIdentityExhausted,
                parent_frame_id,
                Some(instruction.site.as_str()),
                "root-local frame identity space is exhausted",
            )
        })?;
        self.frames.push(FrameRecord {
            frame_id,
            parent_frame_id: Some(parent_frame_id),
            call_site_id: Some(instruction.site.clone()),
            flow_id: callee.flow_id().to_string(),
            current_plan_hash: callee.execution_bundle_hash().to_string(),
        });
        Ok(frame_id)
    }

    fn pop_callee(&mut self, expected_frame_id: u64) {
        let popped = self
            .frames
            .pop()
            .expect("callee execution retains the pushed frame");
        assert_eq!(
            popped.frame_id, expected_frame_id,
            "frames pop in LIFO order"
        );
        assert!(!self.frames.is_empty(), "the root frame is never popped");
    }

    fn current_plan_node(
        &self,
        node_id: &str,
    ) -> Result<&wamn_catalog::ExecutionPlanNode, FrameExecutionError> {
        self.trusted_current()
            .current()
            .plan()
            .body
            .nodes
            .iter()
            .find(|node| node.local_node_id.as_str() == node_id)
            .ok_or_else(|| {
                self.invalid_plan_error(node_id, "active node is absent from its verified plan")
            })
    }

    fn trusted_current(&self) -> TrustedFrame<'_> {
        let current = self.current_record();
        let current = self.trusted_facts(current);
        let parent = self
            .frames
            .get(self.frames.len().saturating_sub(2))
            .filter(|_| self.frames.len() > 1)
            .map(|record| self.trusted_facts(record));
        TrustedFrame { current, parent }
    }

    fn trusted_facts<'a>(&'a self, record: &'a FrameRecord) -> TrustedFrameFacts<'a> {
        let plan = self
            .snapshot
            .plan(&record.flow_id)
            .expect("every minted frame names a verified snapshot plan");
        debug_assert_eq!(record.current_plan_hash, plan.execution_bundle_hash());
        TrustedFrameFacts {
            run_id: &self.run_id,
            root_plan_hash: self.snapshot.root_execution_bundle_hash(),
            record,
            plan,
        }
    }

    fn current_record(&self) -> &FrameRecord {
        self.frames
            .last()
            .expect("a frame stack always retains its root")
    }

    fn invalid_plan_error(&self, node_id: &str, message: impl Into<String>) -> FrameExecutionError {
        FrameExecutionError::new(
            FrameExecutionErrorKind::InvalidVerifiedPlan,
            self.current_record().frame_id,
            Some(node_id),
            message,
        )
    }
}

fn call_input_error(
    plan: &ExecutionPlanV2,
    payload: &Value,
) -> Result<Option<ErrorDetail>, String> {
    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    compiler
        .add_resource(
            CALL_INPUT_SCHEMA_URI,
            plan.body.entry_input_schema_guard.clone(),
        )
        .map_err(|error| format!("verified call input guard cannot be loaded: {error}"))?;
    let mut schemas = Schemas::new();
    let schema = compiler
        .compile(CALL_INPUT_SCHEMA_URI, &mut schemas)
        .map_err(|error| format!("verified call input guard cannot be compiled: {error}"))?;
    Ok(schemas
        .validate(payload, schema)
        .err()
        .map(|_| ErrorDetail {
            message: "call-flow payload does not satisfy the callee input schema".to_string(),
            code: Some(CALL_INPUT_INVALID.to_string()),
            data: None,
        }))
}

fn callee_failure_outcome(failure: &FrameFailure) -> NodeError {
    let code = failure
        .detail
        .code
        .clone()
        .unwrap_or_else(|| "callee-failed".to_string());
    let message = if failure.detail.message.is_empty() {
        code.clone()
    } else {
        failure.detail.message.clone()
    };
    NodeError::Terminal(ErrorDetail {
        message,
        code: Some(code),
        data: Some(failure.detail.to_error_payload()),
    })
}

fn reducer_inputs(verified: &VerifiedResolutionPlan) -> Result<(Flow, ResolvedInterfaces), String> {
    let execution = verified.plan();
    let mut requirements = BTreeMap::new();
    let mut nodes = Vec::with_capacity(execution.body.nodes.len());
    for node in &execution.body.nodes {
        let config = if node.node_type == "call-flow" {
            let instruction = serde_json::from_value::<CallFlowInstruction>(node.config.clone())
                .map_err(|error| format!("verified call-flow config is invalid: {error}"))?;
            serde_json::json!({"flow-id": instruction.flow_id})
        } else {
            node.config.clone()
        };
        let connection = node
            .source_connection_requirement
            .as_ref()
            .map(|requirement| {
                requirements.insert(requirement.name.clone(), requirement.descriptor.clone());
                requirement.name.clone()
            });
        nodes.push(Node {
            id: node.local_node_id.to_string(),
            node_type: node.node_type.clone(),
            label: None,
            config,
            connection,
        });
    }
    let edges = execution
        .body
        .edges
        .iter()
        .map(|edge| Edge {
            from: edge.source.to_string(),
            from_port: edge.source_port.clone(),
            to: edge.destination.to_string(),
            to_port: edge.destination_port.clone(),
            ordinal: Some(edge.fan_out_ordinal),
        })
        .collect();
    let connection_requirements = requirements
        .into_iter()
        .map(|(name, requirement)| FlowConnectionRequirement { name, requirement })
        .collect();
    let flow = Flow {
        schema_version: SCHEMA_VERSION.to_string(),
        flow_id: verified.flow_id().to_string(),
        version: 1,
        name: None,
        nodes,
        edges,
        connection_requirements,
    };
    Ok((flow, inferred_interfaces(execution)))
}

fn inferred_interfaces(plan: &ExecutionPlanV2) -> ResolvedInterfaces {
    let mut interfaces: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for node in &plan.body.nodes {
        if !matches!(
            node.node_type.as_str(),
            "request" | "event" | "respond" | "fail" | "call-flow"
        ) {
            interfaces.entry(node.node_type.clone()).or_default();
        }
    }
    for edge in &plan.body.edges {
        if edge.source_port != wamn_flow::ERROR_PORT
            && let Some(node) = plan
                .body
                .nodes
                .iter()
                .find(|node| node.local_node_id == edge.source)
            && let Some(ports) = interfaces.get_mut(&node.node_type)
        {
            ports.insert(edge.source_port.clone());
        }
    }
    interfaces
        .into_iter()
        .map(|(node_type, ports)| (node_type, ports.into_iter().collect()))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_catalog::{
        CallableContract, CallableEffectCeiling, CallableReturnContract, ExecutionPlanBody,
        ExecutionPlanEdge, ExecutionPlanNode, ExecutionRuntimeRevision, ExecutionSourceMapEntry,
        HOST_EFFECT_CONTRACT_VERSION, RootTerminalBehavior, entry_input_schema_hash,
        execution_bundle_hash,
    };

    use super::*;

    const RESPOND: &str = "respond";

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SeenFrame {
        node_id: String,
        run_id: String,
        root_plan_hash: String,
        frame_id: u64,
        parent_frame_id: Option<u64>,
        call_site_id: Option<String>,
        flow_id: String,
        plan_hash: String,
        source_hash: String,
        parent_plan_hash: Option<String>,
    }

    #[derive(Default)]
    struct RecordingDispatcher {
        seen: Vec<SeenFrame>,
    }

    impl PureNodeDispatcher for RecordingDispatcher {
        fn dispatch(&mut self, frame: TrustedFrame<'_>, node: &Dispatch) -> NodeOutcome {
            let current = frame.current();
            self.seen.push(SeenFrame {
                node_id: node.node.clone(),
                run_id: current.run_id().to_string(),
                root_plan_hash: current.root_plan_hash().to_string(),
                frame_id: current.frame_id(),
                parent_frame_id: current.parent_frame_id(),
                call_site_id: current.call_site_id().map(ToString::to_string),
                flow_id: current.flow_id().to_string(),
                plan_hash: current.current_plan_hash().to_string(),
                source_hash: current.source_artifact_hash().to_string(),
                parent_plan_hash: frame
                    .parent()
                    .map(|parent| parent.current_plan_hash().to_string()),
            });

            match node.node.as_str() {
                "stamp" => {
                    let mut body = node.payload.clone();
                    let calls = body
                        .get("calls")
                        .and_then(Value::as_u64)
                        .unwrap_or_default()
                        + 1;
                    body.as_object_mut()
                        .expect("stamp tests pass an object")
                        .insert("calls".to_string(), json!(calls));
                    NodeOutcome::ok(body)
                }
                "observe" | "recover" => NodeOutcome::ok(node.payload.clone()),
                "explode" => NodeOutcome::Error(NodeError::Terminal(ErrorDetail::coded(
                    "callee-broke",
                    "callee exploded",
                ))),
                "branch" => {
                    let remaining = node
                        .payload
                        .as_u64()
                        .expect("recursion input is an integer");
                    if remaining == 0 {
                        NodeOutcome::ok_on(json!("done"), "done")
                    } else {
                        NodeOutcome::ok_on(json!(remaining - 1), "recurse")
                    }
                }
                unexpected => panic!("unexpected pure-node dispatch: {unexpected}"),
            }
        }
    }

    fn hash(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn node_id(value: &str) -> ExecutionNodeId {
        ExecutionNodeId::new(value).unwrap()
    }

    fn node(
        id: &str,
        node_type: &str,
        config: Value,
        effect_policy: ExecutionEffectPolicy,
    ) -> ExecutionPlanNode {
        ExecutionPlanNode {
            local_node_id: node_id(id),
            source_node_id: id.to_string(),
            node_type: node_type.to_string(),
            config,
            effect_policy,
            source_connection_requirement: None,
        }
    }

    fn call_node(id: &str, flow_id: &str) -> ExecutionPlanNode {
        node(
            id,
            "call-flow",
            serde_json::to_value(CallFlowInstruction {
                site: node_id(id),
                flow_id: flow_id.to_string(),
            })
            .unwrap(),
            ExecutionEffectPolicy::Effectful,
        )
    }

    fn edge(source: &str, source_port: &str, destination: &str) -> ExecutionPlanEdge {
        ExecutionPlanEdge {
            source: node_id(source),
            source_port: source_port.to_string(),
            destination: node_id(destination),
            destination_port: None,
            fan_out_ordinal: 0,
        }
    }

    fn request_plan(
        source_artifact_hash: &str,
        guard: Value,
        mut nodes: Vec<ExecutionPlanNode>,
        edges: Vec<ExecutionPlanEdge>,
        response_status: u16,
    ) -> ExecutionPlanV2 {
        nodes.insert(
            0,
            node(
                "request",
                "request",
                json!({"input-schema": guard.clone()}),
                ExecutionEffectPolicy::Pure,
            ),
        );
        nodes.push(node(
            RESPOND,
            "respond",
            json!({"status": response_status}),
            ExecutionEffectPolicy::Pure,
        ));
        let source_map = nodes
            .iter()
            .map(|node| ExecutionSourceMapEntry {
                local_node_id: node.local_node_id.clone(),
                source_node_id: node.source_node_id.clone(),
            })
            .collect();
        ExecutionPlanV2::new(
            ExecutionRuntimeRevision {
                flowrunner_component_digest: hash('a'),
                effect_provider_revision: hash('b'),
                host_effect_contract_version: HOST_EFFECT_CONTRACT_VERSION.to_string(),
            },
            source_artifact_hash,
            ExecutionPlanBody {
                entry_instruction: node_id("request"),
                nodes,
                edges,
                root_terminal_behavior: RootTerminalBehavior::Respond {
                    responders: vec![node_id(RESPOND)],
                },
                entry_input_schema_guard: guard.clone(),
                callable_contract: Some(CallableContract {
                    version: wamn_catalog::CALLABLE_CONTRACT_VERSION.to_string(),
                    input_schema_hash: entry_input_schema_hash(&guard),
                    return_contract: CallableReturnContract::UntypedJsonBody,
                    effect_ceiling: CallableEffectCeiling::Effectful,
                }),
                source_map,
            },
        )
        .unwrap()
    }

    fn snapshot(
        root_flow_id: &str,
        plans: Vec<(&str, ExecutionPlanV2)>,
    ) -> VerifiedResolutionSnapshot {
        let plans = plans
            .into_iter()
            .map(|(flow_id, plan)| {
                let exact_bytes = serde_json::to_vec(&plan).unwrap();
                let verified = VerifiedResolutionPlan {
                    flow_id: flow_id.to_string(),
                    execution_bundle_hash: execution_bundle_hash(&exact_bytes),
                    source_artifact_hash: plan.header.root_artifact_hash.clone(),
                    plan,
                };
                (flow_id.to_string(), verified)
            })
            .collect::<BTreeMap<_, _>>();
        let root_execution_bundle_hash = plans
            .get(root_flow_id)
            .expect("test root is present")
            .execution_bundle_hash()
            .to_string();
        VerifiedResolutionSnapshot {
            root_flow_id: root_flow_id.to_string(),
            root_execution_bundle_hash,
            plans,
        }
    }

    #[test]
    fn sibling_calls_push_execute_pop_with_monotonic_trusted_frame_facts() {
        let callee = request_plan(
            &hash('d'),
            json!({"type": "object"}),
            vec![node(
                "stamp",
                "test-pure",
                json!({}),
                ExecutionEffectPolicy::Pure,
            )],
            vec![
                edge("request", "main", "stamp"),
                edge("stamp", "main", RESPOND),
            ],
            299,
        );
        let root = request_plan(
            &hash('c'),
            json!({"type": "object"}),
            vec![
                call_node("call-one", "callee"),
                call_node("call-two", "callee"),
                node(
                    "observe",
                    "test-pure",
                    json!({}),
                    ExecutionEffectPolicy::Pure,
                ),
            ],
            vec![
                edge("request", "main", "call-one"),
                edge("call-one", "main", "call-two"),
                edge("call-two", "main", "observe"),
                edge("observe", "main", RESPOND),
            ],
            201,
        );
        let snapshot = snapshot("root", vec![("root", root), ("callee", callee)]);
        let root_hash = snapshot.root_execution_bundle_hash().to_string();
        let callee_hash = snapshot
            .plan("callee")
            .unwrap()
            .execution_bundle_hash()
            .to_string();
        let mut stack = FrameStack::new("run-7", &snapshot);
        let mut dispatcher = RecordingDispatcher::default();

        let completion = stack
            .execute_root(json!({"calls": 0}), &mut dispatcher)
            .unwrap();

        assert_eq!(
            completion,
            FrameCompletion::Responded {
                body: json!({"calls": 2}),
                status: 201,
            },
            "nested status hints are discarded and only each body returns"
        );
        assert_eq!(stack.depth(), 1, "both siblings popped to the root");
        assert_eq!(
            dispatcher
                .seen
                .iter()
                .map(|seen| seen.node_id.as_str())
                .collect::<Vec<_>>(),
            ["stamp", "stamp", "observe"],
            "call-flow itself never reaches the ordinary-node dispatcher"
        );
        assert_eq!(dispatcher.seen[0].frame_id, 1);
        assert_eq!(dispatcher.seen[0].parent_frame_id, Some(0));
        assert_eq!(dispatcher.seen[0].call_site_id.as_deref(), Some("call-one"));
        assert_eq!(dispatcher.seen[0].flow_id, "callee");
        assert_eq!(dispatcher.seen[0].root_plan_hash, root_hash);
        assert_eq!(dispatcher.seen[0].plan_hash, callee_hash);
        assert_eq!(dispatcher.seen[0].source_hash, hash('d'));
        assert_eq!(dispatcher.seen[0].parent_plan_hash, Some(root_hash.clone()));
        assert_eq!(dispatcher.seen[1].frame_id, 2);
        assert_eq!(dispatcher.seen[1].parent_frame_id, Some(0));
        assert_eq!(dispatcher.seen[1].call_site_id.as_deref(), Some("call-two"));
        assert_eq!(dispatcher.seen[1].root_plan_hash, root_hash);
        assert_eq!(dispatcher.seen[2].frame_id, 0);
        assert_eq!(dispatcher.seen[2].parent_frame_id, None);
        assert_eq!(dispatcher.seen[2].call_site_id, None);
        assert_eq!(dispatcher.seen[2].root_plan_hash, root_hash);
        assert_eq!(dispatcher.seen[2].plan_hash, root_hash);
        assert_eq!(dispatcher.seen[2].source_hash, hash('c'));
        assert!(dispatcher.seen.iter().all(|seen| seen.run_id == "run-7"));
    }

    #[test]
    fn call_enter_invalid_input_stays_at_caller_and_follows_error_port() {
        let callee = request_plan(
            &hash('d'),
            json!({
                "type": "object",
                "required": ["name"],
                "properties": {"name": {"type": "string"}}
            }),
            Vec::new(),
            vec![edge("request", "main", RESPOND)],
            200,
        );
        let root = request_plan(
            &hash('c'),
            json!(true),
            vec![
                call_node("call", "callee"),
                node(
                    "recover",
                    "test-pure",
                    json!({}),
                    ExecutionEffectPolicy::Pure,
                ),
            ],
            vec![
                edge("request", "main", "call"),
                edge("call", "main", RESPOND),
                edge("call", wamn_flow::ERROR_PORT, "recover"),
                edge("recover", "main", RESPOND),
            ],
            200,
        );
        let snapshot = snapshot("root", vec![("root", root), ("callee", callee)]);
        let mut stack = FrameStack::new("run-invalid", &snapshot);
        let mut dispatcher = RecordingDispatcher::default();

        let FrameCompletion::Responded { body, .. } = stack
            .execute_root(json!({"name": 7}), &mut dispatcher)
            .unwrap()
        else {
            panic!("caller error path must respond");
        };

        assert_eq!(body["error"]["code"], CALL_INPUT_INVALID);
        assert_eq!(dispatcher.seen.len(), 1);
        assert_eq!(dispatcher.seen[0].node_id, "recover");
        assert_eq!(dispatcher.seen[0].frame_id, 0);
        assert_eq!(stack.next_frame_id, 1, "invalid input never pushes a frame");
        assert_eq!(stack.depth(), 1);
    }

    #[test]
    fn unhandled_callee_failure_becomes_call_site_error_and_restores_stack() {
        let callee = request_plan(
            &hash('d'),
            json!(true),
            vec![node(
                "explode",
                "test-pure",
                json!({}),
                ExecutionEffectPolicy::Pure,
            )],
            vec![
                edge("request", "main", "explode"),
                edge("explode", "main", RESPOND),
            ],
            204,
        );
        let root = request_plan(
            &hash('c'),
            json!(true),
            vec![
                call_node("call", "callee"),
                node(
                    "recover",
                    "test-pure",
                    json!({}),
                    ExecutionEffectPolicy::Pure,
                ),
            ],
            vec![
                edge("request", "main", "call"),
                edge("call", "main", RESPOND),
                edge("call", wamn_flow::ERROR_PORT, "recover"),
                edge("recover", "main", RESPOND),
            ],
            200,
        );
        let snapshot = snapshot("root", vec![("root", root), ("callee", callee)]);
        let mut stack = FrameStack::new("run-failure", &snapshot);
        let mut dispatcher = RecordingDispatcher::default();

        let FrameCompletion::Responded { body, .. } = stack
            .execute_root(json!({"value": 1}), &mut dispatcher)
            .unwrap()
        else {
            panic!("caller error path must respond");
        };

        assert_eq!(body["error"]["code"], "callee-broke");
        assert_eq!(body["error"]["data"]["error"]["message"], "callee exploded");
        assert_eq!(
            dispatcher
                .seen
                .iter()
                .map(|seen| (seen.node_id.as_str(), seen.frame_id))
                .collect::<Vec<_>>(),
            [("explode", 1), ("recover", 0)]
        );
        assert_eq!(stack.depth(), 1);
    }

    fn recursive_plan(source_hash: &str, target: &str) -> ExecutionPlanV2 {
        request_plan(
            source_hash,
            json!({"type": "integer", "minimum": 0}),
            vec![
                node(
                    "branch",
                    "test-branch",
                    json!({}),
                    ExecutionEffectPolicy::Pure,
                ),
                call_node("call", target),
            ],
            vec![
                edge("request", "main", "branch"),
                edge("branch", "recurse", "call"),
                edge("branch", "done", RESPOND),
                edge("call", "main", RESPOND),
            ],
            200,
        )
    }

    #[test]
    fn finite_self_and_mutual_recursion_use_ordinary_monotonic_frames() {
        let self_plan = recursive_plan(&hash('c'), "self-flow");
        let self_snapshot = snapshot("self-flow", vec![("self-flow", self_plan)]);
        let mut self_stack = FrameStack::new("run-self", &self_snapshot);
        let mut self_dispatcher = RecordingDispatcher::default();
        assert_eq!(
            self_stack
                .execute_root(json!(2), &mut self_dispatcher)
                .unwrap(),
            FrameCompletion::Responded {
                body: json!("done"),
                status: 200,
            }
        );
        assert_eq!(
            self_dispatcher
                .seen
                .iter()
                .map(|seen| (seen.frame_id, seen.parent_frame_id, seen.flow_id.as_str()))
                .collect::<Vec<_>>(),
            [
                (0, None, "self-flow"),
                (1, Some(0), "self-flow"),
                (2, Some(1), "self-flow"),
            ]
        );
        assert_eq!(self_stack.depth(), 1);

        let alpha = recursive_plan(&hash('d'), "beta");
        let beta = recursive_plan(&hash('e'), "alpha");
        let mutual_snapshot = snapshot("alpha", vec![("alpha", alpha), ("beta", beta)]);
        let mut mutual_stack = FrameStack::new("run-mutual", &mutual_snapshot);
        let mut mutual_dispatcher = RecordingDispatcher::default();
        assert!(matches!(
            mutual_stack
                .execute_root(json!(3), &mut mutual_dispatcher)
                .unwrap(),
            FrameCompletion::Responded { body, .. } if body == json!("done")
        ));
        assert_eq!(
            mutual_dispatcher
                .seen
                .iter()
                .map(|seen| (seen.frame_id, seen.parent_frame_id, seen.flow_id.as_str()))
                .collect::<Vec<_>>(),
            [
                (0, None, "alpha"),
                (1, Some(0), "beta"),
                (2, Some(1), "alpha"),
                (3, Some(2), "beta"),
            ]
        );
        assert_eq!(mutual_stack.depth(), 1);
    }

    #[test]
    fn missing_callee_refuses_without_push_or_ordinary_dispatch() {
        let root = request_plan(
            &hash('c'),
            json!(true),
            vec![call_node("call", "missing")],
            vec![
                edge("request", "main", "call"),
                edge("call", "main", RESPOND),
            ],
            200,
        );
        let snapshot = snapshot("root", vec![("root", root)]);
        let mut stack = FrameStack::new("run-missing", &snapshot);
        let mut dispatcher = RecordingDispatcher::default();

        let error = stack.execute_root(json!({}), &mut dispatcher).unwrap_err();

        assert_eq!(error.kind(), FrameExecutionErrorKind::MissingCallee);
        assert_eq!(error.frame_id(), 0);
        assert_eq!(error.node_id(), Some("call"));
        assert!(dispatcher.seen.is_empty());
        assert_eq!(stack.depth(), 1);
    }

    #[test]
    fn forged_call_site_is_refused_from_original_compiled_instruction() {
        let callee = request_plan(
            &hash('d'),
            json!(true),
            Vec::new(),
            vec![edge("request", "main", RESPOND)],
            200,
        );
        let mut root = request_plan(
            &hash('c'),
            json!(true),
            vec![call_node("call", "callee")],
            vec![
                edge("request", "main", "call"),
                edge("call", "main", RESPOND),
            ],
            200,
        );
        root.body
            .nodes
            .iter_mut()
            .find(|node| node.local_node_id.as_str() == "call")
            .unwrap()
            .config["site"] = json!("forged-site");
        let snapshot = snapshot("root", vec![("root", root), ("callee", callee)]);
        let mut stack = FrameStack::new("run-forged", &snapshot);
        let mut dispatcher = RecordingDispatcher::default();

        let error = stack.execute_root(json!({}), &mut dispatcher).unwrap_err();

        assert_eq!(error.kind(), FrameExecutionErrorKind::InvalidVerifiedPlan);
        assert_eq!(error.node_id(), Some("call"));
        assert!(dispatcher.seen.is_empty());
        assert_eq!(stack.depth(), 1);
    }

    #[test]
    fn nested_effectful_node_refuses_before_activation_and_pops_callee() {
        let callee = request_plan(
            &hash('d'),
            json!(true),
            vec![node(
                "effect",
                "test-effect",
                json!({}),
                ExecutionEffectPolicy::Effectful,
            )],
            vec![
                edge("request", "main", "effect"),
                edge("effect", "main", RESPOND),
            ],
            200,
        );
        let root = request_plan(
            &hash('c'),
            json!(true),
            vec![call_node("call", "callee")],
            vec![
                edge("request", "main", "call"),
                edge("call", "main", RESPOND),
            ],
            200,
        );
        let snapshot = snapshot("root", vec![("root", root), ("callee", callee)]);
        let mut stack = FrameStack::new("run-effect", &snapshot);
        let mut dispatcher = RecordingDispatcher::default();

        let error = stack.execute_root(json!({}), &mut dispatcher).unwrap_err();

        assert_eq!(
            error.kind(),
            FrameExecutionErrorKind::EffectActivationUnavailable
        );
        assert_eq!(error.frame_id(), 1);
        assert_eq!(error.node_id(), Some("effect"));
        assert!(dispatcher.seen.is_empty());
        assert_eq!(
            stack.next_frame_id, 2,
            "the callee was entered exactly once"
        );
        assert_eq!(stack.depth(), 1, "interpreter refusal popped the callee");

        let replay = stack.execute_root(json!({}), &mut dispatcher).unwrap_err();
        assert_eq!(replay.kind(), FrameExecutionErrorKind::RootAlreadyExecuted);
        assert!(dispatcher.seen.is_empty(), "a refused root cannot replay");
    }

    #[test]
    fn completed_root_stack_refuses_repeat_execution_without_dispatch() {
        let root = request_plan(
            &hash('c'),
            json!(true),
            Vec::new(),
            vec![edge("request", "main", RESPOND)],
            200,
        );
        let snapshot = snapshot("root", vec![("root", root)]);
        let mut stack = FrameStack::new("run-once", &snapshot);
        let mut dispatcher = RecordingDispatcher::default();

        assert!(matches!(
            stack.execute_root(json!({}), &mut dispatcher).unwrap(),
            FrameCompletion::Responded { .. }
        ));
        let replay = stack
            .execute_root(json!({"replay": true}), &mut dispatcher)
            .unwrap_err();

        assert_eq!(replay.kind(), FrameExecutionErrorKind::RootAlreadyExecuted);
        assert_eq!(replay.frame_id(), 0);
        assert_eq!(replay.node_id(), None);
        assert!(dispatcher.seen.is_empty());
        assert_eq!(stack.depth(), 1);
    }
}

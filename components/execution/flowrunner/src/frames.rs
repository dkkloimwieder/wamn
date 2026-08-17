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
use wamn_run_state::{CaptureMode, Captured, derive_capture};
use wamn_runner::{
    Dispatch, ExecutionFailureKind, ExecutionStatus, NodeOutcome, Plan, ReservedStep, Step,
};

use super::{VerifiedResolutionPlan, VerifiedResolutionSnapshot};

/// In-memory document key under which an entry guard is handed to the schema
/// compiler. Never persisted, compared, or observable outside this module.
const ENTRY_INPUT_SCHEMA_URI: &str = "mem://entry-input-schema.json";

/// The one classification for a payload refused by a plan's entry input-schema
/// guard, at either admission boundary.
///
/// The spelling predates root-entry enforcement (wamn-0h0g.15.118) and is kept
/// verbatim: the call-flow boundary already emits it, and a second spelling for
/// the same refusal would split one classification in two.
const CALL_INPUT_INVALID: &str = "call-input-invalid";

/// Maximum number of simultaneously active callee frames under one root run.
///
/// The root frame is depth zero and does not consume this allowance. The
/// sixty-fifth callee is refused before its frame identity is allocated.
pub const MAX_CALL_DEPTH: usize = 64;

/// Maximum node dispatches emitted across every frame under one root run.
///
/// Ordinary loop dispatches, reducer re-dispatches after a retryable pure-node
/// outcome, and call-flow instructions consume the same root-owned allowance.
/// The 10,001st dispatch fails the root run; effects are never retried.
pub const DEFAULT_ROOT_DISPATCH_BUDGET: u64 = 10_000;

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
    /// The root-local monotonic fact sequence space was exhausted.
    FactSequenceExhausted,
    /// The trusted fact consumer refused one completed occurrence.
    FactSinkRefused,
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
    source: Option<NodeFactSinkError>,
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
            source: None,
        }
    }

    fn fact_sink_refused(frame_id: u64, node_id: &str, source: NodeFactSinkError) -> Self {
        Self {
            kind: FrameExecutionErrorKind::FactSinkRefused,
            frame_id,
            node_id: Some(node_id.to_string()),
            message: "trusted node-fact sink refused a completed occurrence".to_string(),
            source: Some(source),
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

impl std::error::Error for FrameExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

/// One graph failure retained at a root frame boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameFailure {
    frame_id: u64,
    node_id: String,
    occurrence: u32,
    kind: ExecutionFailureKind,
    detail: ErrorDetail,
}

impl FrameFailure {
    /// Trusted frame in which the terminal failure was observed.
    pub fn frame_id(&self) -> u64 {
        self.frame_id
    }

    /// Local node whose unhandled failure ended this frame.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Visit of the local node attributed to the terminal failure.
    pub fn occurrence(&self) -> u32 {
        self.occurrence
    }

    /// Reducer classification of the unhandled failure.
    pub fn kind(&self) -> ExecutionFailureKind {
        self.kind
    }

    /// Exact error detail emitted by the failed graph.
    pub fn detail(&self) -> &ErrorDetail {
        &self.detail
    }

    fn is_root_budget_exhaustion(&self) -> bool {
        matches!(
            self.kind,
            ExecutionFailureKind::DepthBudget | ExecutionFailureKind::DispatchBudget
        )
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

enum FrameDispatchOutcome {
    Node(NodeOutcome),
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

/// A terminal result attached to one trusted node fact.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeFactOutcome {
    /// The occurrence completed on one exact output port.
    Success { output_port: String },
    /// The occurrence completed with the frozen standard-node error taxonomy.
    Error(NodeError),
}

impl NodeFactOutcome {
    /// Exact output port selected by the terminal outcome.
    pub fn output_port(&self) -> &str {
        match self {
            Self::Success { output_port } => output_port,
            Self::Error(_) => wamn_flow::ERROR_PORT,
        }
    }

    /// Classified node error, when this is an error fact.
    pub fn error(&self) -> Option<&NodeError> {
        match self {
            Self::Success { .. } => None,
            Self::Error(error) => Some(error),
        }
    }
}

/// One completed node occurrence constructed only by the trusted frame stack.
#[derive(Debug, Clone, PartialEq)]
pub struct TrustedNodeFact {
    run_id: String,
    root_plan_hash: String,
    frame_id: u64,
    parent_frame_id: Option<u64>,
    call_site_id: Option<ExecutionNodeId>,
    flow_id: String,
    current_plan_hash: String,
    source_artifact_hash: String,
    local_node_id: ExecutionNodeId,
    source_node_id: String,
    occurrence: u32,
    sequence: u64,
    outcome: NodeFactOutcome,
    capture: Captured,
}

impl TrustedNodeFact {
    /// Root run under which this occurrence completed.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Exact root plan hash anchoring every frame in this run.
    pub fn root_plan_hash(&self) -> &str {
        &self.root_plan_hash
    }

    /// Root-run-local frame identity.
    pub fn frame_id(&self) -> u64 {
        self.frame_id
    }

    /// Immediate parent frame, absent only for the root frame.
    pub fn parent_frame_id(&self) -> Option<u64> {
        self.parent_frame_id
    }

    /// Call-flow site in the immediate parent, absent only for the root frame.
    pub fn call_site_id(&self) -> Option<&ExecutionNodeId> {
        self.call_site_id.as_ref()
    }

    /// Immutable flow selected from the verified run resolution.
    pub fn flow_id(&self) -> &str {
        &self.flow_id
    }

    /// Exact execution bundle hash of the active plan.
    pub fn current_plan_hash(&self) -> &str {
        &self.current_plan_hash
    }

    /// Exact source artifact hash carried by the active verified plan.
    pub fn source_artifact_hash(&self) -> &str {
        &self.source_artifact_hash
    }

    /// Plan-local node identity used by the durable frame key.
    pub fn local_node_id(&self) -> &ExecutionNodeId {
        &self.local_node_id
    }

    /// Original authored node identity derived from the verified source map.
    pub fn source_node_id(&self) -> &str {
        &self.source_node_id
    }

    /// Visit number of this local node in the current frame.
    pub fn occurrence(&self) -> u32 {
        self.occurrence
    }

    /// Zero-based accepted-emission order across the complete root run.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Exact terminal node result.
    pub fn outcome(&self) -> &NodeFactOutcome {
        &self.outcome
    }

    /// Run-state-owned full/off node I/O projection.
    pub fn capture(&self) -> &Captured {
        &self.capture
    }
}

/// Context retained when a trusted node-fact consumer refuses an emission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeFactSinkError {
    message: String,
}

impl NodeFactSinkError {
    /// Describe one fact-consumer refusal without introducing a wire taxonomy.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for NodeFactSinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for NodeFactSinkError {}

/// Fallible consumer of trusted, terminal frame facts.
pub trait NodeFactSink {
    fn emit(&mut self, fact: TrustedNodeFact) -> Result<(), NodeFactSinkError>;
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
/// policy is [`ExecutionEffectPolicy::Pure`]. Entry, response, and call-flow
/// boundaries stay interpreter-owned; fail keeps its existing pure standard-
/// node ABI. An ordinary effectful node is refused before this method can run.
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
    capture_mode: CaptureMode,
    frames: Vec<FrameRecord>,
    next_frame_id: u64,
    next_fact_sequence: u64,
    dispatched_nodes: u64,
    root_dispatch_budget: u64,
    started: bool,
}

impl<'snapshot> FrameStack<'snapshot> {
    /// Start one root-local stack at frame zero.
    pub fn new(
        run_id: impl Into<String>,
        snapshot: &'snapshot VerifiedResolutionSnapshot,
        capture_mode: CaptureMode,
    ) -> Self {
        let root = snapshot.root();
        Self {
            run_id: run_id.into(),
            snapshot,
            capture_mode,
            frames: vec![FrameRecord {
                frame_id: 0,
                parent_frame_id: None,
                call_site_id: None,
                flow_id: root.flow_id().to_string(),
                current_plan_hash: root.execution_bundle_hash().to_string(),
            }],
            next_frame_id: 1,
            next_fact_sequence: 0,
            dispatched_nodes: 0,
            root_dispatch_budget: DEFAULT_ROOT_DISPATCH_BUDGET,
            started: false,
        }
    }

    /// Execute the root graph using only the injected pure-node dispatcher.
    ///
    /// The payload must satisfy the root plan's own entry input-schema guard; a
    /// violation fails the root run before the graph is projected.
    pub fn execute_root(
        &mut self,
        payload: Value,
        dispatcher: &mut impl PureNodeDispatcher,
        fact_sink: &mut impl NodeFactSink,
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
        // Nothing upstream can enforce the root entry guard: the guard document
        // lives only in these plan bytes, which only this process pulls, so the
        // host-side HTTP routing provider serves the unconstraining schema and
        // would otherwise admit any shape (wamn-0h0g.15.118). Enforced before the
        // graph is projected and before any dispatch is debited — an inadmissible
        // payload buys no root work — and refused with the classification the
        // call-flow boundary already uses. An event entry's guard is required to
        // be exactly `true`, so every event payload stays admitted.
        let root_plan = self.snapshot.root().plan();
        let entry_id = &root_plan.body.entry_instruction;
        let entry_refusal = entry_input_error(root_plan, &payload).map_err(|message| {
            FrameExecutionError::new(
                FrameExecutionErrorKind::InvalidVerifiedPlan,
                0,
                Some(entry_id.as_str()),
                message,
            )
        })?;
        if let Some(detail) = entry_refusal {
            return Ok(FrameCompletion::Failed(FrameFailure {
                frame_id: 0,
                node_id: entry_id.to_string(),
                occurrence: 0,
                kind: ExecutionFailureKind::InvalidInput,
                detail,
            }));
        }
        self.execute_current(payload, dispatcher, fact_sink)
    }

    /// Current stack depth, including the root frame.
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    fn execute_current(
        &mut self,
        payload: Value,
        dispatcher: &mut impl PureNodeDispatcher,
        fact_sink: &mut impl NodeFactSink,
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
        // Framed execution owns the root-global depth and dispatch budgets. Do
        // not substitute the reducer's legacy direct-Plan compatibility limit.
        plan.set_dispatch_budget(u64::MAX);

        let mut state = plan.start(&self.run_id, payload);
        let mut now_ms = 0;
        let mut response_status = None;
        let mut last_boundary = None;
        let mut pending_retry_fact: Option<(Dispatch, NodeOutcome)> = None;
        loop {
            match plan.next(&mut state, now_ms) {
                Step::Done(ExecutionStatus::Completed) => {
                    self.emit_pending_retry_fact(&mut pending_retry_fact, fact_sink)?;
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
                    self.emit_pending_retry_fact(&mut pending_retry_fact, fact_sink)?;
                    let failure = state
                        .failure()
                        .expect("failed reducer state retains its failure");
                    let occurrence = last_boundary
                        .as_ref()
                        .filter(|(node, _)| node == &failure.node)
                        .map(|(_, occurrence)| *occurrence)
                        .unwrap_or_default();
                    return Ok(FrameCompletion::Failed(FrameFailure {
                        frame_id,
                        node_id: failure.node.clone(),
                        occurrence,
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
                Step::Wait {
                    node,
                    until_ms,
                    attempt,
                    ..
                } => {
                    if let Some((pending, _)) = pending_retry_fact.take() {
                        debug_assert_eq!(pending.node, node);
                        debug_assert!(attempt > pending.attempt);
                    }
                    now_ms = until_ms;
                }
                Step::Reserved(step) => {
                    self.emit_pending_retry_fact(&mut pending_retry_fact, fact_sink)?;
                    last_boundary = Some((step.node().to_string(), step.occurrence()));
                    plan.apply_reserved(&mut state, &step).map_err(|error| {
                        FrameExecutionError::new(
                            FrameExecutionErrorKind::InvalidTransition,
                            frame_id,
                            Some(step.node()),
                            error.to_string(),
                        )
                    })?;
                    let outcome = reserved_outcome(&step);
                    self.emit_node_fact(
                        step.node(),
                        step.occurrence(),
                        step.payload(),
                        &outcome,
                        fact_sink,
                    )?;
                }
                Step::Dispatch(node) => {
                    let is_retry = pending_retry_fact.as_ref().is_some_and(|(pending, _)| {
                        pending.node == node.node
                            && pending.occurrence == node.occurrence
                            && node.attempt > pending.attempt
                    });
                    if is_retry {
                        pending_retry_fact = None;
                    } else {
                        self.emit_pending_retry_fact(&mut pending_retry_fact, fact_sink)?;
                    }
                    last_boundary = Some((node.node.clone(), node.occurrence));
                    if let Some(failure) = self.debit_root_dispatch(&node) {
                        return Ok(FrameCompletion::Failed(failure));
                    }
                    match self.dispatch_node(&node, &mut response_status, dispatcher, fact_sink)? {
                        FrameDispatchOutcome::Node(outcome) => {
                            plan.apply(&mut state, &node, outcome.clone(), now_ms)
                                .map_err(|error| {
                                    FrameExecutionError::new(
                                        FrameExecutionErrorKind::InvalidTransition,
                                        frame_id,
                                        Some(&node.node),
                                        error.to_string(),
                                    )
                                })?;
                            if outcome_may_retry(&outcome) {
                                pending_retry_fact = Some((node, outcome));
                            } else {
                                self.emit_node_fact(
                                    &node.node,
                                    node.occurrence,
                                    &node.payload,
                                    &outcome,
                                    fact_sink,
                                )?;
                            }
                        }
                        FrameDispatchOutcome::Failed(failure) => {
                            return Ok(FrameCompletion::Failed(failure));
                        }
                    }
                }
            }
        }
    }

    fn emit_pending_retry_fact(
        &mut self,
        pending: &mut Option<(Dispatch, NodeOutcome)>,
        fact_sink: &mut impl NodeFactSink,
    ) -> Result<(), FrameExecutionError> {
        let Some((dispatch, outcome)) = pending.take() else {
            return Ok(());
        };
        self.emit_node_fact(
            &dispatch.node,
            dispatch.occurrence,
            &dispatch.payload,
            &outcome,
            fact_sink,
        )
    }

    fn emit_node_fact(
        &mut self,
        node_id: &str,
        occurrence: u32,
        input: &Value,
        outcome: &NodeOutcome,
        fact_sink: &mut impl NodeFactSink,
    ) -> Result<(), FrameExecutionError> {
        let sequence = self.next_fact_sequence;
        let next_sequence = sequence.checked_add(1).ok_or_else(|| {
            FrameExecutionError::new(
                FrameExecutionErrorKind::FactSequenceExhausted,
                self.current_record().frame_id,
                Some(node_id),
                "root-local node-fact sequence space is exhausted",
            )
        })?;
        let (fact_outcome, output) = node_fact_outcome(outcome);
        let capture = derive_capture(self.capture_mode, &output, input);
        let (
            run_id,
            root_plan_hash,
            frame_id,
            parent_frame_id,
            call_site_id,
            flow_id,
            current_plan_hash,
            source_artifact_hash,
            local_node_id,
            source_node_id,
        ) = {
            let frame = self.trusted_current().current();
            let plan_node = frame
                .plan()
                .body
                .nodes
                .iter()
                .find(|node| node.local_node_id.as_str() == node_id)
                .ok_or_else(|| {
                    self.invalid_plan_error(
                        node_id,
                        "active fact node is absent from its verified plan",
                    )
                })?;
            let source = frame
                .plan()
                .body
                .source_map
                .iter()
                .find(|source| source.local_node_id == plan_node.local_node_id)
                .filter(|source| source.source_node_id == plan_node.source_node_id)
                .ok_or_else(|| {
                    self.invalid_plan_error(
                        node_id,
                        "active fact node has no exact verified source-map entry",
                    )
                })?;
            (
                frame.run_id().to_string(),
                frame.root_plan_hash().to_string(),
                frame.frame_id(),
                frame.parent_frame_id(),
                frame.call_site_id().cloned(),
                frame.flow_id().to_string(),
                frame.current_plan_hash().to_string(),
                frame.source_artifact_hash().to_string(),
                plan_node.local_node_id.clone(),
                source.source_node_id.clone(),
            )
        };
        let fact = TrustedNodeFact {
            run_id,
            root_plan_hash,
            frame_id,
            parent_frame_id,
            call_site_id,
            flow_id,
            current_plan_hash,
            source_artifact_hash,
            local_node_id,
            source_node_id,
            occurrence,
            sequence,
            outcome: fact_outcome,
            capture,
        };
        fact_sink
            .emit(fact)
            .map_err(|source| FrameExecutionError::fact_sink_refused(frame_id, node_id, source))?;
        self.next_fact_sequence = next_sequence;
        Ok(())
    }

    fn debit_root_dispatch(&mut self, dispatch: &Dispatch) -> Option<FrameFailure> {
        if checked_debit(&mut self.dispatched_nodes, self.root_dispatch_budget) {
            return None;
        }
        Some(FrameFailure {
            frame_id: self.current_record().frame_id,
            node_id: dispatch.node.clone(),
            occurrence: dispatch.occurrence,
            kind: ExecutionFailureKind::DispatchBudget,
            detail: ErrorDetail::coded(
                "dispatch-budget",
                format!(
                    "root dispatch budget of {} exhausted before node dispatch",
                    self.root_dispatch_budget
                ),
            ),
        })
    }

    fn dispatch_node(
        &mut self,
        dispatch: &Dispatch,
        response_status: &mut Option<u16>,
        dispatcher: &mut impl PureNodeDispatcher,
        fact_sink: &mut impl NodeFactSink,
    ) -> Result<FrameDispatchOutcome, FrameExecutionError> {
        match dispatch.node_type.as_str() {
            "request" | "event" => Ok(FrameDispatchOutcome::Node(NodeOutcome::ok(
                dispatch.payload.clone(),
            ))),
            "respond" => {
                let config = serde_json::from_value::<RespondConfig>(dispatch.config.clone())
                    .map_err(|error| self.invalid_plan_error(&dispatch.node, error.to_string()))?;
                *response_status = Some(config.status);
                Ok(FrameDispatchOutcome::Node(NodeOutcome::ok(
                    dispatch.payload.clone(),
                )))
            }
            "call-flow" => self.dispatch_call(dispatch, dispatcher, fact_sink),
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
                Ok(FrameDispatchOutcome::Node(
                    dispatcher.dispatch(self.trusted_current(), dispatch),
                ))
            }
        }
    }

    fn dispatch_call(
        &mut self,
        dispatch: &Dispatch,
        dispatcher: &mut impl PureNodeDispatcher,
        fact_sink: &mut impl NodeFactSink,
    ) -> Result<FrameDispatchOutcome, FrameExecutionError> {
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

        if let Some(error) = entry_input_error(callee.plan(), &dispatch.payload)
            .map_err(|message| self.invalid_plan_error(&dispatch.node, message))?
        {
            return Ok(FrameDispatchOutcome::Node(NodeOutcome::Error(
                NodeError::InvalidInput(error),
            )));
        }

        if self.frames.len() > MAX_CALL_DEPTH {
            return Ok(FrameDispatchOutcome::Failed(FrameFailure {
                frame_id: self.current_record().frame_id,
                node_id: dispatch.node.clone(),
                occurrence: dispatch.occurrence,
                kind: ExecutionFailureKind::DepthBudget,
                detail: ErrorDetail::coded(
                    "depth-budget",
                    format!("maximum call depth of {MAX_CALL_DEPTH} exhausted before callee entry"),
                ),
            }));
        }

        let frame_id = self.push_callee(&instruction)?;
        let completion = self.execute_current(dispatch.payload.clone(), dispatcher, fact_sink);
        self.pop_callee(frame_id);
        match completion? {
            FrameCompletion::Responded { body, .. } => {
                Ok(FrameDispatchOutcome::Node(NodeOutcome::ok(body)))
            }
            FrameCompletion::Failed(failure) if failure.is_root_budget_exhaustion() => {
                Ok(FrameDispatchOutcome::Failed(failure))
            }
            FrameCompletion::Failed(failure) => Ok(FrameDispatchOutcome::Node(NodeOutcome::Error(
                callee_failure_outcome(&failure),
            ))),
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

/// Verdict of one payload against a verified plan's entry input-schema guard.
///
/// Both admission boundaries share this check and its classification: the
/// call-flow boundary validates a callee's guard before pushing its frame, and
/// the root boundary validates the root plan's own guard before the graph is
/// projected (wamn-0h0g.15.118). `Err` means the verified guard itself cannot be
/// compiled — a plan-integrity refusal, not a payload refusal.
fn entry_input_error(
    plan: &ExecutionPlanV2,
    payload: &Value,
) -> Result<Option<ErrorDetail>, String> {
    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    compiler
        .add_resource(
            ENTRY_INPUT_SCHEMA_URI,
            plan.body.entry_input_schema_guard.clone(),
        )
        .map_err(|error| format!("verified entry input guard cannot be loaded: {error}"))?;
    let mut schemas = Schemas::new();
    let schema = compiler
        .compile(ENTRY_INPUT_SCHEMA_URI, &mut schemas)
        .map_err(|error| format!("verified entry input guard cannot be compiled: {error}"))?;
    Ok(schemas
        .validate(payload, schema)
        .err()
        .map(|_| ErrorDetail {
            message: "payload does not satisfy the entry input schema".to_string(),
            code: Some(CALL_INPUT_INVALID.to_string()),
            data: None,
        }))
}

fn checked_debit(counter: &mut u64, limit: u64) -> bool {
    if *counter >= limit {
        return false;
    }
    *counter = counter
        .checked_add(1)
        .expect("a dispatch counter below its u64 limit can increment");
    true
}

fn outcome_may_retry(outcome: &NodeOutcome) -> bool {
    matches!(
        outcome,
        NodeOutcome::Error(NodeError::Retryable(_) | NodeError::RateLimited(_))
    )
}

fn node_fact_outcome(outcome: &NodeOutcome) -> (NodeFactOutcome, Value) {
    match outcome {
        NodeOutcome::Success { payload, port, .. } => (
            NodeFactOutcome::Success {
                output_port: port.clone(),
            },
            payload.clone(),
        ),
        NodeOutcome::Error(error) => (
            NodeFactOutcome::Error(error.clone()),
            node_error_detail(error).to_error_payload(),
        ),
    }
}

fn node_error_detail(error: &NodeError) -> &ErrorDetail {
    match error {
        NodeError::Retryable(detail)
        | NodeError::Terminal(detail)
        | NodeError::InvalidInput(detail) => detail,
        NodeError::RateLimited(rate_limit) => &rate_limit.detail,
    }
}

fn reserved_outcome(step: &ReservedStep) -> NodeOutcome {
    match step {
        ReservedStep::Entry { payload, .. } => NodeOutcome::ok(payload.clone()),
        ReservedStep::Fail { code, message, .. } => NodeOutcome::Error(NodeError::Terminal(
            ErrorDetail::coded(code, message.as_deref().unwrap_or(code)),
        )),
    }
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
        // The execution plan carries no test cases; this reconstruction is for
        // structural revalidation of the graph only.
        cases: Vec::new(),
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

    #[derive(Default)]
    struct RecordingFactSink {
        facts: Vec<TrustedNodeFact>,
        refuse_at: Option<usize>,
    }

    impl NodeFactSink for RecordingFactSink {
        fn emit(&mut self, fact: TrustedNodeFact) -> Result<(), NodeFactSinkError> {
            if self.refuse_at == Some(self.facts.len()) {
                return Err(NodeFactSinkError::new("test fact sink refusal"));
            }
            self.facts.push(fact);
            Ok(())
        }
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
                "observe" | "observe-local" | "recover" | "loop-a" | "loop-b" => {
                    NodeOutcome::ok(node.payload.clone())
                }
                "retry-once" if node.attempt == 0 => NodeOutcome::Error(NodeError::Retryable(
                    ErrorDetail::coded("try-again", "retry once"),
                )),
                "retry-once" => NodeOutcome::ok(node.payload.clone()),
                "branch-fail" => NodeOutcome::ok_on(node.payload.clone(), "stop"),
                "stop" => NodeOutcome::Error(NodeError::Terminal(ErrorDetail::coded(
                    "authored-stop",
                    "stop now",
                ))),
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

    fn runaway_loop_plan() -> ExecutionPlanV2 {
        request_plan(
            &hash('c'),
            json!(true),
            vec![
                node(
                    "loop-a",
                    "test-loop",
                    json!({}),
                    ExecutionEffectPolicy::Pure,
                ),
                node(
                    "loop-b",
                    "test-loop",
                    json!({}),
                    ExecutionEffectPolicy::Pure,
                ),
            ],
            vec![
                edge("request", "main", "loop-a"),
                edge("loop-a", "main", "loop-b"),
                edge("loop-a", "done", RESPOND),
                edge("loop-b", "main", "loop-a"),
                edge("loop-b", "done", RESPOND),
            ],
            200,
        )
    }

    #[test]
    fn source_map_drives_author_selector_and_missing_mapping_never_falls_back() {
        let mut root = request_plan(
            &hash('c'),
            json!(true),
            vec![node(
                "observe-local",
                "test-pure",
                json!({}),
                ExecutionEffectPolicy::Pure,
            )],
            vec![
                edge("request", "main", "observe-local"),
                edge("observe-local", "main", RESPOND),
            ],
            200,
        );
        root.body
            .nodes
            .iter_mut()
            .find(|node| node.local_node_id.as_str() == "observe-local")
            .unwrap()
            .source_node_id = "authored-observe".to_string();
        root.body
            .source_map
            .iter_mut()
            .find(|source| source.local_node_id.as_str() == "observe-local")
            .unwrap()
            .source_node_id = "authored-observe".to_string();
        let valid_snapshot = snapshot("root-flow", vec![("root-flow", root)]);
        let root_plan_hash = valid_snapshot.root_execution_bundle_hash().to_string();
        let mut stack = FrameStack::new("run-source", &valid_snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        assert!(matches!(
            stack
                .execute_root(json!({}), &mut dispatcher, &mut fact_sink)
                .unwrap(),
            FrameCompletion::Responded { .. }
        ));

        let observed = fact_sink
            .facts
            .iter()
            .find(|fact| fact.local_node_id().as_str() == "observe-local")
            .unwrap();
        assert_eq!(observed.source_node_id(), "authored-observe");
        assert_eq!(observed.flow_id(), "root-flow");
        assert_eq!(observed.root_plan_hash(), root_plan_hash);
        assert_eq!(observed.current_plan_hash(), root_plan_hash);
        assert_eq!(observed.source_artifact_hash(), hash('c'));

        let mut missing = request_plan(
            &hash('d'),
            json!(true),
            Vec::new(),
            vec![edge("request", "main", RESPOND)],
            200,
        );
        missing
            .body
            .source_map
            .retain(|source| source.local_node_id.as_str() != "request");
        let missing_snapshot = snapshot("missing-map", vec![("missing-map", missing)]);
        let mut missing_stack =
            FrameStack::new("run-missing-map", &missing_snapshot, CaptureMode::Off);
        let mut missing_dispatcher = RecordingDispatcher::default();
        let mut missing_sink = RecordingFactSink::default();

        let error = missing_stack
            .execute_root(json!({}), &mut missing_dispatcher, &mut missing_sink)
            .unwrap_err();

        assert_eq!(error.kind(), FrameExecutionErrorKind::InvalidVerifiedPlan);
        assert_eq!(error.frame_id(), 0);
        assert_eq!(error.node_id(), Some("request"));
        assert!(missing_dispatcher.seen.is_empty());
        assert!(missing_sink.facts.is_empty());
        assert_eq!(missing_stack.next_fact_sequence, 0);
    }

    #[test]
    fn capture_full_scrubs_and_bounds_while_off_records_zero_node_io() {
        let root = request_plan(
            &hash('c'),
            json!(true),
            vec![node(
                "observe",
                "test-pure",
                json!({}),
                ExecutionEffectPolicy::Pure,
            )],
            vec![
                edge("request", "main", "observe"),
                edge("observe", "main", RESPOND),
            ],
            200,
        );
        let snapshot = snapshot("root", vec![("root", root)]);
        let payload = json!({"token": "secret-value", "visible": "yes"});
        let mut full_stack = FrameStack::new("run-full", &snapshot, CaptureMode::Full);
        let mut full_dispatcher = RecordingDispatcher::default();
        let mut full_sink = RecordingFactSink::default();
        full_stack
            .execute_root(payload.clone(), &mut full_dispatcher, &mut full_sink)
            .unwrap();

        let captured = full_sink
            .facts
            .iter()
            .find(|fact| fact.local_node_id().as_str() == "observe")
            .unwrap()
            .capture();
        let stored = format!(
            "{}{}",
            captured.output_json.as_deref().unwrap(),
            captured.input_json.as_deref().unwrap()
        );
        assert!(!stored.contains("secret-value"));
        assert!(stored.contains(wamn_run_state::capture::REDACTED));
        assert_eq!(
            captured.output_size,
            Some(i64::try_from(payload.to_string().len()).unwrap())
        );
        assert!(captured.payload_hash.is_some());

        let oversized = json!({
            "blob": "x".repeat(wamn_run_state::capture::OUTPUT_CAPTURE_CEILING_BYTES),
            "password": "oversized-secret",
        });
        let mut oversized_stack = FrameStack::new("run-oversized", &snapshot, CaptureMode::Full);
        let mut oversized_dispatcher = RecordingDispatcher::default();
        let mut oversized_sink = RecordingFactSink::default();
        oversized_stack
            .execute_root(oversized, &mut oversized_dispatcher, &mut oversized_sink)
            .unwrap();
        let oversized_capture = oversized_sink
            .facts
            .iter()
            .find(|fact| fact.local_node_id().as_str() == "observe")
            .unwrap()
            .capture();
        assert_eq!(oversized_capture.output_json, None);
        assert!(
            oversized_capture.output_size.unwrap()
                > i64::try_from(wamn_run_state::capture::OUTPUT_CAPTURE_CEILING_BYTES).unwrap()
        );
        assert!(oversized_capture.payload_hash.is_some());
        assert!(
            !oversized_capture
                .input_json
                .as_deref()
                .unwrap()
                .contains("oversized-secret")
        );

        let mut off_stack = FrameStack::new("run-off", &snapshot, CaptureMode::Off);
        let mut off_dispatcher = RecordingDispatcher::default();
        let mut off_sink = RecordingFactSink::default();
        off_stack
            .execute_root(payload, &mut off_dispatcher, &mut off_sink)
            .unwrap();
        assert!(off_sink.facts.iter().all(|fact| {
            fact.capture().input_json.is_none()
                && fact.capture().output_json.is_none()
                && fact.capture().output_size.is_none()
                && fact.capture().payload_hash.is_none()
        }));
    }

    #[test]
    fn retry_attempts_emit_only_the_completed_occurrence_fact() {
        let root = request_plan(
            &hash('c'),
            json!(true),
            vec![node(
                "retry-once",
                "test-pure",
                json!({"retry": {"max-attempts": 2, "base-ms": 0}}),
                ExecutionEffectPolicy::Pure,
            )],
            vec![
                edge("request", "main", "retry-once"),
                edge("retry-once", "main", RESPOND),
            ],
            200,
        );
        let snapshot = snapshot("root", vec![("root", root)]);
        let mut stack = FrameStack::new("run-retry", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        stack
            .execute_root(json!({}), &mut dispatcher, &mut fact_sink)
            .unwrap();

        assert_eq!(
            dispatcher
                .seen
                .iter()
                .map(|seen| seen.node_id.as_str())
                .collect::<Vec<_>>(),
            ["retry-once", "retry-once"]
        );
        assert_eq!(
            fact_sink
                .facts
                .iter()
                .map(|fact| (fact.sequence(), fact.local_node_id().as_str()))
                .collect::<Vec<_>>(),
            [(0, "request"), (1, "retry-once"), (2, RESPOND)]
        );
        let retried = &fact_sink.facts[1];
        assert_eq!(retried.occurrence(), 0);
        assert!(matches!(retried.outcome(), NodeFactOutcome::Success { .. }));
    }

    #[test]
    fn fact_sink_refusal_fails_closed_without_allocating_or_running_a_successor() {
        let root = request_plan(
            &hash('c'),
            json!(true),
            vec![node(
                "observe",
                "test-pure",
                json!({}),
                ExecutionEffectPolicy::Pure,
            )],
            vec![
                edge("request", "main", "observe"),
                edge("observe", "main", RESPOND),
            ],
            200,
        );
        let snapshot = snapshot("root", vec![("root", root)]);
        let mut stack = FrameStack::new("run-refusal", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink {
            facts: Vec::new(),
            refuse_at: Some(1),
        };

        let error = stack
            .execute_root(json!({}), &mut dispatcher, &mut fact_sink)
            .unwrap_err();

        assert_eq!(error.kind(), FrameExecutionErrorKind::FactSinkRefused);
        assert_eq!(error.frame_id(), 0);
        assert_eq!(error.node_id(), Some("observe"));
        assert_eq!(
            std::error::Error::source(&error).unwrap().to_string(),
            "test fact sink refusal"
        );
        assert_eq!(dispatcher.seen.len(), 1);
        assert_eq!(dispatcher.seen[0].node_id, "observe");
        assert_eq!(fact_sink.facts.len(), 1);
        assert_eq!(fact_sink.facts[0].sequence(), 0);
        assert_eq!(fact_sink.facts[0].local_node_id().as_str(), "request");
        assert_eq!(stack.next_fact_sequence, 1);
        assert_eq!(stack.depth(), 1);

        let replay = stack
            .execute_root(json!({"again": true}), &mut dispatcher, &mut fact_sink)
            .unwrap_err();
        assert_eq!(replay.kind(), FrameExecutionErrorKind::RootAlreadyExecuted);
        assert_eq!(dispatcher.seen.len(), 1);
        assert_eq!(fact_sink.facts.len(), 1);
    }

    #[test]
    fn reserved_entry_and_dispatched_fail_emit_exact_terminal_facts() {
        let root = request_plan(
            &hash('c'),
            json!(true),
            vec![
                node(
                    "branch-fail",
                    "test-branch",
                    json!({}),
                    ExecutionEffectPolicy::Pure,
                ),
                node(
                    "stop",
                    "fail",
                    json!({"code": "authored-stop", "message": "stop now", "status": 422}),
                    ExecutionEffectPolicy::Pure,
                ),
            ],
            vec![
                edge("request", "main", "branch-fail"),
                edge("branch-fail", "stop", "stop"),
                edge("branch-fail", "continue", RESPOND),
            ],
            200,
        );
        let snapshot = snapshot("root", vec![("root", root)]);
        let mut stack = FrameStack::new("run-fail", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        let FrameCompletion::Failed(failure) = stack
            .execute_root(json!({"value": 1}), &mut dispatcher, &mut fact_sink)
            .unwrap()
        else {
            panic!("the fail node must terminate the frame");
        };

        assert_eq!(failure.node_id(), "stop");
        assert_eq!(failure.detail().code.as_deref(), Some("authored-stop"));
        assert_eq!(dispatcher.seen.len(), 2);
        assert_eq!(dispatcher.seen[0].node_id, "branch-fail");
        assert_eq!(dispatcher.seen[1].node_id, "stop");
        assert_eq!(
            fact_sink
                .facts
                .iter()
                .map(|fact| (fact.sequence(), fact.local_node_id().as_str()))
                .collect::<Vec<_>>(),
            [(0, "request"), (1, "branch-fail"), (2, "stop")]
        );
        assert!(matches!(
            fact_sink.facts[2].outcome(),
            NodeFactOutcome::Error(NodeError::Terminal(detail))
                if detail.code.as_deref() == Some("authored-stop")
        ));
    }

    #[test]
    fn root_budget_constants_are_exact_platform_literals() {
        assert_eq!(MAX_CALL_DEPTH, 64);
        assert_eq!(DEFAULT_ROOT_DISPATCH_BUDGET, 10_000);
    }

    #[test]
    fn checked_dispatch_counter_reaches_its_limit_without_wrapping() {
        let mut counter = u64::MAX - 1;

        assert!(checked_debit(&mut counter, u64::MAX));
        assert_eq!(counter, u64::MAX);
        assert!(!checked_debit(&mut counter, u64::MAX));
        assert_eq!(counter, u64::MAX);
    }

    #[test]
    fn root_global_dispatch_budget_fails_the_ten_thousand_and_first_dispatch() {
        let root = runaway_loop_plan();
        let snapshot = snapshot("root", vec![("root", root)]);
        let mut stack = FrameStack::new("run-dispatch-budget", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        let FrameCompletion::Failed(failure) = stack
            .execute_root(json!({}), &mut dispatcher, &mut fact_sink)
            .unwrap()
        else {
            panic!("the root-global dispatch budget must fail the runaway loop");
        };

        assert_eq!(failure.kind(), ExecutionFailureKind::DispatchBudget);
        assert_eq!(failure.detail().code.as_deref(), Some("dispatch-budget"));
        assert_eq!(failure.frame_id(), 0);
        assert_eq!(failure.node_id(), "loop-b");
        assert_eq!(failure.occurrence(), 4_999);
        assert_eq!(stack.dispatched_nodes, DEFAULT_ROOT_DISPATCH_BUDGET);
        assert_eq!(dispatcher.seen.len(), 9_999);
        assert_eq!(stack.depth(), 1);
        assert_eq!(fact_sink.facts.len(), 10_000);
        assert_eq!(fact_sink.facts.last().unwrap().sequence(), 9_999);
        assert!(
            fact_sink
                .facts
                .iter()
                .all(|fact| fact.outcome().error().is_none()),
            "dispatch-budget is a run failure, not an invented node error fact"
        );
    }

    #[test]
    fn dispatch_budget_debits_before_call_input_validation() {
        let callee = request_plan(
            &hash('d'),
            json!({"type": "string"}),
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
        let mut stack = FrameStack::new("run-budget-before-guard", &snapshot, CaptureMode::Off);
        stack.root_dispatch_budget = 1;
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        let FrameCompletion::Failed(failure) = stack
            .execute_root(json!(7), &mut dispatcher, &mut fact_sink)
            .unwrap()
        else {
            panic!("dispatch exhaustion must win before the invalid call input guard");
        };

        assert_eq!(failure.kind(), ExecutionFailureKind::DispatchBudget);
        assert_eq!(failure.frame_id(), 0);
        assert_eq!(failure.node_id(), "call");
        assert_eq!(failure.occurrence(), 0);
        assert_eq!(stack.dispatched_nodes, 1);
        assert_eq!(stack.next_frame_id, 1);
        assert!(dispatcher.seen.is_empty());
    }

    #[test]
    fn nested_dispatch_budget_failure_bypasses_the_call_error_edge() {
        let callee = request_plan(
            &hash('d'),
            json!(true),
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
        let mut stack = FrameStack::new("run-nested-dispatch-budget", &snapshot, CaptureMode::Off);
        stack.root_dispatch_budget = 2;
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        let FrameCompletion::Failed(failure) = stack
            .execute_root(json!({}), &mut dispatcher, &mut fact_sink)
            .unwrap()
        else {
            panic!("nested dispatch exhaustion must fail the root");
        };

        assert_eq!(failure.kind(), ExecutionFailureKind::DispatchBudget);
        assert_eq!(failure.frame_id(), 1);
        assert_eq!(failure.node_id(), "request");
        assert_eq!(failure.occurrence(), 0);
        assert_eq!(stack.dispatched_nodes, 2);
        assert_eq!(stack.next_frame_id, 2);
        assert!(
            dispatcher.seen.is_empty(),
            "the caller error edge did not run"
        );
        assert_eq!(stack.depth(), 1);
        assert_eq!(
            fact_sink
                .facts
                .iter()
                .map(|fact| (fact.frame_id(), fact.local_node_id().as_str()))
                .collect::<Vec<_>>(),
            [(0, "request")],
            "budget refusal emits no fact for the refused node or caller"
        );
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
        let mut stack = FrameStack::new("run-7", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        let completion = stack
            .execute_root(json!({"calls": 0}), &mut dispatcher, &mut fact_sink)
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
        assert_eq!(
            fact_sink
                .facts
                .iter()
                .map(|fact| (
                    fact.sequence(),
                    fact.frame_id(),
                    fact.local_node_id().as_str(),
                ))
                .collect::<Vec<_>>(),
            [
                (0, 0, "request"),
                (1, 1, "request"),
                (2, 1, "stamp"),
                (3, 1, RESPOND),
                (4, 0, "call-one"),
                (5, 2, "request"),
                (6, 2, "stamp"),
                (7, 2, RESPOND),
                (8, 0, "call-two"),
                (9, 0, "observe"),
                (10, 0, RESPOND),
            ],
            "one accepted root-global sequence orders callee completion before each caller fact"
        );
        let call_facts = fact_sink
            .facts
            .iter()
            .filter(|fact| fact.local_node_id().as_str().starts_with("call-"))
            .collect::<Vec<_>>();
        assert_eq!(call_facts.len(), 2, "each call occurrence emits once");
        assert!(call_facts.iter().all(|fact| fact.frame_id() == 0));
        assert!(call_facts.iter().all(|fact| fact.call_site_id().is_none()));
        assert!(call_facts.iter().all(|fact| {
            matches!(
                fact.outcome(),
                NodeFactOutcome::Success { output_port } if output_port == wamn_flow::MAIN_PORT
            )
        }));
        assert!(fact_sink.facts[1..4].iter().all(|fact| {
            fact.parent_frame_id() == Some(0)
                && fact
                    .call_site_id()
                    .is_some_and(|site| site.as_str() == "call-one")
                && fact.current_plan_hash() == callee_hash
                && fact.root_plan_hash() == root_hash
                && fact.flow_id() == "callee"
                && fact.source_artifact_hash() == hash('d')
        }));
        assert!(fact_sink.facts.iter().all(|fact| {
            fact.run_id() == "run-7"
                && fact.capture().input_json.is_none()
                && fact.capture().output_json.is_none()
                && fact.capture().output_size.is_none()
                && fact.capture().payload_hash.is_none()
        }));
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
        let mut stack = FrameStack::new("run-invalid", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        let FrameCompletion::Responded { body, .. } = stack
            .execute_root(json!({"name": 7}), &mut dispatcher, &mut fact_sink)
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
        assert_eq!(
            fact_sink
                .facts
                .iter()
                .map(|fact| (fact.sequence(), fact.local_node_id().as_str()))
                .collect::<Vec<_>>(),
            [(0, "request"), (1, "call"), (2, "recover"), (3, RESPOND)]
        );
        let call_fact = &fact_sink.facts[1];
        assert_eq!(call_fact.frame_id(), 0);
        assert_eq!(call_fact.parent_frame_id(), None);
        assert_eq!(call_fact.call_site_id(), None);
        assert!(matches!(
            call_fact.outcome(),
            NodeFactOutcome::Error(NodeError::InvalidInput(detail))
                if detail.code.as_deref() == Some(CALL_INPUT_INVALID)
        ));
    }

    fn guarded_entry_plan() -> ExecutionPlanV2 {
        request_plan(
            &hash('c'),
            json!({
                "type": "object",
                "required": ["name"],
                "properties": {"name": {"type": "string"}}
            }),
            vec![node(
                "observe",
                "test-pure",
                json!({}),
                ExecutionEffectPolicy::Pure,
            )],
            vec![
                edge("request", "main", "observe"),
                edge("observe", "main", RESPOND),
            ],
            200,
        )
    }

    #[test]
    fn root_entry_guard_refuses_an_inadmissible_payload_before_projection() {
        let snapshot = snapshot("root", vec![("root", guarded_entry_plan())]);
        let mut stack = FrameStack::new("run-root-guard", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        let FrameCompletion::Failed(failure) = stack
            .execute_root(json!({"name": 7}), &mut dispatcher, &mut fact_sink)
            .unwrap()
        else {
            panic!("a payload violating the root entry guard must fail the root");
        };

        assert_eq!(failure.kind(), ExecutionFailureKind::InvalidInput);
        assert_eq!(failure.detail().code.as_deref(), Some(CALL_INPUT_INVALID));
        assert_eq!(failure.frame_id(), 0);
        assert_eq!(failure.node_id(), "request");
        assert_eq!(failure.occurrence(), 0);
        assert!(
            dispatcher.seen.is_empty(),
            "the refused payload never reached a node"
        );
        assert!(
            fact_sink.facts.is_empty(),
            "the refused payload emits no entry fact"
        );
        assert_eq!(stack.dispatched_nodes, 0);
        assert_eq!(stack.next_fact_sequence, 0);
        assert_eq!(stack.depth(), 1);

        let replay = stack
            .execute_root(json!({"name": "ada"}), &mut dispatcher, &mut fact_sink)
            .unwrap_err();

        assert_eq!(replay.kind(), FrameExecutionErrorKind::RootAlreadyExecuted);
        assert!(dispatcher.seen.is_empty(), "a refused root cannot replay");
    }

    #[test]
    fn root_entry_guard_admits_a_conforming_payload_under_the_same_plan() {
        let snapshot = snapshot("root", vec![("root", guarded_entry_plan())]);
        let mut stack = FrameStack::new("run-root-admit", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        assert_eq!(
            stack
                .execute_root(json!({"name": "ada"}), &mut dispatcher, &mut fact_sink)
                .unwrap(),
            FrameCompletion::Responded {
                body: json!({"name": "ada"}),
                status: 200,
            }
        );
        assert_eq!(
            dispatcher
                .seen
                .iter()
                .map(|seen| seen.node_id.as_str())
                .collect::<Vec<_>>(),
            ["observe"]
        );
        assert_eq!(
            fact_sink
                .facts
                .iter()
                .map(|fact| (fact.sequence(), fact.local_node_id().as_str()))
                .collect::<Vec<_>>(),
            [(0, "request"), (1, "observe"), (2, RESPOND)]
        );
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
        let mut stack = FrameStack::new("run-failure", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        let FrameCompletion::Responded { body, .. } = stack
            .execute_root(json!({"value": 1}), &mut dispatcher, &mut fact_sink)
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
        assert_eq!(
            fact_sink
                .facts
                .iter()
                .map(|fact| (
                    fact.sequence(),
                    fact.frame_id(),
                    fact.local_node_id().as_str(),
                ))
                .collect::<Vec<_>>(),
            [
                (0, 0, "request"),
                (1, 1, "request"),
                (2, 1, "explode"),
                (3, 0, "call"),
                (4, 0, "recover"),
                (5, 0, RESPOND),
            ]
        );
        let explode = &fact_sink.facts[2];
        assert_eq!(explode.parent_frame_id(), Some(0));
        assert_eq!(
            explode.call_site_id().map(ExecutionNodeId::as_str),
            Some("call")
        );
        assert!(matches!(
            explode.outcome(),
            NodeFactOutcome::Error(NodeError::Terminal(detail))
                if detail.code.as_deref() == Some("callee-broke")
        ));
        let caller = &fact_sink.facts[3];
        assert_eq!(caller.frame_id(), 0);
        assert!(matches!(
            caller.outcome(),
            NodeFactOutcome::Error(NodeError::Terminal(detail))
                if detail.code.as_deref() == Some("callee-broke")
        ));
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
    fn invalid_sixty_fifth_call_input_wins_before_depth_budget() {
        let plan = request_plan(
            &hash('c'),
            json!({"type": "integer", "minimum": 1}),
            vec![
                node(
                    "branch",
                    "test-branch",
                    json!({}),
                    ExecutionEffectPolicy::Pure,
                ),
                call_node("call", "self-flow"),
            ],
            vec![
                edge("request", "main", "branch"),
                edge("branch", "recurse", "call"),
                edge("branch", "done", RESPOND),
                edge("call", "main", RESPOND),
                edge("call", wamn_flow::ERROR_PORT, RESPOND),
            ],
            200,
        );
        let snapshot = snapshot("self-flow", vec![("self-flow", plan)]);
        let mut stack = FrameStack::new("run-depth-guard-order", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        let FrameCompletion::Responded { body, .. } = stack
            .execute_root(json!(MAX_CALL_DEPTH + 1), &mut dispatcher, &mut fact_sink)
            .unwrap()
        else {
            panic!("the invalid call input must route before the depth floor");
        };

        assert_eq!(body["error"]["code"], CALL_INPUT_INVALID);
        assert_eq!(
            stack.next_frame_id,
            u64::try_from(MAX_CALL_DEPTH).expect("platform call depth fits u64") + 1,
            "the invalid sixty-fifth call did not allocate a frame identity"
        );
        assert_eq!(
            dispatcher.seen.last().map(|seen| seen.frame_id),
            Some(u64::try_from(MAX_CALL_DEPTH).expect("platform call depth fits u64"))
        );
        assert_eq!(stack.depth(), 1, "every admitted callee was popped");
    }

    #[test]
    fn sixty_fifth_valid_recursive_call_fails_before_frame_id_allocation() {
        let plan = recursive_plan(&hash('c'), "self-flow");
        let snapshot = snapshot("self-flow", vec![("self-flow", plan)]);
        let mut stack = FrameStack::new("run-depth-budget", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        let FrameCompletion::Failed(failure) = stack
            .execute_root(json!(MAX_CALL_DEPTH + 1), &mut dispatcher, &mut fact_sink)
            .unwrap()
        else {
            panic!("the sixty-fifth valid recursive call must fail the root");
        };

        assert_eq!(failure.kind(), ExecutionFailureKind::DepthBudget);
        assert_eq!(failure.detail().code.as_deref(), Some("depth-budget"));
        assert_eq!(
            failure.frame_id(),
            u64::try_from(MAX_CALL_DEPTH).expect("platform call depth fits u64")
        );
        assert_eq!(failure.node_id(), "call");
        assert_eq!(failure.occurrence(), 0);
        assert_eq!(
            stack.next_frame_id,
            u64::try_from(MAX_CALL_DEPTH).expect("platform call depth fits u64") + 1,
            "the refused sixty-fifth callee did not allocate a frame identity"
        );
        assert_eq!(
            stack.dispatched_nodes,
            u64::try_from((MAX_CALL_DEPTH + 1) * 3).expect("platform budget proof count fits u64")
        );
        assert_eq!(stack.depth(), 1, "every admitted callee was popped");
    }

    #[test]
    fn finite_self_and_mutual_recursion_use_ordinary_monotonic_frames() {
        let self_plan = recursive_plan(&hash('c'), "self-flow");
        let self_snapshot = snapshot("self-flow", vec![("self-flow", self_plan)]);
        let mut self_stack = FrameStack::new("run-self", &self_snapshot, CaptureMode::Off);
        let mut self_dispatcher = RecordingDispatcher::default();
        let mut self_fact_sink = RecordingFactSink::default();
        assert_eq!(
            self_stack
                .execute_root(json!(2), &mut self_dispatcher, &mut self_fact_sink)
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
        assert_eq!(
            self_fact_sink
                .facts
                .iter()
                .map(TrustedNodeFact::sequence)
                .collect::<Vec<_>>(),
            (0..u64::try_from(self_fact_sink.facts.len()).unwrap()).collect::<Vec<_>>()
        );
        let self_keys = self_fact_sink
            .facts
            .iter()
            .map(|fact| {
                (
                    fact.frame_id(),
                    fact.local_node_id().clone(),
                    fact.occurrence(),
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(self_keys.len(), self_fact_sink.facts.len());
        assert_eq!(
            self_fact_sink
                .facts
                .iter()
                .filter(|fact| fact.local_node_id().as_str() == "call")
                .map(TrustedNodeFact::frame_id)
                .collect::<Vec<_>>(),
            [1, 0],
            "callee call facts complete before their callers"
        );

        let alpha = recursive_plan(&hash('d'), "beta");
        let beta = recursive_plan(&hash('e'), "alpha");
        let mutual_snapshot = snapshot("alpha", vec![("alpha", alpha), ("beta", beta)]);
        let mut mutual_stack = FrameStack::new("run-mutual", &mutual_snapshot, CaptureMode::Off);
        let mut mutual_dispatcher = RecordingDispatcher::default();
        let mut mutual_fact_sink = RecordingFactSink::default();
        assert!(matches!(
            mutual_stack
                .execute_root(json!(3), &mut mutual_dispatcher, &mut mutual_fact_sink)
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
        let mutual_keys = mutual_fact_sink
            .facts
            .iter()
            .map(|fact| {
                (
                    fact.frame_id(),
                    fact.local_node_id().clone(),
                    fact.occurrence(),
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(mutual_keys.len(), mutual_fact_sink.facts.len());
        assert!(
            mutual_fact_sink
                .facts
                .iter()
                .enumerate()
                .all(|(index, fact)| {
                    fact.sequence() == u64::try_from(index).expect("fact count fits u64")
                })
        );
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
        let mut stack = FrameStack::new("run-missing", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        let error = stack
            .execute_root(json!({}), &mut dispatcher, &mut fact_sink)
            .unwrap_err();

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
        let mut stack = FrameStack::new("run-forged", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        let error = stack
            .execute_root(json!({}), &mut dispatcher, &mut fact_sink)
            .unwrap_err();

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
        let mut stack = FrameStack::new("run-effect", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        let error = stack
            .execute_root(json!({}), &mut dispatcher, &mut fact_sink)
            .unwrap_err();

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
        assert_eq!(
            fact_sink
                .facts
                .iter()
                .map(|fact| (fact.frame_id(), fact.local_node_id().as_str()))
                .collect::<Vec<_>>(),
            [(0, "request"), (1, "request")],
            "the effectful occurrence and its unfinished call site emit no fact"
        );

        let replay = stack
            .execute_root(json!({}), &mut dispatcher, &mut fact_sink)
            .unwrap_err();
        assert_eq!(replay.kind(), FrameExecutionErrorKind::RootAlreadyExecuted);
        assert!(dispatcher.seen.is_empty(), "a refused root cannot replay");
        assert_eq!(
            fact_sink.facts.len(),
            2,
            "a refused root cannot re-emit facts"
        );
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
        let mut stack = FrameStack::new("run-once", &snapshot, CaptureMode::Off);
        let mut dispatcher = RecordingDispatcher::default();
        let mut fact_sink = RecordingFactSink::default();

        assert!(matches!(
            stack
                .execute_root(json!({}), &mut dispatcher, &mut fact_sink)
                .unwrap(),
            FrameCompletion::Responded { .. }
        ));
        let replay = stack
            .execute_root(json!({"replay": true}), &mut dispatcher, &mut fact_sink)
            .unwrap_err();

        assert_eq!(replay.kind(), FrameExecutionErrorKind::RootAlreadyExecuted);
        assert_eq!(replay.frame_id(), 0);
        assert_eq!(replay.node_id(), None);
        assert!(dispatcher.seen.is_empty());
        assert_eq!(stack.depth(), 1);
    }
}

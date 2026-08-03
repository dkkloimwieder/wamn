//! Executable prototype for moving the remaining reserved primitives through
//! `wamn:node` without giving nodes authority over engine lifecycle state.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use wamn_flow::{Flow, ResolvedInterfaces};
use wamn_node_sdk::{
    Emission, ErrorDetail, HttpCapError, HttpRequest, HttpResponse, Node, NodeCtx, NodeError,
    PgCapError, PgRows, PgValue, RunContext,
};
use wamn_runner::{
    CallerState, ExecutionState, ExecutionStatus, MAIN_PORT, NodeOutcome, Plan, ReservedStep, Step,
};

struct NoEffects;

impl NodeCtx for NoEffects {
    fn http(&mut self, _req: &HttpRequest) -> Result<HttpResponse, HttpCapError> {
        unreachable!("lifecycle prototype nodes have no capabilities")
    }

    fn pg_query(&mut self, _sql: &str, _params: &[PgValue]) -> Result<PgRows, PgCapError> {
        unreachable!("lifecycle prototype nodes have no capabilities")
    }

    fn pg_execute(&mut self, _sql: &str, _params: &[PgValue]) -> Result<u64, PgCapError> {
        unreachable!("lifecycle prototype nodes have no capabilities")
    }

    fn catalog_json(&mut self) -> Result<String, PgCapError> {
        unreachable!("lifecycle prototype nodes have no capabilities")
    }
}

struct EntryNode;

impl Node for EntryNode {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        _run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        Ok(Emission::main(input.clone()))
    }
}

struct FailNode;

impl Node for FailNode {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        run: &RunContext<'_>,
        _input: &Value,
    ) -> Result<Emission, NodeError> {
        let code = run
            .config
            .get("code")
            .and_then(Value::as_str)
            .expect("validated fail config has code");
        let message = run
            .config
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(code);
        Err(NodeError::Terminal(ErrorDetail::coded(code, message)))
    }
}

fn flow(source: &str) -> Flow {
    Flow::from_json(source).expect("prototype flow parses")
}

fn compile(flow: &Flow) -> Plan<'_> {
    let interfaces: ResolvedInterfaces = BTreeMap::from([
        (
            "choice".to_string(),
            vec![MAIN_PORT.to_string(), "drop".to_string()],
        ),
        ("cron".to_string(), vec![MAIN_PORT.to_string()]),
        ("event".to_string(), vec![MAIN_PORT.to_string()]),
        ("request".to_string(), vec![MAIN_PORT.to_string()]),
        ("respond".to_string(), vec![MAIN_PORT.to_string()]),
    ]);
    Plan::compile(flow, &interfaces).expect("prototype flow validates")
}

fn reserved(plan: &Plan<'_>, state: &mut ExecutionState) -> ReservedStep {
    match plan.next(state, 0) {
        Step::Reserved(step) => step,
        other => panic!("expected reserved step, got {other:?}"),
    }
}

fn run_context<'a>(
    state: &'a ExecutionState,
    node_id: &'a str,
    config: &'a Value,
) -> RunContext<'a> {
    RunContext {
        run_id: state.run_id(),
        flow_id: "prototype",
        flow_version: 1,
        node_id,
        attempt: 0,
        idempotency_key: "prototype:node:0",
        deadline_ms: None,
        traceparent: None,
        tracestate: None,
        config,
        context: state.context(),
    }
}

/// Prototype adapter: the ABI supplies the authored failure detail while the
/// engine remains authoritative for terminality, status, and caller release.
fn apply_fail_after_abi(
    plan: &Plan<'_>,
    state: &mut ExecutionState,
    step: &ReservedStep,
    node: &dyn Node,
    config: &Value,
) -> Result<(), &'static str> {
    let ReservedStep::Fail { code, message, .. } = step else {
        return Err("not a fail boundary");
    };
    let expected = ErrorDetail::coded(code, message.as_deref().unwrap_or(code));
    let mut ctx = NoEffects;
    let run = run_context(state, step.node(), config);
    match node.run(&mut ctx, &run, step.payload()) {
        Err(NodeError::Terminal(detail)) if detail == expected => plan
            .apply_reserved(state, step)
            .map_err(|_| "engine rejected fail lifecycle transition"),
        _ => Err("fail node did not return the authored terminal detail"),
    }
}

#[test]
fn request_data_crosses_node_abi_before_engine_advances_entry_and_keeps_caller_attached() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"request","version":1,
            "nodes":[
              {"id":"in","type":"request","config":{"input-schema":{}}},
              {"id":"out","type":"respond","config":{"status":200}}
            ],
            "edges":[{"from":"in","to":"out"}]}"#,
    );
    let plan = compile(&flow);
    let input = json!({"request": 1});
    let mut state = plan.start("request-run", input.clone());
    let dispatch = match plan.next(&mut state, 0) {
        Step::Dispatch(dispatch) => dispatch,
        other => panic!("expected request dispatch, got {other:?}"),
    };
    let mut ctx = NoEffects;
    let run = run_context(&state, &dispatch.node, &dispatch.config);
    let emission = EntryNode.run(&mut ctx, &run, &dispatch.payload).unwrap();
    plan.apply(
        &mut state,
        &dispatch,
        NodeOutcome::ok_on(emission.payload, emission.port),
        0,
    )
    .unwrap();

    assert_eq!(state.result(), &input);
    assert_eq!(state.caller_state(), CallerState::Attached);
    assert!(matches!(plan.next(&mut state, 0), Step::Dispatch(_)));
}

#[test]
fn failed_request_node_result_does_not_advance_the_entry_token_or_release_the_caller() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"request-failure","version":1,
            "nodes":[
              {"id":"in","type":"request","config":{"input-schema":{}}},
              {"id":"out","type":"respond","config":{"status":200}}
            ],
            "edges":[{"from":"in","to":"out"}]}"#,
    );
    let plan = compile(&flow);
    let mut state = plan.start("request-failure-run", json!({"request": 1}));
    let dispatch = match plan.next(&mut state, 0) {
        Step::Dispatch(dispatch) => dispatch,
        other => panic!("expected request dispatch, got {other:?}"),
    };

    plan.apply(
        &mut state,
        &dispatch,
        NodeOutcome::Error(NodeError::Terminal(ErrorDetail::coded(
            "request-failed",
            "request node failed",
        ))),
        0,
    )
    .unwrap();

    assert_eq!(state.step_seq(), 0);
    assert_eq!(state.caller_state(), CallerState::Attached);
    assert_eq!(state.status(), ExecutionStatus::Failed);
}

#[test]
fn cron_data_crosses_node_abi_before_engine_completes_callerless_run() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"cron","version":1,
            "nodes":[{"id":"in","type":"cron"}],"edges":[]}"#,
    );
    let plan = compile(&flow);
    let input = json!({"scheduled-at": "2026-08-03T12:00:00Z"});
    let mut state = plan.start("cron-run", input.clone());
    let dispatch = match plan.next(&mut state, 0) {
        Step::Dispatch(dispatch) => dispatch,
        other => panic!("expected cron dispatch, got {other:?}"),
    };
    let mut ctx = NoEffects;
    let run = run_context(&state, &dispatch.node, &dispatch.config);
    let emission = EntryNode.run(&mut ctx, &run, &dispatch.payload).unwrap();
    plan.apply(
        &mut state,
        &dispatch,
        NodeOutcome::ok_on(emission.payload, emission.port),
        0,
    )
    .unwrap();

    assert_eq!(state.result(), &input);
    assert_eq!(state.caller_state(), CallerState::None);
    assert_eq!(
        plan.next(&mut state, 0),
        Step::Done(ExecutionStatus::Completed)
    );
}

#[test]
fn failed_cron_node_result_does_not_advance_the_entry_token() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"cron-failure","version":1,
            "nodes":[{"id":"in","type":"cron"}],"edges":[]}"#,
    );
    let plan = compile(&flow);
    let mut state = plan.start("cron-failure-run", json!({"scheduled-at": 42}));
    let dispatch = match plan.next(&mut state, 0) {
        Step::Dispatch(dispatch) => dispatch,
        other => panic!("expected cron dispatch, got {other:?}"),
    };

    plan.apply(
        &mut state,
        &dispatch,
        NodeOutcome::Error(NodeError::Terminal(ErrorDetail::coded(
            "cron-failed",
            "cron node failed",
        ))),
        0,
    )
    .unwrap();

    assert_eq!(state.step_seq(), 0);
    assert_eq!(state.caller_state(), CallerState::None);
    assert_eq!(state.status(), ExecutionStatus::Failed);
}

#[test]
fn malformed_cron_emission_cannot_advance_the_entry_token() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"cron-guard","version":1,
            "nodes":[{"id":"in","type":"cron"}],"edges":[]}"#,
    );
    let plan = compile(&flow);
    let mut state = plan.start("cron-guard-run", json!({"scheduled-at": 42}));
    let dispatch = match plan.next(&mut state, 0) {
        Step::Dispatch(dispatch) => dispatch,
        other => panic!("expected cron dispatch, got {other:?}"),
    };

    assert_eq!(
        plan.apply(
            &mut state,
            &dispatch,
            NodeOutcome::ok(json!({"changed": true})),
            0,
        ),
        Err(wamn_runner::ApplyError::InvalidCronEmission)
    );
    assert_eq!(state.step_seq(), 0);
    assert_eq!(plan.next(&mut state, 0), Step::Dispatch(dispatch));
}

#[test]
fn event_data_crosses_node_abi_before_engine_completes_callerless_run() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"event","version":1,
            "nodes":[{"id":"in","type":"event"}],"edges":[]}"#,
    );
    let plan = compile(&flow);
    let input = json!({"topic": "orders.created", "id": 42});
    let mut state = plan.start("event-run", input.clone());
    let dispatch = match plan.next(&mut state, 0) {
        Step::Dispatch(dispatch) => dispatch,
        other => panic!("expected event dispatch, got {other:?}"),
    };
    assert_eq!(state.dispatched(), 1, "the event entry consumes one unit");
    let mut ctx = NoEffects;
    let run = run_context(&state, &dispatch.node, &dispatch.config);
    let emission = EntryNode.run(&mut ctx, &run, &dispatch.payload).unwrap();
    plan.apply(
        &mut state,
        &dispatch,
        NodeOutcome::ok_on(emission.payload, emission.port),
        0,
    )
    .unwrap();

    assert_eq!(state.result(), &input);
    assert_eq!(state.caller_state(), CallerState::None);
    assert_eq!(
        plan.next(&mut state, 0),
        Step::Done(ExecutionStatus::Completed)
    );
}

#[test]
fn failed_event_node_result_does_not_advance_the_entry_token() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"event-failure","version":1,
            "nodes":[{"id":"in","type":"event"}],"edges":[]}"#,
    );
    let plan = compile(&flow);
    let mut state = plan.start("event-failure-run", json!({"event": 42}));
    let dispatch = match plan.next(&mut state, 0) {
        Step::Dispatch(dispatch) => dispatch,
        other => panic!("expected event dispatch, got {other:?}"),
    };

    plan.apply(
        &mut state,
        &dispatch,
        NodeOutcome::Error(NodeError::Terminal(ErrorDetail::coded(
            "event-failed",
            "event node failed",
        ))),
        0,
    )
    .unwrap();

    assert_eq!(state.step_seq(), 0);
    assert_eq!(state.caller_state(), CallerState::None);
    assert_eq!(state.status(), ExecutionStatus::Failed);
}

#[test]
fn malformed_event_emission_cannot_advance_the_entry_token() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"event-guard","version":1,
            "nodes":[{"id":"in","type":"event"}],"edges":[]}"#,
    );
    let plan = compile(&flow);
    let mut state = plan.start("event-guard-run", json!({"event": 42}));
    let dispatch = match plan.next(&mut state, 0) {
        Step::Dispatch(dispatch) => dispatch,
        other => panic!("expected event dispatch, got {other:?}"),
    };

    assert_eq!(
        plan.apply(
            &mut state,
            &dispatch,
            NodeOutcome::ok(json!({"changed": true})),
            0,
        ),
        Err(wamn_runner::ApplyError::InvalidEventEmission)
    );
    assert_eq!(state.step_seq(), 0);
    assert_eq!(plan.next(&mut state, 0), Step::Dispatch(dispatch));
}

#[test]
fn fail_detail_crosses_node_abi_while_engine_owns_terminality_status_and_caller_release() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"fail","version":1,
            "nodes":[
              {"id":"in","type":"request","config":{"input-schema":{}}},
              {"id":"route","type":"choice"},
              {"id":"out","type":"respond","config":{"status":200}},
              {"id":"bad","type":"fail",
               "config":{"code":"denied","message":"not allowed","status":403}}
            ],
            "edges":[
              {"from":"in","to":"route"},
              {"from":"route","to":"out"},
              {"from":"route","from-port":"drop","to":"bad"}
            ]}"#,
    );
    let plan = compile(&flow);
    let mut state = plan.start("fail-run", json!({"request": 1}));
    let entry = match plan.next(&mut state, 0) {
        Step::Dispatch(dispatch) => dispatch,
        other => panic!("expected request dispatch, got {other:?}"),
    };
    let input = entry.payload.clone();
    plan.apply(&mut state, &entry, NodeOutcome::ok(input), 0)
        .unwrap();
    let route = match plan.next(&mut state, 0) {
        Step::Dispatch(dispatch) => dispatch,
        other => panic!("expected route dispatch, got {other:?}"),
    };
    plan.apply(
        &mut state,
        &route,
        NodeOutcome::ok_on(json!({"request": 1}), "drop"),
        0,
    )
    .unwrap();
    let fail = reserved(&plan, &mut state);
    assert!(matches!(&fail, ReservedStep::Fail { status: 403, .. }));

    apply_fail_after_abi(
        &plan,
        &mut state,
        &fail,
        &FailNode,
        &json!({"code": "denied", "message": "not allowed", "status": 403}),
    )
    .unwrap();

    assert_eq!(state.status(), ExecutionStatus::Failed);
    assert_eq!(state.caller_state(), CallerState::Released);
    assert_eq!(
        state.failure().unwrap().detail.code.as_deref(),
        Some("denied")
    );
}

#[test]
fn malformed_abi_results_cannot_authorize_engine_lifecycle_mutation() {
    struct MutatingEntry;

    impl Node for MutatingEntry {
        fn run(
            &self,
            _ctx: &mut dyn NodeCtx,
            _run: &RunContext<'_>,
            _input: &Value,
        ) -> Result<Emission, NodeError> {
            Ok(Emission::main(json!({"changed": true})))
        }
    }

    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"guard","version":1,
            "nodes":[
              {"id":"in","type":"request","config":{"input-schema":{}}},
              {"id":"out","type":"respond","config":{"status":200}}
            ],
            "edges":[{"from":"in","to":"out"}]}"#,
    );
    let plan = compile(&flow);
    let mut state = plan.start("guard-run", json!({"original": true}));
    let dispatch = match plan.next(&mut state, 0) {
        Step::Dispatch(dispatch) => dispatch,
        other => panic!("expected request dispatch, got {other:?}"),
    };
    let mut ctx = NoEffects;
    let run = run_context(&state, &dispatch.node, &dispatch.config);
    let emission = MutatingEntry
        .run(&mut ctx, &run, &dispatch.payload)
        .unwrap();
    assert_eq!(
        plan.apply(
            &mut state,
            &dispatch,
            NodeOutcome::ok_on(emission.payload, emission.port),
            0,
        ),
        Err(wamn_runner::ApplyError::InvalidRequestEmission)
    );
    assert_eq!(state.step_seq(), 0);
    assert_eq!(plan.next(&mut state, 0), Step::Dispatch(dispatch));
}

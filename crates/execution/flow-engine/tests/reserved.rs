use std::collections::BTreeMap;

use serde_json::{Value, json};
use wamn_flow::node_contract::{ErrorDetail, NodeError};
use wamn_flow::{Flow, ResolvedInterfaces};
use wamn_runner::{
    ApplyError, CallerState, EngineError, ExecutionStatus, NodeOutcome, Plan, ReservedStep, Step,
};

fn flow(source: &str) -> Flow {
    Flow::from_json(source).expect("fixture parses")
}

fn interfaces() -> ResolvedInterfaces {
    BTreeMap::from([
        ("echo".to_string(), vec!["main".to_string()]),
        ("event".to_string(), vec!["main".to_string()]),
        ("fail".to_string(), vec!["main".to_string()]),
        ("request".to_string(), vec!["main".to_string()]),
        ("respond".to_string(), vec!["main".to_string()]),
        (
            "choice".to_string(),
            vec!["main".to_string(), "drop".to_string()],
        ),
    ])
}

fn compile(flow: &Flow) -> Plan<'_> {
    Plan::compile(flow, &interfaces()).expect("fixture validates")
}

fn dispatch(plan: &Plan<'_>, state: &mut wamn_runner::ExecutionState) -> wamn_runner::Dispatch {
    match plan.next(state, 0) {
        Step::Dispatch(dispatch) => dispatch,
        other => panic!("expected dispatch, got {other:?}"),
    }
}

fn apply_request_entry(plan: &Plan<'_>, state: &mut wamn_runner::ExecutionState) {
    let request = dispatch(plan, state);
    assert_eq!(request.node_type, "request");
    let payload = request.payload.clone();
    plan.apply(state, &request, NodeOutcome::ok(payload), 0)
        .unwrap();
}

fn request_flow(respond_successor: &str) -> Flow {
    flow(&format!(
        r#"{{"schema-version":"0.1","flow-id":"request-flow","version":1,
             "nodes":[
               {{"id":"in","type":"request","config":{{"input-schema":{{}}}}}},
               {{"id":"work","type":"echo"}},
               {{"id":"out","type":"respond","config":{{"status":201}}}}
               {respond_successor}
             ],
             "edges":[{{"from":"in","to":"work"}},{{"from":"work","to":"out"}}
               {}
             ]}}"#,
        if respond_successor.is_empty() {
            ""
        } else {
            r#",{"from":"out","to":"after"}"#
        }
    ))
}

#[test]
fn event_node_emission_and_unwired_completion_port_exhaust_the_frontier() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"event","version":1,
            "nodes":[{"id":"in","type":"event"},{"id":"work","type":"choice"}],
            "edges":[{"from":"in","to":"work"}]}"#,
    );
    let plan = compile(&flow);
    let input = json!({"scheduled-at":"2026-01-01T00:00:00Z"});
    let mut state = plan.start("run", input.clone());
    let entry = dispatch(&plan, &mut state);
    assert_eq!(entry.node_type, "event");
    assert_eq!(entry.payload, input);
    assert_eq!(plan.next(&mut state, 0), Step::Dispatch(entry.clone()));
    let payload = entry.payload.clone();
    plan.apply(&mut state, &entry, NodeOutcome::ok(payload), 0)
        .unwrap();

    let work = dispatch(&plan, &mut state);
    plan.apply(
        &mut state,
        &work,
        NodeOutcome::ok_on(json!("dropped"), "drop"),
        0,
    )
    .unwrap();
    assert_eq!(
        plan.next(&mut state, 0),
        Step::Done(ExecutionStatus::Completed)
    );
    assert_eq!(state.caller_state(), CallerState::None);
}

#[test]
fn respond_releases_and_continues_then_late_fail_leaves_caller_untouched() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"continue-fail","version":1,
            "nodes":[
              {"id":"in","type":"request","config":{"input-schema":{}}},
              {"id":"out","type":"respond","config":{"status":202}},
              {"id":"late","type":"fail","config":{"code":"late-failure","status":500}}
            ],
            "edges":[{"from":"in","to":"out"},{"from":"out","to":"late"}]}"#,
    );
    let plan = compile(&flow);
    let mut state = plan.start("run", json!({"request":1}));
    apply_request_entry(&plan, &mut state);

    let respond = dispatch(&plan, &mut state);
    assert_eq!(respond.node_type, "respond");
    assert_eq!(respond.config, json!({"status": 202}));
    plan.apply(
        &mut state,
        &respond,
        NodeOutcome::ok(json!({"request":1})),
        0,
    )
    .unwrap();
    assert_eq!(state.caller_state(), CallerState::Released);

    let fail = dispatch(&plan, &mut state);
    assert_eq!(fail.node_type, "fail");
    plan.apply(
        &mut state,
        &fail,
        NodeOutcome::Error(NodeError::Terminal(ErrorDetail::coded(
            "late-failure",
            "late-failure",
        ))),
        0,
    )
    .unwrap();
    assert_eq!(state.status(), ExecutionStatus::Failed);
    assert_eq!(state.caller_state(), CallerState::Released);
}

#[test]
fn zero_successor_respond_is_a_release_and_complete_boundary() {
    let flow = request_flow("");
    let plan = compile(&flow);
    let mut state = plan.start("run", json!({"request": 1}));
    apply_request_entry(&plan, &mut state);
    let work = dispatch(&plan, &mut state);
    plan.apply(&mut state, &work, NodeOutcome::ok(json!({"answer":42})), 0)
        .unwrap();
    let respond = dispatch(&plan, &mut state);
    assert_eq!(respond.node_type, "respond");
    plan.apply(
        &mut state,
        &respond,
        NodeOutcome::ok(json!({"answer":42})),
        0,
    )
    .unwrap();
    assert_eq!(state.status(), ExecutionStatus::Completed);
    assert_eq!(state.caller_state(), CallerState::Released);
}

#[test]
fn fail_releases_an_attached_request_caller() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"failed","version":1,
            "nodes":[
              {"id":"in","type":"request","config":{"input-schema":{}}},
              {"id":"choice","type":"choice"},
              {"id":"out","type":"respond","config":{"status":200}},
              {"id":"bad","type":"fail","config":{"code":"denied","message":"no","status":403}}
            ],
            "edges":[
              {"from":"in","to":"choice"},{"from":"choice","to":"out"},
              {"from":"choice","from-port":"drop","to":"bad"}
            ]}"#,
    );
    let plan = compile(&flow);
    let mut state = plan.start("run", Value::Null);
    apply_request_entry(&plan, &mut state);
    let choice = dispatch(&plan, &mut state);
    plan.apply(
        &mut state,
        &choice,
        NodeOutcome::ok_on(json!({"reason":"denied"}), "drop"),
        0,
    )
    .unwrap();
    let fail = dispatch(&plan, &mut state);
    assert_eq!(fail.config["status"], 403);
    plan.apply(
        &mut state,
        &fail,
        NodeOutcome::Error(NodeError::Terminal(ErrorDetail::coded("denied", "no"))),
        0,
    )
    .unwrap();
    assert_eq!(state.status(), ExecutionStatus::Failed);
    assert_eq!(state.caller_state(), CallerState::Released);
    assert_eq!(
        state.dispatched(),
        3,
        "request, choice, and fail each count"
    );
}

#[test]
fn fail_refuses_non_terminal_or_mismatched_results_without_lifecycle_mutation() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"guarded-fail","version":1,
            "nodes":[{"id":"in","type":"event"},
                     {"id":"bad","type":"fail",
                      "config":{"code":"denied","message":"not allowed","status":403}}],
            "edges":[{"from":"in","to":"bad"}]}"#,
    );
    let plan = compile(&flow);
    let mut state = plan.start("run", json!({"tick": 1}));
    let entry = dispatch(&plan, &mut state);
    let input = entry.payload.clone();
    plan.apply(&mut state, &entry, NodeOutcome::ok(input), 0)
        .unwrap();
    let fail = dispatch(&plan, &mut state);

    for outcome in [
        NodeOutcome::ok(json!({"not": "terminal"})),
        NodeOutcome::Error(NodeError::Terminal(ErrorDetail::coded(
            "wrong",
            "not allowed",
        ))),
        NodeOutcome::Error(NodeError::InvalidInput(ErrorDetail::coded(
            "denied",
            "not allowed",
        ))),
    ] {
        let mut candidate = state.clone();
        assert_eq!(
            plan.apply(&mut candidate, &fail, outcome, 0),
            Err(ApplyError::InvalidFailOutcome)
        );
        assert_eq!(candidate, state, "refusal must not mutate lifecycle state");
    }
}

#[test]
fn validation_rejects_response_without_request_and_ambiguous_request_ports() {
    let event_respond = flow(
        r#"{"schema-version":"0.1","flow-id":"bad-response","version":1,
            "nodes":[{"id":"in","type":"event"},{"id":"out","type":"respond","config":{"status":200}}],
            "edges":[{"from":"in","to":"out"}]}"#,
    );
    assert!(matches!(
        Plan::compile(&event_respond, &interfaces()),
        Err(EngineError::Invalid(issues))
            if issues.iter().any(|issue| issue.code == "respond-without-request-entry")
    ));

    let ambiguous = flow(
        r#"{"schema-version":"0.1","flow-id":"ambiguous","version":1,
            "nodes":[
              {"id":"in","type":"request","config":{"input-schema":{}}},
              {"id":"choice","type":"choice"},
              {"id":"out","type":"respond","config":{"status":200}}
            ],
            "edges":[{"from":"in","to":"choice"},{"from":"choice","to":"out"}]}"#,
    );
    assert!(matches!(
        Plan::compile(&ambiguous, &interfaces()),
        Err(EngineError::Invalid(issues))
            if issues.iter().any(|issue| issue.code == "unanswered-port")
    ));
}

#[test]
fn post_terminal_reserved_writes_are_rejected() {
    let flow = request_flow("");
    let plan = compile(&flow);
    let mut state = plan.start("run", Value::Null);
    apply_request_entry(&plan, &mut state);
    for expected in ["work", "out"] {
        let node = dispatch(&plan, &mut state);
        assert_eq!(node.node, expected);
        plan.apply(&mut state, &node, NodeOutcome::ok(json!("answer")), 0)
            .unwrap();
    }
    assert_eq!(
        plan.next(&mut state, 0),
        Step::Done(ExecutionStatus::Completed)
    );
    let entry = ReservedStep::Entry {
        node: "in".to_string(),
        payload: Value::Null,
        occurrence: 0,
    };
    assert_eq!(
        plan.apply_reserved(&mut state, &entry),
        Err(ApplyError::Terminal(ExecutionStatus::Completed))
    );
}

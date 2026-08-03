use std::collections::BTreeMap;

use serde_json::{Value, json};
use wamn_flow::{Flow, ResolvedInterfaces};
use wamn_runner::{
    ApplyError, CallerState, EngineError, ErrorDetail, ExecutionStatus, NodeError, NodeOutcome,
    Plan, Recorded, ReservedStep, SeedError, Step,
};

fn flow(source: &str) -> Flow {
    Flow::from_json(source).expect("fixture parses")
}

fn interfaces() -> ResolvedInterfaces {
    BTreeMap::from([
        ("echo".to_string(), vec!["main".to_string()]),
        ("cron".to_string(), vec!["main".to_string()]),
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
fn cron_node_emission_and_unwired_completion_port_exhaust_the_frontier() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"cron","version":1,
            "nodes":[{"id":"in","type":"cron"},{"id":"work","type":"choice"}],
            "edges":[{"from":"in","to":"work"}]}"#,
    );
    let plan = compile(&flow);
    let input = json!({"scheduled-at":"2026-01-01T00:00:00Z"});
    let mut state = plan.start("run", input.clone());
    let entry = dispatch(&plan, &mut state);
    assert_eq!(entry.node_type, "cron");
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
    let completed = [
        Recorded::new("in", "main", json!({"request":1})),
        Recorded::new("work", "main", json!({"answer":42})),
    ];
    let mut state = plan
        .resume("run", json!({"request": 1}), &completed)
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
            "nodes":[{"id":"in","type":"cron"},
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
    let cron_respond = flow(
        r#"{"schema-version":"0.1","flow-id":"bad-response","version":1,
            "nodes":[{"id":"in","type":"cron"},{"id":"out","type":"respond","config":{"status":200}}],
            "edges":[{"from":"in","to":"out"}]}"#,
    );
    assert!(matches!(
        Plan::compile(&cron_respond, &interfaces()),
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
fn reserved_user_seed_and_post_terminal_writes_are_rejected() {
    let flow = request_flow("");
    let plan = compile(&flow);
    for node in ["in", "out"] {
        assert_eq!(
            plan.seed_at("rerun", node, Value::Null),
            Err(SeedError::ReservedNode(node.to_string()))
        );
    }

    let completed = [
        Recorded::new("in", "main", Value::Null),
        Recorded::new("work", "main", json!("answer")),
        Recorded::new("out", "main", json!("answer")),
    ];
    let mut state = plan.resume("run", Value::Null, &completed).unwrap();
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

#[test]
fn resume_rejects_history_missing_the_request_node_emission() {
    let flow = request_flow("");
    let plan = compile(&flow);
    let error = plan
        .resume(
            "run",
            json!({"request": 1}),
            &[Recorded::new("work", "main", json!({"answer": 42}))],
        )
        .unwrap_err();
    assert_eq!(
        error,
        wamn_runner::ResumeError::Mismatch {
            recorded: "work".to_string(),
            dispatched: "in".to_string(),
        }
    );
}

#[test]
fn crash_restart_redispatches_request_until_exact_emission_commits() {
    let flow = request_flow("");
    let plan = compile(&flow);
    let input = json!({"request": 1});
    let mut first = plan.start("run", input.clone());
    let boundary = dispatch(&plan, &mut first);
    assert_eq!(boundary.node_type, "request");
    assert_eq!(plan.next(&mut first, 0), Step::Dispatch(boundary.clone()));

    let mut restarted = plan.start("run", input.clone());
    assert_eq!(plan.next(&mut restarted, 0), Step::Dispatch(boundary));

    let mut committed = plan
        .resume(
            "run",
            input.clone(),
            &[Recorded::new("in", "main", input.clone())],
        )
        .unwrap();
    let next = dispatch(&plan, &mut committed);
    assert_eq!(next.node, "work");
    assert_eq!(next.payload, input);
    assert_eq!(committed.caller_state(), CallerState::Attached);
}

#[test]
fn crash_restart_redispatches_cron_until_exact_emission_commits() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"cron-replay","version":1,
            "nodes":[{"id":"in","type":"cron"},{"id":"work","type":"echo"}],
            "edges":[{"from":"in","to":"work"}]}"#,
    );
    let plan = compile(&flow);
    let input = json!({"scheduled-at": 42});
    let mut first = plan.start("run", input.clone());
    let boundary = dispatch(&plan, &mut first);
    assert_eq!(boundary.node_type, "cron");
    assert_eq!(plan.next(&mut first, 0), Step::Dispatch(boundary.clone()));

    let mut restarted = plan.start("run", input.clone());
    assert_eq!(plan.next(&mut restarted, 0), Step::Dispatch(boundary));

    let mut committed = plan
        .resume(
            "run",
            input.clone(),
            &[Recorded::new("in", "main", input.clone())],
        )
        .unwrap();
    let next = dispatch(&plan, &mut committed);
    assert_eq!(next.node, "work");
    assert_eq!(next.payload, input);
    assert_eq!(committed.caller_state(), CallerState::None);
}

#[test]
fn crash_restart_redispatches_event_until_exact_emission_commits() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"event-replay","version":1,
            "nodes":[{"id":"in","type":"event"},{"id":"work","type":"echo"}],
            "edges":[{"from":"in","to":"work"}]}"#,
    );
    let plan = compile(&flow);
    let input = json!({"topic": "orders.created", "id": 42});
    let mut first = plan.start("run", input.clone());
    let boundary = dispatch(&plan, &mut first);
    assert_eq!(boundary.node_type, "event");
    assert_eq!(plan.next(&mut first, 0), Step::Dispatch(boundary.clone()));

    let mut restarted = plan.start("run", input.clone());
    assert_eq!(plan.next(&mut restarted, 0), Step::Dispatch(boundary));

    let mut committed = plan
        .resume(
            "run",
            input.clone(),
            &[Recorded::new("in", "main", input.clone())],
        )
        .unwrap();
    let next = dispatch(&plan, &mut committed);
    assert_eq!(next.node, "work");
    assert_eq!(next.payload, input);
    assert_eq!(committed.caller_state(), CallerState::None);
}

#[test]
fn crash_restart_redispatches_respond_until_its_typed_emission_commits() {
    let flow = request_flow("");
    let plan = compile(&flow);
    let before = [
        Recorded::new("in", "main", json!({"request":1})),
        Recorded::new("work", "main", json!({"answer":42})),
    ];
    let mut first = plan.resume("run", json!({"request": 1}), &before).unwrap();
    let boundary = dispatch(&plan, &mut first);
    assert_eq!(boundary.node_type, "respond");
    assert_eq!(plan.next(&mut first, 0), Step::Dispatch(boundary.clone()));

    let mut restarted = plan.resume("run", json!({"request": 1}), &before).unwrap();
    assert_eq!(
        plan.next(&mut restarted, 0),
        Step::Dispatch(boundary.clone())
    );
    plan.apply(
        &mut restarted,
        &boundary,
        NodeOutcome::ok(json!({"answer":42})),
        0,
    )
    .unwrap();

    let after = [
        before[0].clone(),
        before[1].clone(),
        Recorded::new("out", "main", json!({"answer":42})),
    ];
    let mut committed = plan.resume("run", json!({"request": 1}), &after).unwrap();
    assert_eq!(
        plan.next(&mut committed, 0),
        Step::Done(ExecutionStatus::Completed)
    );
    assert_eq!(committed.caller_state(), CallerState::Released);
}

#[test]
fn crash_restart_replays_the_fail_boundary_until_its_record_commits() {
    let flow = flow(
        r#"{"schema-version":"0.1","flow-id":"cron-fail","version":1,
            "nodes":[{"id":"in","type":"cron"},{"id":"bad","type":"fail","config":{"code":"stop"}}],
            "edges":[{"from":"in","to":"bad"}]}"#,
    );
    let plan = compile(&flow);
    let input = json!({"tick": 1});
    let before = [Recorded::new("in", "main", input.clone())];
    let mut first = plan.resume("run", input.clone(), &before).unwrap();
    let boundary = dispatch(&plan, &mut first);
    assert_eq!(boundary.node_type, "fail");
    let mut restarted = plan.resume("run", input, &before).unwrap();
    assert_eq!(
        plan.next(&mut restarted, 0),
        Step::Dispatch(boundary.clone())
    );
    plan.apply(
        &mut restarted,
        &boundary,
        NodeOutcome::Error(NodeError::Terminal(ErrorDetail::coded("stop", "stop"))),
        0,
    )
    .unwrap();
    assert_eq!(restarted.status(), ExecutionStatus::Failed);
    assert_eq!(restarted.caller_state(), CallerState::None);
    assert_eq!(
        restarted.dispatched(),
        1,
        "only the live fail dispatch counts"
    );
}

//! RecoveryCheckpointV1 model and error-path tests.

use serde_json::{Value, json};
use wamn_flow::{Flow, ResolvedInterfaces};
use wamn_runner::{
    CallerState, CheckpointError, Dispatch, ExecutionState, ExecutionStatus, NodeError,
    NodeOutcome, Plan, RateLimitDetail, ReservedStep, Step, ThrottleKey, restore, snapshot,
};

fn compile(source: &str) -> (Flow, ResolvedInterfaces) {
    let flow = Flow::from_json(source).expect("fixture parses");
    let mut interfaces = ResolvedInterfaces::new();
    for node in &flow.nodes {
        if matches!(node.node_type.as_str(), "event" | "fail") {
            continue;
        }
        interfaces
            .entry(node.node_type.clone())
            .or_insert_with(|| vec!["main".to_string()]);
    }
    (flow, interfaces)
}

fn plan<'a>(flow: &'a Flow, interfaces: &'a ResolvedInterfaces) -> Plan<'a> {
    Plan::compile(flow, interfaces).expect("fixture validates")
}

fn apply_entry(plan: &Plan<'_>, state: &mut ExecutionState) {
    match plan.next(state, 0) {
        Step::Reserved(entry @ ReservedStep::Entry { .. }) => {
            plan.apply_reserved(state, &entry).expect("entry applies");
        }
        Step::Dispatch(entry) if matches!(entry.node_type.as_str(), "request" | "cron") => {
            let payload = entry.payload.clone();
            plan.apply(state, &entry, NodeOutcome::ok(payload), 0)
                .expect("Node-ABI entry applies");
        }
        other => panic!("fresh state must yield its entry boundary, got {other:?}"),
    }
}

fn dispatch(plan: &Plan<'_>, state: &mut ExecutionState, now_ms: u64) -> Dispatch {
    let Step::Dispatch(dispatch) = plan.next(state, now_ms) else {
        panic!("expected dispatch");
    };
    dispatch
}

#[test]
fn checkpoint_round_trips_multitoken_merge_and_occurrences() {
    let (flow, interfaces) = compile(
        r#"{"schema-version":"0.1","flow-id":"fan","version":1,
            "nodes":[{"id":"in","type":"cron"},{"id":"s","type":"echo"},
                     {"id":"a","type":"echo"},{"id":"b","type":"echo"},
                     {"id":"m","type":"echo"}],
            "edges":[{"from":"in","to":"s"},{"from":"s","to":"a"},
                     {"from":"s","to":"b"},{"from":"a","to":"m"},
                     {"from":"b","to":"m"}]}"#,
    );
    let plan = plan(&flow, &interfaces);
    let mut state = plan.start("run-merge", json!({"seed": true}));
    apply_entry(&plan, &mut state);

    let s = dispatch(&plan, &mut state, 0);
    plan.apply(&mut state, &s, NodeOutcome::ok(json!({"from": "s"})), 0)
        .unwrap();
    let a = dispatch(&plan, &mut state, 0);
    plan.apply(&mut state, &a, NodeOutcome::ok(json!({"from": "a"})), 0)
        .unwrap();
    let b = dispatch(&plan, &mut state, 0);
    plan.apply(&mut state, &b, NodeOutcome::ok(json!({"from": "b"})), 0)
        .unwrap();

    let encoded = snapshot(&state).expect("snapshot encodes");
    assert_eq!(encoded, snapshot(&state).expect("repeat snapshot encodes"));
    let mut restored = restore(&plan, "run-merge", &encoded).expect("checkpoint restores");
    assert_eq!(snapshot(&restored).unwrap(), encoded);
    assert_eq!(
        restored.dispatched(),
        0,
        "a reclaim gets a fresh dispatch budget"
    );

    let first_merge = dispatch(&plan, &mut restored, 0);
    assert_eq!(first_merge.node, "m");
    assert_eq!(first_merge.occurrence, 0);
    assert_eq!(first_merge.payload, json!({"from": "a"}));
    plan.apply(
        &mut restored,
        &first_merge,
        NodeOutcome::ok(json!({"merged": 1})),
        0,
    )
    .unwrap();

    let second_merge = dispatch(&plan, &mut restored, 0);
    assert_eq!(second_merge.node, "m");
    assert_eq!(second_merge.occurrence, 1);
    assert_eq!(second_merge.payload, json!({"from": "b"}));
}

#[test]
fn checkpoint_round_trips_loop_after_error_route() {
    let (flow, interfaces) = compile(
        r#"{"schema-version":"0.1","flow-id":"error-loop","version":1,
            "nodes":[{"id":"in","type":"cron"},{"id":"work","type":"call"},
                     {"id":"handle","type":"handler"}],
            "edges":[{"from":"in","to":"work"},
                     {"from":"work","from-port":"error","to":"handle"},
                     {"from":"handle","to":"work"}]}"#,
    );
    let plan = plan(&flow, &interfaces);
    let mut state = plan.start("run-loop", json!({"seed": 1}));
    apply_entry(&plan, &mut state);

    let work = dispatch(&plan, &mut state, 0);
    plan.apply(
        &mut state,
        &work,
        NodeOutcome::Error(NodeError::Terminal(wamn_runner::ErrorDetail::msg("caught"))),
        0,
    )
    .unwrap();
    let handle = dispatch(&plan, &mut state, 0);
    plan.apply(
        &mut state,
        &handle,
        NodeOutcome::ok_with_context(json!({"retry": true}), "main", json!({"handled": 1})),
        0,
    )
    .unwrap();

    let encoded = snapshot(&state).unwrap();
    let mut restored = restore(&plan, "run-loop", &encoded).unwrap();
    assert_eq!(restored.context(), &json!({"handled": 1}));
    assert_eq!(snapshot(&restored).unwrap(), encoded);
    let repeated = dispatch(&plan, &mut restored, 0);
    assert_eq!((repeated.node.as_str(), repeated.occurrence), ("work", 1));
    assert_eq!(repeated.payload, json!({"retry": true}));
}

#[test]
fn checkpoint_round_trips_parked_retry_state() {
    let (flow, interfaces) = compile(
        r#"{"schema-version":"0.1","flow-id":"parked","version":1,
            "nodes":[{"id":"in","type":"cron"},
                     {"id":"call","type":"http-call","credential":"erp"}],
            "edges":[{"from":"in","to":"call"}],
            "credentials":[{"name":"erp"}]}"#,
    );
    let plan = plan(&flow, &interfaces);
    let mut state = plan.start("run-parked", json!({"request": 7}));
    apply_entry(&plan, &mut state);
    let call = dispatch(&plan, &mut state, 7);
    plan.apply(
        &mut state,
        &call,
        NodeOutcome::Error(NodeError::RateLimited(RateLimitDetail {
            detail: wamn_runner::ErrorDetail::msg("slow down"),
            retry_after_ms: Some(500),
            target_host: Some("erp.example".to_string()),
        })),
        7,
    )
    .unwrap();
    let expected = Step::Wait {
        node: "call".to_string(),
        until_ms: 507,
        attempt: 1,
        throttle: Some(ThrottleKey::new(
            "http-call",
            Some("erp".to_string()),
            Some("erp.example".to_string()),
        )),
    };
    assert_eq!(plan.next(&mut state, 7), expected);

    let encoded = snapshot(&state).unwrap();
    let mut restored = restore(&plan, "run-parked", &encoded).unwrap();
    assert_eq!(snapshot(&restored).unwrap(), encoded);
    assert_eq!(plan.next(&mut restored, 7), expected);
}

#[test]
fn checkpoint_round_trips_released_caller_context_and_last_result() {
    let (flow, interfaces) = compile(
        r#"{"schema-version":"0.1","flow-id":"caller","version":1,
            "nodes":[{"id":"in","type":"request","config":{"input-schema":{}}},
                     {"id":"work","type":"echo"},
                     {"id":"out","type":"respond","config":{"status":202}},
                     {"id":"after","type":"echo"}],
            "edges":[{"from":"in","to":"work"},{"from":"work","to":"out"},
                     {"from":"out","to":"after"}]}"#,
    );
    let plan = plan(&flow, &interfaces);
    let mut state = plan.start("run-caller", json!({"request": 1}));
    apply_entry(&plan, &mut state);
    let work = dispatch(&plan, &mut state, 0);
    plan.apply(
        &mut state,
        &work,
        NodeOutcome::ok_with_context(json!({"answer": 42}), "main", json!({"trace": "kept"})),
        0,
    )
    .unwrap();
    let response = dispatch(&plan, &mut state, 0);
    assert_eq!(response.node_type, "respond");
    plan.apply(
        &mut state,
        &response,
        NodeOutcome::ok(json!({"answer": 42})),
        0,
    )
    .unwrap();

    let encoded = snapshot(&state).unwrap();
    let mut restored = restore(&plan, "run-caller", &encoded).unwrap();
    assert_eq!(restored.caller_state(), CallerState::Released);
    assert_eq!(restored.context(), &json!({"trace": "kept"}));
    assert_eq!(restored.result(), &json!({"answer": 42}));
    assert_eq!(snapshot(&restored).unwrap(), encoded);
    assert_eq!(dispatch(&plan, &mut restored, 0).node, "after");
}

#[test]
fn checkpoint_rejects_unknown_versions_without_fallback() {
    let (flow, interfaces) = compile(
        r#"{"schema-version":"0.1","flow-id":"version","version":1,
            "nodes":[{"id":"in","type":"cron"}],"edges":[]}"#,
    );
    let plan = plan(&flow, &interfaces);
    let state = plan.start("run-version", Value::Null);
    let mut encoded: Value = serde_json::from_str(&snapshot(&state).unwrap()).unwrap();
    encoded["version"] = json!(2);

    assert_eq!(
        restore(&plan, "run-version", &encoded.to_string()),
        Err(CheckpointError::UnsupportedVersion { version: 2 })
    );
}

#[test]
fn checkpoint_v1_encoding_is_exact_and_environment_independent() {
    let (flow, interfaces) = compile(
        r#"{"schema-version":"0.1","flow-id":"golden","version":1,
            "nodes":[{"id":"in","type":"cron"}],"edges":[]}"#,
    );
    let plan = plan(&flow, &interfaces);
    let state = plan.start("run-golden", json!({"input": 1}));

    assert_eq!(
        snapshot(&state).unwrap(),
        r#"{"version":1,"frontier":[{"node":"in","payload":{"kind":"inline","value":{"input":1}}}],"current":null,"visits":{},"step-seq":0,"context":{},"result":null,"caller":"none"}"#
    );
}

#[test]
fn checkpoint_rejects_invalid_shape_context_and_nodes() {
    let (flow, interfaces) = compile(
        r#"{"schema-version":"0.1","flow-id":"invalid","version":1,
            "nodes":[{"id":"in","type":"cron"},{"id":"work","type":"echo"}],
            "edges":[{"from":"in","to":"work"}]}"#,
    );
    let plan = plan(&flow, &interfaces);
    let mut state = plan.start("run-invalid", Value::Null);
    apply_entry(&plan, &mut state);
    let encoded = snapshot(&state).unwrap();
    let mut value: Value = serde_json::from_str(&encoded).unwrap();
    value["context"] = json!(["not", "an", "object"]);
    assert_eq!(
        restore(&plan, "run-invalid", &value.to_string()),
        Err(CheckpointError::InvalidContext)
    );

    let mut value: Value = serde_json::from_str(&encoded).unwrap();
    value["frontier"][0]["node"] = json!("missing");
    assert_eq!(
        restore(&plan, "run-invalid", &value.to_string()),
        Err(CheckpointError::UnknownNode {
            node: "missing".to_string()
        })
    );

    let mut value: Value = serde_json::from_str(&encoded).unwrap();
    value["capture"] = json!({"output": "forbidden"});
    assert!(matches!(
        restore(&plan, "run-invalid", &value.to_string()),
        Err(CheckpointError::InvalidEncoding { .. })
    ));
}

#[test]
fn checkpoint_rejects_terminal_state() {
    let (flow, interfaces) = compile(
        r#"{"schema-version":"0.1","flow-id":"terminal","version":1,
            "nodes":[{"id":"in","type":"cron"}],"edges":[]}"#,
    );
    let plan = plan(&flow, &interfaces);
    let mut state = plan.start("run-terminal", Value::Null);
    apply_entry(&plan, &mut state);
    assert_eq!(
        plan.next(&mut state, 0),
        Step::Done(ExecutionStatus::Completed)
    );
    assert_eq!(
        snapshot(&state),
        Err(CheckpointError::TerminalState {
            status: ExecutionStatus::Completed
        })
    );
}

#[test]
fn checkpoint_visit_map_is_encoded_in_stable_key_order() {
    let (flow, interfaces) = compile(
        r#"{"schema-version":"0.1","flow-id":"stable","version":1,
            "nodes":[{"id":"in","type":"cron"},{"id":"z","type":"echo"},
                     {"id":"a","type":"echo"}],
            "edges":[{"from":"in","to":"z"},{"from":"z","to":"a"}]}"#,
    );
    let plan = plan(&flow, &interfaces);
    let mut state = plan.start("run-stable", Value::Null);
    apply_entry(&plan, &mut state);
    for expected in ["z", "a"] {
        let next = dispatch(&plan, &mut state, 0);
        assert_eq!(next.node, expected);
        plan.apply(&mut state, &next, NodeOutcome::ok(Value::Null), 0)
            .unwrap();
    }

    let encoded = snapshot(&state).unwrap();
    assert!(
        encoded.contains(r#""visits":{"a":1,"in":1,"z":1}"#),
        "visit keys must be stable regardless of HashMap iteration: {encoded}"
    );
}

use serde_json::{Value, json};
use wamn_flow::{Flow, ResolvedInterfaces};
use wamn_runner::{ApplyError, NodeOutcome, Plan, Recorded, ResumeError, Step};

fn plan(source: &str) -> Plan<'static> {
    let flow = Box::leak(Box::new(Flow::from_json(source).unwrap()));
    let interfaces = Box::leak(Box::new(ResolvedInterfaces::from([
        (
            "pure".to_string(),
            vec!["main".to_string(), "again".to_string()],
        ),
        ("cron".to_string(), vec!["main".to_string()]),
        ("effect".to_string(), vec!["main".to_string()]),
    ])));
    Plan::compile(flow, interfaces).unwrap()
}

fn acknowledge_entry(plan: &Plan<'_>, state: &mut wamn_runner::ExecutionState) {
    let Step::Dispatch(entry) = plan.next(state, 0) else {
        panic!("cron entry must dispatch");
    };
    assert_eq!(entry.node_type, "cron");
    let payload = entry.payload.clone();
    plan.apply(state, &entry, NodeOutcome::ok(payload), 0)
        .unwrap();
}

fn dispatch(plan: &Plan<'_>, state: &mut wamn_runner::ExecutionState) -> wamn_runner::Dispatch {
    let Step::Dispatch(dispatch) = plan.next(state, 0) else {
        panic!("expected dispatch");
    };
    dispatch
}

#[test]
fn successful_context_writes_replace_and_effects_observe_the_snapshot() {
    let plan = plan(
        r#"{"schema-version":"0.1","flow-id":"ctx","version":1,
            "nodes":[
              {"id":"in","type":"cron"},
              {"id":"first","type":"pure"},
              {"id":"second","type":"pure"},
              {"id":"effect","type":"effect"}],
            "edges":[
              {"from":"in","to":"first"},
              {"from":"first","to":"second"},
              {"from":"second","to":"effect"}]}"#,
    );
    let mut state = plan.start("run", json!({"tick": 1}));
    acknowledge_entry(&plan, &mut state);

    let first = dispatch(&plan, &mut state);
    assert_eq!(first.context, json!({}));
    plan.apply(
        &mut state,
        &first,
        NodeOutcome::ok_with_context(json!({"output": 1}), "main", json!({"dropped": true})),
        0,
    )
    .unwrap();

    let second = dispatch(&plan, &mut state);
    assert_eq!(second.context, json!({"dropped": true}));
    plan.apply(
        &mut state,
        &second,
        NodeOutcome::ok_with_context(json!({"output": 2}), "main", json!({"kept": true})),
        0,
    )
    .unwrap();

    let effect = dispatch(&plan, &mut state);
    assert_eq!(effect.context, json!({"kept": true}));
    assert!(
        effect.context.get("dropped").is_none(),
        "writes must not merge"
    );
}

#[test]
fn pure_replay_reconstructs_context_byte_identically_through_a_loop() {
    let plan = plan(
        r#"{"schema-version":"0.1","flow-id":"loop-ctx","version":1,
            "nodes":[
              {"id":"in","type":"cron"},
              {"id":"loop-a","type":"pure"},
              {"id":"loop-b","type":"pure"},
              {"id":"effect","type":"effect"}],
            "edges":[
              {"from":"in","to":"loop-a"},
              {"from":"loop-a","from-port":"again","to":"loop-b"},
              {"from":"loop-b","to":"loop-a"},
              {"from":"loop-a","to":"effect"}]}"#,
    );
    let input = json!({"tick": 1});
    let history = [
        Recorded::new("in", "main", input.clone()),
        Recorded::new("loop-a", "again", json!({"lap": 1}))
            .with_context(json!({"cutoff": "2026-01-01T00:00:00Z", "lap": 1})),
        Recorded::new("loop-b", "main", json!({"lap": 1}))
            .with_context(json!({"cutoff": "2026-01-01T00:00:00Z", "lap": 1})),
        Recorded::new("loop-a", "main", json!({"lap": 2}))
            .with_context(json!({"cutoff": "2026-01-01T00:00:00Z", "lap": 2})),
    ];
    let mut recovered = plan.resume("run", input, &history).unwrap();
    let effect = dispatch(&plan, &mut recovered);
    let expected = json!({"cutoff": "2026-01-01T00:00:00Z", "lap": 2});
    assert_eq!(effect.context, expected);
    assert_eq!(
        serde_json::to_vec(&effect.context).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
}

#[test]
fn invalid_replacement_is_atomic_and_error_records_cannot_mutate_context() {
    let plan = plan(
        r#"{"schema-version":"0.1","flow-id":"negative-ctx","version":1,
            "nodes":[{"id":"in","type":"cron"},{"id":"pure","type":"pure"}],
            "edges":[{"from":"in","to":"pure"}]}"#,
    );
    let mut state = plan.start("run", Value::Null);
    acknowledge_entry(&plan, &mut state);
    let node = dispatch(&plan, &mut state);
    let error = plan
        .apply(
            &mut state,
            &node,
            NodeOutcome::ok_with_context(json!({"output": 1}), "main", json!(["merge"])),
            0,
        )
        .unwrap_err();
    assert!(matches!(error, ApplyError::InvalidContext(_)));
    assert_eq!(state.context(), &json!({}));
    assert_eq!(dispatch(&plan, &mut state), node);

    let replay = [
        Recorded::new("in", "main", Value::Null),
        Recorded::new("pure", "error", json!({"error": {"message": "boom"}}))
            .with_context(json!({"forbidden": true})),
    ];
    assert!(matches!(
        plan.resume("run", Value::Null, &replay),
        Err(ResumeError::ErrorContext { .. })
    ));
}

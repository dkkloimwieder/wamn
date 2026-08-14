//! T-CTX package-level proofs for single-shot callable-flow context.

#[cfg(test)]
pub mod tests {
    use serde_json::{Value, json};
    use wamn_flow::{Flow, ResolvedInterfaces};
    use wamn_runner::{NodeOutcome, Plan, Step};

    fn plan() -> Plan<'static> {
        let flow = Box::leak(Box::new(
            Flow::from_json(
                r#"{"schema-version":"0.1","flow-id":"t-ctx","version":1,
                    "nodes":[
                      {"id":"in","type":"event"},
                      {"id":"mark","type":"pure"},
                      {"id":"effect","type":"effect"}],
                    "edges":[
                      {"from":"in","to":"mark"},
                      {"from":"mark","to":"effect"}]}"#,
            )
            .unwrap(),
        ));
        let interfaces = Box::leak(Box::new(ResolvedInterfaces::from([
            ("pure".to_string(), vec!["main".to_string()]),
            ("effect".to_string(), vec!["main".to_string()]),
        ])));
        Plan::compile(flow, interfaces).unwrap()
    }

    #[test]
    fn effect_input_keeps_the_exact_context_snapshot() {
        let plan = plan();
        let input = json!({"scheduled-at": "2026-07-27T12:00:00Z"});
        let context = json!({"cutoff": "2026-07-26T12:00:00Z", "hold": {"id": 7}});
        let mut state = plan.start("run", input);
        let Step::Dispatch(entry) = plan.next(&mut state, 0) else {
            panic!("entry must dispatch");
        };
        let admitted = entry.payload.clone();
        plan.apply(&mut state, &entry, NodeOutcome::ok(admitted), 0)
            .unwrap();
        let Step::Dispatch(mark) = plan.next(&mut state, 0) else {
            panic!("mark must dispatch");
        };
        plan.apply(
            &mut state,
            &mark,
            NodeOutcome::ok_with_context(json!({"selected": 7}), "main", context.clone()),
            0,
        )
        .unwrap();
        let Step::Dispatch(effect) = plan.next(&mut state, 0) else {
            panic!("effect must remain outstanding");
        };
        assert_eq!(
            serde_json::to_vec(&effect.context).unwrap(),
            serde_json::to_vec(&context).unwrap()
        );
        assert_eq!(effect.attempt_input()["context"], context);
    }

    #[test]
    fn invalid_context_is_detected_before_mutating_payload_progress() {
        let plan = plan();
        let mut state = plan.start("run", Value::Null);
        // The `event` entry executes through standard-node dispatch:
        // it dispatches, and may only re-emit its admitted input unchanged.
        let Step::Dispatch(entry) = plan.next(&mut state, 0) else {
            panic!("event entry must dispatch");
        };
        let admitted = entry.payload.clone();
        plan.apply(&mut state, &entry, NodeOutcome::ok(admitted), 0)
            .unwrap();
        let Step::Dispatch(mark) = plan.next(&mut state, 0) else {
            panic!("mark must dispatch");
        };
        assert!(
            plan.apply(
                &mut state,
                &mark,
                NodeOutcome::ok_with_context(json!({"selected": 7}), "main", json!(null)),
                0,
            )
            .is_err()
        );
        assert_eq!(state.context(), &json!({}));
        assert_eq!(plan.next(&mut state, 0), Step::Dispatch(mark));
    }
}

//! T-CTX package-level proofs for durable callable-flow context.

#[cfg(test)]
pub mod tests {
    use serde_json::{Value, json};
    use wamn_flow::{Flow, ResolvedInterfaces};
    use wamn_run_state::context;
    use wamn_run_state::transitions::node_context_checkpoint_sql;
    use wamn_runner::{NodeOutcome, Plan, Recorded, Step};

    fn plan() -> Plan<'static> {
        let flow = Box::leak(Box::new(
            Flow::from_json(
                r#"{"schema-version":"0.1","flow-id":"t-ctx","version":1,
                    "nodes":[
                      {"id":"in","type":"cron"},
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
    fn boundary_recovery_is_byte_identical_and_effect_input_keeps_context_snapshot() {
        let plan = plan();
        let input = json!({"scheduled-at": "2026-07-27T12:00:00Z"});
        let context = json!({"cutoff": "2026-07-26T12:00:00Z", "hold": {"id": 7}});
        let history = [
            Recorded::new("in", "main", input.clone()),
            Recorded::new("mark", "main", json!({"selected": 7})).with_context(context.clone()),
        ];
        let mut recovered = plan.resume("run", input, &history).unwrap();
        let Step::Dispatch(effect) = plan.next(&mut recovered, 0) else {
            panic!("effect must remain outstanding");
        };
        assert_eq!(
            serde_json::to_vec(&effect.context).unwrap(),
            serde_json::to_vec(&context).unwrap()
        );
        assert_eq!(effect.attempt_input()["context"], context);
    }

    #[test]
    fn later_write_replaces_without_implicit_merge() {
        let state = context::replace(None, json!({"old": true})).unwrap();
        let state = context::replace(Some(state), json!({"new": true})).unwrap();
        assert_eq!(context::read(Some(&state)).unwrap(), json!({"new": true}));
    }

    #[test]
    fn stale_fence_guards_output_and_context_in_the_same_statement() {
        let sql = node_context_checkpoint_sql();
        let authority = sql
            .find("q.lease_generation IS DISTINCT FROM i.lease_generation")
            .unwrap();
        let output = sql.find("INSERT INTO node_runs").unwrap();
        let context = sql.find("SET state_json = jsonb_set").unwrap();
        assert!(authority < output);
        assert!(output < context);
        assert!(sql.contains("q.lease_generation IS DISTINCT FROM i.lease_generation"));
        assert!(sql.contains("$13::text::jsonb"));
        assert!(sql.contains("(SELECT count(*) FROM recorded) = 1"));
        assert!(!sql.contains("UPDATE runs SET state_json = $"));
    }

    #[test]
    fn invalid_context_is_detected_before_mutating_payload_progress() {
        let plan = plan();
        let mut state = plan.start("run", Value::Null);
        // Since wamn-ayq7.23 the `cron` entry executes through the node ABI:
        // it dispatches, and may only re-emit its admitted input unchanged.
        let Step::Dispatch(entry) = plan.next(&mut state, 0) else {
            panic!("cron entry must dispatch");
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

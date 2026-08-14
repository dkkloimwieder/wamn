//! Engine tests — the whole execution model exercised with NO cluster, NO DB, NO
//! wasm: build a `wamn_flow::Flow`, compile a `Plan`, and drive it with a
//! programmable node dispatcher, asserting the walk / branch / merge / error /
//! retry / throttle behavior purely from returned `Step`s and final `ExecutionState`.

use std::cell::Cell;

use serde_json::{Value, json};
use wamn_flow::node_contract::{ErrorDetail, NodeError, RateLimitDetail};
use wamn_flow::{Flow, ResolvedInterfaces};
use wamn_runner::{
    ConcurrencyGate, Dispatch, EngineError, ExecutionFailureKind, ExecutionState, ExecutionStatus,
    NodeOutcome, Plan, RetryPolicy, Step, ThrottleKey, ThrottleTable,
};

/// A recorded drive of one run to a terminal status.
struct Trace {
    /// Every `Dispatch`, in order.
    visited: Vec<Dispatch>,
    /// Every `Wait` as `(node, until_ms, throttle)`, in order.
    waits: Vec<(String, u64, Option<ThrottleKey>)>,
    status: ExecutionStatus,
    state: ExecutionState,
}

impl Trace {
    /// Node ids dispatched, in order.
    fn nodes(&self) -> Vec<&str> {
        self.visited.iter().map(|d| d.node.as_str()).collect()
    }
}

/// Drive a run: a `Wait` "sleeps" by jumping a virtual clock to the deadline; a
/// `Dispatch` calls `dispatch_fn`. Records the whole trace.
fn run(
    plan: &Plan,
    run_id: &str,
    input: Value,
    mut dispatch_fn: impl FnMut(&Dispatch) -> NodeOutcome,
) -> Trace {
    let clock = Cell::new(0u64);
    let mut visited = Vec::new();
    let mut waits = Vec::new();
    let mut st = started(plan, run_id, input);
    let status = loop {
        match plan.next(&mut st, clock.get()) {
            Step::Done(s) => break s,
            Step::Wait {
                node,
                until_ms,
                throttle,
                ..
            } => {
                waits.push((node, until_ms, throttle));
                clock.set(until_ms); // virtual sleep
            }
            Step::Reserved(step) => {
                plan.apply_reserved(&mut st, &step).unwrap();
            }
            Step::Dispatch(d) => {
                visited.push(d.clone());
                let outcome = dispatch_fn(&d);
                plan.apply(&mut st, &d, outcome, clock.get()).unwrap();
            }
        }
    };
    Trace {
        visited,
        waits,
        status,
        state: st,
    }
}

fn flow(json_str: &str) -> Flow {
    let mut value: Value = serde_json::from_str(json_str).expect("fixture JSON parses");
    let object = value.as_object_mut().expect("fixture is a JSON object");
    let entry = object
        .remove("entry")
        .and_then(|value| value.as_str().map(str::to_string));
    object.remove("trigger");
    if let Some(entry) = entry {
        let nodes = object["nodes"].as_array_mut().expect("nodes array");
        for node in nodes.iter_mut() {
            if node["type"] == "respond" {
                node["type"] = json!("echo");
            }
        }
        nodes.insert(0, json!({"id":"entry","type":"event"}));
        object["edges"]
            .as_array_mut()
            .expect("edges array")
            .insert(0, json!({"from":"entry","to":entry}));
    }
    serde_json::from_value(value).expect("fixture flow parses")
}

fn compile(flow: &Flow) -> Result<Plan<'_>, EngineError> {
    let mut interfaces = ResolvedInterfaces::new();
    for node in &flow.nodes {
        if matches!(node.node_type.as_str(), "request" | "fail") {
            continue;
        }
        let ports = interfaces.entry(node.node_type.clone()).or_default();
        if !ports.iter().any(|port| port == "main") {
            ports.push("main".to_string());
        }
        for edge in flow.edges.iter().filter(|edge| edge.from == node.id) {
            if edge.from_port != "error" && !ports.contains(&edge.from_port) {
                ports.push(edge.from_port.clone());
            }
        }
    }
    Plan::compile(flow, &interfaces)
}

fn started(plan: &Plan<'_>, run_id: impl Into<String>, input: Value) -> ExecutionState {
    let mut state = plan.start(run_id, input);
    let Step::Dispatch(entry) = plan.next(&mut state, 0) else {
        panic!("fresh run must dispatch its event entry");
    };
    assert_eq!(entry.node_type, "event");
    let payload = entry.payload.clone();
    plan.apply(&mut state, &entry, NodeOutcome::ok(payload), 0)
        .unwrap();
    state
}

// ---- walk: linear / branch / merge / fan-out ------------------------------

#[test]
fn linear_walk_completes_in_order() {
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"lin","version":1,
            "trigger":{"type":"manual"},"entry":"a",
            "nodes":[{"id":"a","type":"echo"},{"id":"b","type":"echo"},{"id":"c","type":"echo"}],
            "edges":[{"from":"a","to":"b"},{"from":"b","to":"c"}]}"#,
    );
    let plan = compile(&f).unwrap();
    // Each node emits a payload naming itself, so the result is the last node's.
    let t = run(&plan, "r1", json!({ "seen": [] }), |d| {
        NodeOutcome::ok(json!({ "at": d.node }))
    });
    assert_eq!(t.status, ExecutionStatus::Completed);
    assert_eq!(t.nodes(), ["a", "b", "c"]);
    assert_eq!(t.state.step_seq(), 4); // event entry + three downstream nodes
    assert_eq!(t.state.result(), &json!({ "at": "c" }));
    // Each node's input payload is the upstream node's output.
    assert_eq!(t.visited[0].payload, json!({ "seen": [] })); // entry gets the trigger payload
    assert_eq!(t.visited[1].payload, json!({ "at": "a" })); // b sees a's output
    assert_eq!(t.visited[2].payload, json!({ "at": "b" })); // c sees b's output
}

#[test]
fn branch_follows_only_the_selected_port() {
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"br","version":1,
            "trigger":{"type":"manual"},"entry":"cond",
            "nodes":[{"id":"cond","type":"conditional"},{"id":"yes","type":"echo"},{"id":"no","type":"echo"}],
            "edges":[{"from":"cond","from-port":"true","to":"yes"},
                     {"from":"cond","from-port":"false","to":"no"}]}"#,
    );
    let plan = compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |d| match d.node.as_str() {
        "cond" => NodeOutcome::ok_on(json!({ "picked": true }), "true"),
        _ => NodeOutcome::ok(json!({ "at": d.node })),
    });
    assert_eq!(t.status, ExecutionStatus::Completed);
    assert_eq!(t.nodes(), ["cond", "yes"]); // "no" never runs
}

#[test]
fn fan_out_and_merge_without_a_join_barrier() {
    // s fans out on main to a and b; both edge into m -> m runs once per arrival.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"fan","version":1,
            "trigger":{"type":"manual"},"entry":"s",
            "nodes":[{"id":"s","type":"echo"},{"id":"a","type":"echo"},
                     {"id":"b","type":"echo"},{"id":"m","type":"echo"}],
            "edges":[{"from":"s","to":"a"},{"from":"s","to":"b"},
                     {"from":"a","to":"m"},{"from":"b","to":"m"}]}"#,
    );
    let plan = compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |d| {
        NodeOutcome::ok(json!({ "at": d.node }))
    });
    assert_eq!(t.status, ExecutionStatus::Completed);
    // BFS order: s, then a, b, then m (from a), m (from b).
    assert_eq!(t.nodes(), ["s", "a", "b", "m", "m"]);
    assert_eq!(t.state.step_seq(), 6); // event entry + five downstream visits
}

#[test]
fn fan_out_order_follows_the_explicit_edge_ordinal() {
    // The runtime half of W2 digest ordering (wamn-jvzx.15): fan-out order is the
    // explicit `Edge::ordinal`, not the edge's position in the array. Here `s`
    // declares a before b but ordinals say b first, so b must run first.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"fan-ordinal","version":1,
            "trigger":{"type":"manual"},"entry":"s",
            "nodes":[{"id":"s","type":"echo"},{"id":"a","type":"echo"},
                     {"id":"b","type":"echo"}],
            "edges":[{"from":"s","to":"a","ordinal":1},
                     {"from":"s","to":"b","ordinal":0}]}"#,
    );
    let plan = compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |d| {
        NodeOutcome::ok(json!({ "at": d.node }))
    });
    assert_eq!(t.status, ExecutionStatus::Completed);
    assert_eq!(
        t.nodes(),
        ["s", "b", "a"],
        "array position must not decide fan-out order"
    );
}

#[test]
fn merge_visits_carry_distinct_occurrences() {
    // A merge runs once per arriving token; each visit is its own occurrence
    // (wamn-03m / R24) so the driver's node_runs rows never collide on the
    // (run, node, occurrence) key.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"fan","version":1,
            "trigger":{"type":"manual"},"entry":"s",
            "nodes":[{"id":"s","type":"echo"},{"id":"a","type":"echo"},
                     {"id":"b","type":"echo"},{"id":"m","type":"echo"}],
            "edges":[{"from":"s","to":"a"},{"from":"s","to":"b"},
                     {"from":"a","to":"m"},{"from":"b","to":"m"}]}"#,
    );
    let plan = compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |d| {
        NodeOutcome::ok(json!({ "at": d.node }))
    });
    assert_eq!(t.status, ExecutionStatus::Completed);
    let visits: Vec<(&str, u32)> = t
        .visited
        .iter()
        .map(|d| (d.node.as_str(), d.occurrence))
        .collect();
    assert_eq!(
        visits,
        [("s", 0), ("a", 0), ("b", 0), ("m", 0), ("m", 1)],
        "each arrival at the merge is a distinct occurrence"
    );
}

#[test]
fn occurrence_is_stable_across_retries_of_one_visit() {
    // Retries share the visit (attempt bumps, occurrence does not) — the
    // node_runs row identity is per-visit, not per-attempt.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"retry-occ","version":1,
            "trigger":{"type":"manual"},"entry":"b",
            "nodes":[{"id":"b","type":"call"}],"edges":[]}"#,
    );
    let plan = compile(&f).unwrap();
    let attempts = Cell::new(0u32);
    let t = run(&plan, "r1", json!({}), |_| {
        let n = attempts.replace(attempts.get() + 1);
        if n < 2 {
            NodeOutcome::Error(NodeError::Retryable(ErrorDetail::msg("x")))
        } else {
            NodeOutcome::ok(json!({}))
        }
    });
    assert_eq!(t.status, ExecutionStatus::Completed);
    assert_eq!(t.visited.len(), 3);
    assert!(t.visited.iter().all(|d| d.occurrence == 0));
    assert_eq!(t.visited[2].attempt, 2);
}

#[test]
fn an_error_routed_visit_advances_the_occurrence() {
    // b's first visit error-routes (a COMPLETED visit — the driver persists its
    // error row), h loops back, and b's second visit must be occurrence 1: a
    // driver keying rows off occurrence would otherwise collide the revisit
    // with the recorded error visit.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"err-loop","version":1,
            "trigger":{"type":"manual"},"entry":"a",
            "nodes":[{"id":"a","type":"echo"},{"id":"b","type":"call"},
                     {"id":"h","type":"handler"}],
            "edges":[{"from":"a","to":"b"},
                     {"from":"b","from-port":"error","to":"h"},
                     {"from":"h","to":"b"}]}"#,
    );
    let plan = compile(&f).unwrap();
    let first = Cell::new(true);
    let t = run(&plan, "r1", json!({}), |d| match d.node.as_str() {
        "b" if first.replace(false) => {
            NodeOutcome::Error(NodeError::Terminal(ErrorDetail::msg("boom")))
        }
        _ => NodeOutcome::ok(json!({ "at": d.node })),
    });
    assert_eq!(t.status, ExecutionStatus::Completed);
    let visits: Vec<(&str, u32)> = t
        .visited
        .iter()
        .map(|d| (d.node.as_str(), d.occurrence))
        .collect();
    assert_eq!(visits, [("a", 0), ("b", 0), ("h", 0), ("b", 1)]);
}

#[test]
fn a_leaf_with_no_successors_just_ends() {
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"leaf","version":1,
            "trigger":{"type":"manual"},"entry":"a",
            "nodes":[{"id":"a","type":"echo"}],"edges":[]}"#,
    );
    let plan = compile(&f).unwrap();
    let t = run(&plan, "r1", json!({ "x": 1 }), |_| {
        NodeOutcome::ok(json!({ "done": true }))
    });
    assert_eq!(t.status, ExecutionStatus::Completed);
    assert_eq!(t.nodes(), ["a"]);
    assert_eq!(t.state.result(), &json!({ "done": true }));
}

// ---- error paths ----------------------------------------------------------

#[test]
fn terminal_error_routes_to_error_port_and_continues() {
    // a -> b, b has main->c and error->h. b fails terminally -> h runs, c does not.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"err","version":1,
            "trigger":{"type":"manual"},"entry":"a",
            "nodes":[{"id":"a","type":"echo"},{"id":"b","type":"call"},
                     {"id":"c","type":"echo"},{"id":"h","type":"handler"}],
            "edges":[{"from":"a","to":"b"},{"from":"b","to":"c"},
                     {"from":"b","from-port":"error","to":"h"}]}"#,
    );
    let plan = compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |d| match d.node.as_str() {
        "b" => NodeOutcome::Error(NodeError::Terminal(ErrorDetail {
            message: "boom".into(),
            code: Some("HTTP_500".into()),
            data: None,
        })),
        _ => NodeOutcome::ok(json!({ "at": d.node })),
    });
    assert_eq!(t.status, ExecutionStatus::Completed); // error was handled
    assert_eq!(t.nodes(), ["a", "b", "h"]); // c skipped
    // The handler received the error payload.
    assert_eq!(
        t.visited.last().unwrap().node,
        "h",
        "handler ran last: {:?}",
        t.nodes()
    );
}

#[test]
fn terminal_error_with_no_error_path_fails_the_run() {
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"errfail","version":1,
            "trigger":{"type":"manual"},"entry":"a",
            "nodes":[{"id":"a","type":"echo"},{"id":"b","type":"call"}],
            "edges":[{"from":"a","to":"b"}]}"#,
    );
    let plan = compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |d| match d.node.as_str() {
        "b" => NodeOutcome::Error(NodeError::Terminal(ErrorDetail::msg("boom"))),
        _ => NodeOutcome::ok(json!({})),
    });
    assert_eq!(t.status, ExecutionStatus::Failed);
    let fail = t.state.failure().expect("failure recorded");
    assert_eq!(fail.node, "b");
    assert_eq!(fail.kind, ExecutionFailureKind::Terminal);
    assert_eq!(fail.detail.message, "boom");
}

// ---- retries / backoff ----------------------------------------------------

#[test]
fn retryable_retries_then_succeeds() {
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"retry","version":1,
            "trigger":{"type":"manual"},"entry":"b",
            "nodes":[{"id":"b","type":"call"}],"edges":[]}"#,
    );
    let plan = compile(&f).unwrap();
    let attempts = Cell::new(0u32);
    let t = run(&plan, "run-9", json!({}), |_| {
        let n = attempts.get();
        attempts.set(n + 1);
        if n < 2 {
            NodeOutcome::Error(NodeError::Retryable(ErrorDetail::msg("try again")))
        } else {
            NodeOutcome::ok(json!({ "ok": true }))
        }
    });
    assert_eq!(t.status, ExecutionStatus::Completed);
    // 3 dispatches (attempt 0,1,2), 2 waits at the default backoff (100, then 300).
    assert_eq!(t.nodes(), ["b", "b", "b"]);
    assert_eq!(t.visited[0].attempt, 0);
    assert_eq!(t.visited[2].attempt, 2);
    assert_eq!(t.waits.len(), 2);
    assert_eq!(t.waits[0].1, 100); // now(0) + backoff(0)=100
    assert_eq!(t.waits[1].1, 300); // now(100) + backoff(1)=200
    assert!(t.waits.iter().all(|(_, _, thr)| thr.is_none())); // plain retryable, no throttle
    // step_seq counts only the one successful completion.
    assert_eq!(t.state.step_seq(), 2); // event entry + successful node
}

#[test]
fn retry_budget_exhausts_to_failure() {
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"exhaust","version":1,
            "trigger":{"type":"manual"},"entry":"b",
            "nodes":[{"id":"b","type":"call"}],"edges":[]}"#,
    );
    let plan = compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |_| {
        NodeOutcome::Error(NodeError::Retryable(ErrorDetail::msg("nope")))
    });
    assert_eq!(t.status, ExecutionStatus::Failed);
    assert_eq!(t.nodes().len(), 3); // default max_attempts = 3
    assert_eq!(
        t.state.failure().unwrap().kind,
        ExecutionFailureKind::RetryExhausted
    );
}

#[test]
fn retry_config_overrides_budget_and_routes_to_error_path_when_exhausted() {
    // max-attempts=2 via config; b--error-->h catches the exhaustion.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"cfg","version":1,
            "trigger":{"type":"manual"},"entry":"b",
            "nodes":[{"id":"b","type":"call","config":{"retry":{"max-attempts":2,"base-ms":10}}},
                     {"id":"h","type":"handler"}],
            "edges":[{"from":"b","from-port":"error","to":"h"}]}"#,
    );
    let plan = compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |d| match d.node.as_str() {
        "b" => NodeOutcome::Error(NodeError::Retryable(ErrorDetail::msg("x"))),
        _ => NodeOutcome::ok(json!({ "handled": true })),
    });
    assert_eq!(t.status, ExecutionStatus::Completed);
    assert_eq!(t.nodes(), ["b", "b", "h"]); // 2 attempts then error branch
    assert_eq!(t.waits[0].1, 10); // base-ms override
}

#[test]
fn rate_limited_honors_retry_after_and_emits_the_shared_throttle_key() {
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"rl","version":1,
            "trigger":{"type":"manual"},"entry":"call",
            "nodes":[{"id":"call","type":"http-call"}],
            "edges":[]}"#,
    );
    let plan = compile(&f).unwrap();
    let first = Cell::new(true);
    let t = run(&plan, "r1", json!({}), |_| {
        if first.replace(false) {
            NodeOutcome::Error(NodeError::RateLimited(RateLimitDetail {
                detail: ErrorDetail::msg("429"),
                retry_after_ms: Some(5000),
                target_host: Some("erp.example".into()),
            }))
        } else {
            NodeOutcome::ok(json!({ "ok": true }))
        }
    });
    assert_eq!(t.status, ExecutionStatus::Completed);
    assert_eq!(t.waits.len(), 1);
    let (node, until, throttle) = &t.waits[0];
    assert_eq!(node, "call");
    assert_eq!(*until, 5000); // source-authoritative retry-after, not the backoff curve
    assert_eq!(
        throttle.as_ref().unwrap(),
        &ThrottleKey::new("http-call", None, Some("erp.example".into()))
    );
}

// ---- invalid-input --------------------------------------------------------

#[test]
fn invalid_input_is_never_retried() {
    // A generous retry budget must be ignored for invalid-input.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"inv","version":1,
            "trigger":{"type":"manual"},"entry":"b",
            "nodes":[{"id":"b","type":"call","config":{"retry":{"max-attempts":9}}}],"edges":[]}"#,
    );
    let plan = compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |_| {
        NodeOutcome::Error(NodeError::InvalidInput(ErrorDetail::msg("bad shape")))
    });
    assert_eq!(t.status, ExecutionStatus::Failed);
    assert_eq!(t.nodes().len(), 1); // exactly one dispatch, no retry
    assert_eq!(
        t.state.failure().unwrap().kind,
        ExecutionFailureKind::InvalidInput
    );
}

// ---- dispatch context -----------------------------------------------------

#[test]
fn dispatch_carries_type_config_and_deadline_without_flow_credentials() {
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"ctx","version":1,
            "trigger":{"type":"manual"},"entry":"n",
            "nodes":[{"id":"n","type":"http-call",
                      "config":{"url":"https://x","deadline-ms":5000}}],
            "edges":[]}"#,
    );
    let plan = compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |_| NodeOutcome::ok(json!({})));
    let d = &t.visited[0];
    assert_eq!(d.node_type, "http-call");
    assert_eq!(d.credential, None);
    assert_eq!(d.deadline_ms, Some(5000));
    assert_eq!(d.config["url"], json!("https://x"));
}

// ---- plan compilation guard -----------------------------------------------

#[test]
fn compile_rejects_an_invalid_flow() {
    // entry points at a node that does not exist -> validation error.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"bad","version":1,
            "trigger":{"type":"manual"},"entry":"missing",
            "nodes":[{"id":"a","type":"echo"}],"edges":[]}"#,
    );
    let err = compile(&f).unwrap_err();
    assert!(matches!(err, EngineError::Invalid(_)));
}

// ---- retry policy (unit) --------------------------------------------------

#[test]
fn retry_policy_reads_config_and_computes_backoff() {
    let d = RetryPolicy::DEFAULT;
    assert_eq!(d.max_attempts, 3);
    assert_eq!(d.backoff_ms(0), 100);
    assert_eq!(d.backoff_ms(1), 200);
    assert_eq!(d.backoff_ms(2), 400);
    assert!(d.may_retry(0) && d.may_retry(1) && !d.may_retry(2));
    // cap applies.
    let capped = RetryPolicy {
        base_ms: 1000,
        factor: 10.0,
        cap_ms: 5000,
        max_attempts: 10,
    };
    assert_eq!(capped.backoff_ms(0), 1000);
    assert_eq!(capped.backoff_ms(3), 5000); // 1000*1000 capped
    // from_config: reserved "retry" object; missing keys fall back.
    let p = RetryPolicy::from_config(&json!({ "retry": { "max-attempts": 5, "base-ms": 50 } }));
    assert_eq!(p.max_attempts, 5);
    assert_eq!(p.base_ms, 50);
    assert_eq!(p.factor, RetryPolicy::DEFAULT.factor);
    // no retry object / null config -> default.
    assert_eq!(RetryPolicy::from_config(&json!({})), RetryPolicy::DEFAULT);
    assert_eq!(RetryPolicy::from_config(&Value::Null), RetryPolicy::DEFAULT);
}

// ---- throttle table + concurrency gate (unit) ------------------------------

#[test]
fn throttle_table_gates_and_opens() {
    let mut t = ThrottleTable::new();
    let k = ThrottleKey::new("http-call", Some("erp".into()), Some("h".into()));
    assert!(t.ready(&k, 0)); // no gate
    t.gate(k.clone(), 1000);
    assert!(!t.ready(&k, 999));
    assert_eq!(t.gated_until(&k, 999), Some(1000));
    assert!(t.ready(&k, 1000)); // deadline reached
    // gate never shortens.
    t.gate(k.clone(), 2000);
    t.gate(k.clone(), 1500);
    assert!(!t.ready(&k, 1900));
    // an unrelated key is unaffected.
    let other = ThrottleKey::new("http-call", Some("other".into()), Some("h".into()));
    assert!(t.ready(&other, 0));
    t.sweep(3000);
    assert!(t.ready(&k, 3000));
}

#[test]
fn concurrency_gate_enforces_per_flow_concurrency() {
    let mut s = ConcurrencyGate::new(2);
    assert!(s.try_admit("f"));
    assert!(s.try_admit("f"));
    assert!(!s.try_admit("f")); // at cap -> backpressure
    assert_eq!(s.in_flight("f"), 2);
    // a different flow is independent.
    assert!(s.try_admit("g"));
    s.finish("f");
    assert!(s.try_admit("f")); // slot freed
    // limit 0 = unlimited.
    let mut u = ConcurrencyGate::new(0);
    for _ in 0..100 {
        assert!(u.try_admit("x"));
    }
}

// ---- dispatch budget: the runaway-loop runtime bound (cjv.4) --------------

/// A 4-node linear flow a -> b -> c -> d.
fn linear4() -> Flow {
    flow(
        r#"{"schema-version":"0.1","flow-id":"lin4","version":1,
            "trigger":{"type":"manual"},"entry":"a",
            "nodes":[{"id":"a","type":"echo"},{"id":"b","type":"echo"},
                     {"id":"c","type":"echo"},{"id":"d","type":"echo"}],
            "edges":[{"from":"a","to":"b"},{"from":"b","to":"c"},{"from":"c","to":"d"}]}"#,
    )
}

/// A permitted 2-node cycle with no exit: `in → a → b → a → …`. Loops are a
/// flow feature (only self-loops are rejected), so termination is bounded at
/// runtime by the dispatch budget, not at validate time.
fn runaway_cycle() -> Flow {
    flow(
        r#"{"schema-version":"0.1","flow-id":"runaway","version":1,
            "trigger":{"type":"manual"},"entry":"in",
            "nodes":[{"id":"in","type":"echo"},{"id":"a","type":"echo"},
                     {"id":"b","type":"echo"}],
            "edges":[{"from":"in","to":"a"},{"from":"a","to":"b"},
                     {"from":"b","to":"a"}]}"#,
    )
}

/// Drive with a hard iteration ceiling so a budget-removed mutant FAILS the
/// assert instead of hanging the test binary (the plain `run` helper loops
/// until terminal, which a runaway mutant never reaches).
fn run_bounded(
    plan: &Plan,
    st: &mut ExecutionState,
    max_iters: usize,
) -> (Vec<String>, ExecutionStatus) {
    let mut dispatched = Vec::new();
    for _ in 0..max_iters {
        match plan.next(st, 0) {
            Step::Done(s) => return (dispatched, s),
            Step::Wait { .. } => panic!("unexpected wait in a budget test"),
            Step::Reserved(step) => plan.apply_reserved(st, &step).unwrap(),
            Step::Dispatch(d) => {
                dispatched.push(d.node.clone());
                plan.apply(st, &d, NodeOutcome::ok(json!("loop")), 0)
                    .unwrap();
            }
        }
    }
    panic!("no terminal status within {max_iters} iterations — the dispatch budget did not fire");
}

#[test]
fn a_runaway_cycle_fails_at_exactly_the_budget() {
    let f = runaway_cycle();
    let mut plan = compile(&f).unwrap();
    plan.set_dispatch_budget(5);
    let mut st = started(&plan, "r1", json!("go"));
    let (dispatched, status) = run_bounded(&plan, &mut st, 20);
    // The event entry consumes one unit, then four downstream executions are
    // allowed before the run fails terminally at the configured total of five.
    assert_eq!(dispatched.len(), 4);
    assert_eq!(st.dispatched(), 5);
    assert_eq!(status, ExecutionStatus::Failed);
    let failure = st.failure().expect("failure recorded");
    assert_eq!(failure.kind, ExecutionFailureKind::RunawayBudget);
    // The failure names the node that would have run next (the 6th execution).
    assert_eq!(failure.node, "b");
    assert_eq!(failure.detail.code.as_deref(), Some("runaway-budget"));
}

#[test]
fn a_flow_that_uses_exactly_the_budget_completes() {
    // The event entry plus linear4 dispatch exactly 5 nodes; budget 5 must let
    // the run complete (the budget is "may execute N nodes", not "fails at N").
    let f = linear4();
    let mut plan = compile(&f).unwrap();
    plan.set_dispatch_budget(5);
    let mut st = started(&plan, "r1", json!("go"));
    let (dispatched, status) = run_bounded(&plan, &mut st, 20);
    assert_eq!(status, ExecutionStatus::Completed);
    assert_eq!(dispatched.len(), 4);
    assert!(st.failure().is_none());
}

#[test]
fn retries_count_against_the_budget() {
    // A node that never stops failing retryable would burn its retry budget —
    // but with a dispatch budget below the retry allowance, the run fails
    // RunawayBudget first: every execution (retries included) counts.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"retryloop","version":1,
            "trigger":{"type":"manual"},"entry":"x",
            "nodes":[{"id":"x","type":"echo",
                      "config":{"retry":{"max-attempts":10,"base-ms":0}}}],
            "edges":[]}"#,
    );
    let mut plan = compile(&f).unwrap();
    plan.set_dispatch_budget(3);
    let mut st = started(&plan, "r1", json!("go"));
    let mut executions = 0;
    let status = loop {
        if executions > 20 {
            panic!("budget did not fire");
        }
        // Jump the clock past any scheduled backoff so every retry is due.
        match plan.next(&mut st, u64::MAX / 2) {
            Step::Done(s) => break s,
            Step::Wait { .. } => panic!("retry should be due at a huge now"),
            Step::Reserved(step) => plan.apply_reserved(&mut st, &step).unwrap(),
            Step::Dispatch(d) => {
                executions += 1;
                plan.apply(
                    &mut st,
                    &d,
                    NodeOutcome::Error(NodeError::Retryable(ErrorDetail::msg("flaky"))),
                    u64::MAX / 2,
                )
                .unwrap();
            }
        }
    };
    assert_eq!(executions, 2);
    assert_eq!(st.dispatched(), 3);
    assert_eq!(status, ExecutionStatus::Failed);
    assert_eq!(
        st.failure().unwrap().kind,
        ExecutionFailureKind::RunawayBudget
    );
}

#[test]
fn the_budget_verdict_is_terminal_even_with_an_error_path() {
    // The looping node has an error edge to a rescue node — which must NOT
    // catch the budget verdict (an error path can itself be part of the loop).
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"looped-rescue","version":1,
            "trigger":{"type":"manual"},"entry":"in",
            "nodes":[{"id":"in","type":"echo"},{"id":"a","type":"echo"},
                     {"id":"b","type":"echo"},{"id":"rescue","type":"echo"}],
            "edges":[{"from":"in","to":"a"},{"from":"a","to":"b"},
                     {"from":"b","to":"a"},
                     {"from":"a","from-port":"error","to":"rescue"}]}"#,
    );
    let mut plan = compile(&f).unwrap();
    plan.set_dispatch_budget(5);
    let mut st = started(&plan, "r1", json!("go"));
    let (dispatched, status) = run_bounded(&plan, &mut st, 20);
    assert_eq!(status, ExecutionStatus::Failed);
    assert_eq!(
        st.failure().unwrap().kind,
        ExecutionFailureKind::RunawayBudget
    );
    // The rescue node never ran: the verdict bypassed the error path.
    assert!(!dispatched.iter().any(|n| n == "rescue"));
}

#[test]
fn the_default_budget_is_generous_but_finite() {
    let f = runaway_cycle();
    let plan = compile(&f).unwrap();
    assert_eq!(plan.dispatch_budget(), wamn_runner::DEFAULT_DISPATCH_BUDGET);
    assert_eq!(wamn_runner::DEFAULT_DISPATCH_BUDGET, 10_000);
    let mut st = started(&plan, "r1", json!("go"));
    let (dispatched, status) = run_bounded(&plan, &mut st, 10_100);
    assert_eq!(status, ExecutionStatus::Failed);
    assert_eq!(dispatched.len(), 9_999);
    assert_eq!(st.dispatched(), 10_000);
    assert_eq!(
        st.failure().unwrap().kind,
        ExecutionFailureKind::RunawayBudget
    );
}

// ---------------------------------------------------------------------------
// R32: in-memory retry cursor
// ---------------------------------------------------------------------------

/// A single retryable node with no edges — the R32 acceptance shape. `cfg` is an
/// optional `,"config":{...}` tail.
fn one_retryable_node(cfg: &str) -> Flow {
    flow(&format!(
        r#"{{"schema-version":"0.1","flow-id":"r32","version":1,
            "trigger":{{"type":"manual"}},"entry":"b",
            "nodes":[{{"id":"b","type":"call"{cfg}}}],"edges":[]}}"#
    ))
}

#[test]
fn wait_carries_the_pending_retry_attempt() {
    // After a retryable failure the next in-memory step is a Wait carrying the
    // attempt (1) at the default backoff (100ms). A mutant that zeroes this
    // cursor is caught here.
    let f = one_retryable_node("");
    let plan = compile(&f).unwrap();
    let mut st = started(&plan, "r1", json!({}));
    let Step::Dispatch(d) = plan.next(&mut st, 0) else {
        panic!("first step dispatches");
    };
    assert_eq!(d.attempt, 0);
    plan.apply(
        &mut st,
        &d,
        NodeOutcome::Error(NodeError::Retryable(ErrorDetail::msg("flaky"))),
        0,
    )
    .unwrap();
    match plan.next(&mut st, 0) {
        Step::Wait {
            node,
            until_ms,
            attempt,
            ..
        } => {
            assert_eq!(node, "b");
            assert_eq!(until_ms, 100); // now(0) + backoff(0)
            assert_eq!(
                attempt, 1,
                "the Wait carries the attempt the retry will run as"
            );
        }
        other => panic!("expected a retry Wait, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// SDK contract drift-guards (5.3)
// ---------------------------------------------------------------------------

/// The SDK defines its own port constants (it must not depend on the flow
/// schema crate); this pins them to the engine's `wamn_flow` values.
#[test]
fn node_contract_port_constants_mirror_the_flow_schema() {
    assert_eq!(wamn_flow::MAIN_PORT, wamn_runner::MAIN_PORT);
    assert_eq!(wamn_flow::ERROR_PORT, wamn_runner::ERROR_PORT);
}

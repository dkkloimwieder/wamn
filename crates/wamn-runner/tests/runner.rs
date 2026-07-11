//! Engine tests — the whole execution model exercised with NO cluster, NO DB, NO
//! wasm: build a `wamn_flow::Flow`, compile a `Plan`, and drive it with a
//! programmable node dispatcher, asserting the walk / branch / merge / error /
//! retry / throttle behavior purely from returned `Step`s and final `RunState`.

use std::cell::Cell;

use serde_json::{Value, json};
use wamn_flow::Flow;
use wamn_runner::{
    Dispatch, EngineError, FailKind, NodeError, NodeOutcome, Plan, RateLimitDetail, RetryPolicy,
    RunState, RunStatus, Scheduler, Step, ThrottleKey, ThrottleTable,
};

/// A recorded drive of one run to a terminal status.
struct Trace {
    /// Every `Dispatch`, in order.
    visited: Vec<Dispatch>,
    /// Every `Wait` as `(node, until_ms, throttle)`, in order.
    waits: Vec<(String, u64, Option<ThrottleKey>)>,
    status: RunStatus,
    state: RunState,
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
    let mut st = plan.start(run_id, input);
    let status = loop {
        match plan.next(&mut st, clock.get()) {
            Step::Done(s) => break s,
            Step::Wait {
                node,
                until_ms,
                throttle,
            } => {
                waits.push((node, until_ms, throttle));
                clock.set(until_ms); // virtual sleep
            }
            Step::Dispatch(d) => {
                visited.push(d.clone());
                let outcome = dispatch_fn(&d);
                plan.apply(&mut st, &d, outcome, clock.get());
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
    Flow::from_json(json_str).expect("fixture flow parses")
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
    let plan = Plan::compile(&f).unwrap();
    // Each node emits a payload naming itself, so the result is the last node's.
    let t = run(&plan, "r1", json!({ "seen": [] }), |d| {
        NodeOutcome::ok(json!({ "at": d.node }))
    });
    assert_eq!(t.status, RunStatus::Completed);
    assert_eq!(t.nodes(), ["a", "b", "c"]);
    assert_eq!(t.state.step_seq(), 3);
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
    let plan = Plan::compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |d| match d.node.as_str() {
        "cond" => NodeOutcome::ok_on(json!({ "picked": true }), "true"),
        _ => NodeOutcome::ok(json!({ "at": d.node })),
    });
    assert_eq!(t.status, RunStatus::Completed);
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
    let plan = Plan::compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |d| {
        NodeOutcome::ok(json!({ "at": d.node }))
    });
    assert_eq!(t.status, RunStatus::Completed);
    // BFS order: s, then a, b, then m (from a), m (from b).
    assert_eq!(t.nodes(), ["s", "a", "b", "m", "m"]);
    assert_eq!(t.state.step_seq(), 5);
}

#[test]
fn a_leaf_with_no_successors_just_ends() {
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"leaf","version":1,
            "trigger":{"type":"manual"},"entry":"a",
            "nodes":[{"id":"a","type":"echo"}],"edges":[]}"#,
    );
    let plan = Plan::compile(&f).unwrap();
    let t = run(&plan, "r1", json!({ "x": 1 }), |_| {
        NodeOutcome::ok(json!({ "done": true }))
    });
    assert_eq!(t.status, RunStatus::Completed);
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
    let plan = Plan::compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |d| match d.node.as_str() {
        "b" => NodeOutcome::Error(NodeError::Terminal(wamn_runner::ErrorDetail {
            message: "boom".into(),
            code: Some("HTTP_500".into()),
            data: None,
        })),
        _ => NodeOutcome::ok(json!({ "at": d.node })),
    });
    assert_eq!(t.status, RunStatus::Completed); // error was handled
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
    let plan = Plan::compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |d| match d.node.as_str() {
        "b" => NodeOutcome::Error(NodeError::Terminal(wamn_runner::ErrorDetail::msg("boom"))),
        _ => NodeOutcome::ok(json!({})),
    });
    assert_eq!(t.status, RunStatus::Failed);
    let fail = t.state.failure().expect("failure recorded");
    assert_eq!(fail.node, "b");
    assert_eq!(fail.kind, FailKind::Terminal);
    assert_eq!(fail.detail.message, "boom");
}

// ---- retries / backoff ----------------------------------------------------

#[test]
fn retryable_retries_then_succeeds_with_stable_idempotency_key() {
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"retry","version":1,
            "trigger":{"type":"manual"},"entry":"b",
            "nodes":[{"id":"b","type":"call"}],"edges":[]}"#,
    );
    let plan = Plan::compile(&f).unwrap();
    let attempts = Cell::new(0u32);
    let t = run(&plan, "run-9", json!({}), |_| {
        let n = attempts.get();
        attempts.set(n + 1);
        if n < 2 {
            NodeOutcome::Error(NodeError::Retryable(wamn_runner::ErrorDetail::msg(
                "try again",
            )))
        } else {
            NodeOutcome::ok(json!({ "ok": true }))
        }
    });
    assert_eq!(t.status, RunStatus::Completed);
    // 3 dispatches (attempt 0,1,2), 2 waits at the default backoff (100, then 300).
    assert_eq!(t.nodes(), ["b", "b", "b"]);
    assert_eq!(t.visited[0].attempt, 0);
    assert_eq!(t.visited[2].attempt, 2);
    assert_eq!(t.waits.len(), 2);
    assert_eq!(t.waits[0].1, 100); // now(0) + backoff(0)=100
    assert_eq!(t.waits[1].1, 300); // now(100) + backoff(1)=200
    assert!(t.waits.iter().all(|(_, _, thr)| thr.is_none())); // plain retryable, no throttle
    // Idempotency key stable across retries.
    let key = &t.visited[0].idempotency_key;
    assert_eq!(key, "run-9:b");
    assert!(t.visited.iter().all(|d| &d.idempotency_key == key));
    // step_seq counts only the one successful completion.
    assert_eq!(t.state.step_seq(), 1);
}

#[test]
fn retry_budget_exhausts_to_failure() {
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"exhaust","version":1,
            "trigger":{"type":"manual"},"entry":"b",
            "nodes":[{"id":"b","type":"call"}],"edges":[]}"#,
    );
    let plan = Plan::compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |_| {
        NodeOutcome::Error(NodeError::Retryable(wamn_runner::ErrorDetail::msg("nope")))
    });
    assert_eq!(t.status, RunStatus::Failed);
    assert_eq!(t.nodes().len(), 3); // default max_attempts = 3
    assert_eq!(t.state.failure().unwrap().kind, FailKind::RetryExhausted);
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
    let plan = Plan::compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |d| match d.node.as_str() {
        "b" => NodeOutcome::Error(NodeError::Retryable(wamn_runner::ErrorDetail::msg("x"))),
        _ => NodeOutcome::ok(json!({ "handled": true })),
    });
    assert_eq!(t.status, RunStatus::Completed);
    assert_eq!(t.nodes(), ["b", "b", "h"]); // 2 attempts then error branch
    assert_eq!(t.waits[0].1, 10); // base-ms override
}

#[test]
fn rate_limited_honors_retry_after_and_emits_the_shared_throttle_key() {
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"rl","version":1,
            "trigger":{"type":"manual"},"entry":"call",
            "nodes":[{"id":"call","type":"http-call","credential":"erp"}],
            "edges":[],"credentials":[{"name":"erp"}]}"#,
    );
    let plan = Plan::compile(&f).unwrap();
    let first = Cell::new(true);
    let t = run(&plan, "r1", json!({}), |_| {
        if first.replace(false) {
            NodeOutcome::Error(NodeError::RateLimited(RateLimitDetail {
                detail: wamn_runner::ErrorDetail::msg("429"),
                retry_after_ms: Some(5000),
                target_host: Some("erp.example".into()),
            }))
        } else {
            NodeOutcome::ok(json!({ "ok": true }))
        }
    });
    assert_eq!(t.status, RunStatus::Completed);
    assert_eq!(t.waits.len(), 1);
    let (node, until, throttle) = &t.waits[0];
    assert_eq!(node, "call");
    assert_eq!(*until, 5000); // source-authoritative retry-after, not the backoff curve
    assert_eq!(
        throttle.as_ref().unwrap(),
        &ThrottleKey::new("http-call", Some("erp".into()), Some("erp.example".into()))
    );
}

// ---- invalid-input / cancelled --------------------------------------------

#[test]
fn invalid_input_is_never_retried() {
    // A generous retry budget must be ignored for invalid-input.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"inv","version":1,
            "trigger":{"type":"manual"},"entry":"b",
            "nodes":[{"id":"b","type":"call","config":{"retry":{"max-attempts":9}}}],"edges":[]}"#,
    );
    let plan = Plan::compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |_| {
        NodeOutcome::Error(NodeError::InvalidInput(wamn_runner::ErrorDetail::msg(
            "bad shape",
        )))
    });
    assert_eq!(t.status, RunStatus::Failed);
    assert_eq!(t.nodes().len(), 1); // exactly one dispatch, no retry
    assert_eq!(t.state.failure().unwrap().kind, FailKind::InvalidInput);
}

#[test]
fn cancelled_stops_the_run_and_does_not_fire_error_branches() {
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"cancel","version":1,
            "trigger":{"type":"manual"},"entry":"b",
            "nodes":[{"id":"b","type":"call"},{"id":"h","type":"handler"}],
            "edges":[{"from":"b","from-port":"error","to":"h"}]}"#,
    );
    let plan = Plan::compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |d| match d.node.as_str() {
        "b" => NodeOutcome::Error(NodeError::Cancelled),
        _ => NodeOutcome::ok(json!({})),
    });
    assert_eq!(t.status, RunStatus::Cancelled);
    assert_eq!(t.nodes(), ["b"]); // error branch h did NOT fire
}

// ---- dispatch context -----------------------------------------------------

#[test]
fn dispatch_carries_type_config_credential_and_deadline() {
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"ctx","version":1,
            "trigger":{"type":"manual"},"entry":"n",
            "nodes":[{"id":"n","type":"http-call","credential":"c",
                      "config":{"url":"https://x","deadline-ms":5000}}],
            "edges":[],"credentials":[{"name":"c"}]}"#,
    );
    let plan = Plan::compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |_| NodeOutcome::ok(json!({})));
    let d = &t.visited[0];
    assert_eq!(d.node_type, "http-call");
    assert_eq!(d.credential.as_deref(), Some("c"));
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
    let err = Plan::compile(&f).unwrap_err();
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

// ---- throttle table + scheduler (unit) ------------------------------------

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
fn scheduler_enforces_per_flow_concurrency() {
    let mut s = Scheduler::new(2);
    assert!(s.try_admit("f"));
    assert!(s.try_admit("f"));
    assert!(!s.try_admit("f")); // at cap -> backpressure
    assert_eq!(s.in_flight("f"), 2);
    // a different flow is independent.
    assert!(s.try_admit("g"));
    s.finish("f");
    assert!(s.try_admit("f")); // slot freed
    // limit 0 = unlimited.
    let mut u = Scheduler::new(0);
    for _ in 0..100 {
        assert!(u.try_admit("x"));
    }
}

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
                ..
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
    let plan = Plan::compile(&f).unwrap();
    let t = run(&plan, "r1", json!({}), |d| {
        NodeOutcome::ok(json!({ "at": d.node }))
    });
    assert_eq!(t.status, RunStatus::Completed);
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
    // R25: distinct visits carry DISTINCT idempotency keys — an external
    // system honoring idempotency headers must not dedupe the merge's second
    // execution away.
    assert_eq!(t.visited[3].idempotency_key, "r1:m:0");
    assert_eq!(t.visited[4].idempotency_key, "r1:m:1");
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
    let plan = Plan::compile(&f).unwrap();
    let attempts = Cell::new(0u32);
    let t = run(&plan, "r1", json!({}), |_| {
        let n = attempts.replace(attempts.get() + 1);
        if n < 2 {
            NodeOutcome::Error(NodeError::Retryable(wamn_runner::ErrorDetail::msg("x")))
        } else {
            NodeOutcome::ok(json!({}))
        }
    });
    assert_eq!(t.status, RunStatus::Completed);
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
    let plan = Plan::compile(&f).unwrap();
    let first = Cell::new(true);
    let t = run(&plan, "r1", json!({}), |d| match d.node.as_str() {
        "b" if first.replace(false) => {
            NodeOutcome::Error(NodeError::Terminal(wamn_runner::ErrorDetail::msg("boom")))
        }
        _ => NodeOutcome::ok(json!({ "at": d.node })),
    });
    assert_eq!(t.status, RunStatus::Completed);
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
    // Idempotency key stable across retries of one visit (R25: the trailing
    // occurrence stays 0 — retries never mint a new key).
    let key = &t.visited[0].idempotency_key;
    assert_eq!(key, "run-9:b:0");
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

// ---- resume: branch-aware reconstruction from recorded steps --------------

use wamn_runner::{Recorded, ResumeError, UnknownNode};

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

/// A conditional that branches into two independent two-node subtrees, so a
/// resume must place the frontier in exactly the taken branch.
fn branchy() -> Flow {
    flow(
        r#"{"schema-version":"0.1","flow-id":"brc","version":1,
            "trigger":{"type":"manual"},"entry":"cond",
            "nodes":[{"id":"cond","type":"conditional"},
                     {"id":"y1","type":"pg-write"},{"id":"y2","type":"respond"},
                     {"id":"n1","type":"pg-write"},{"id":"n2","type":"respond"}],
            "edges":[{"from":"cond","from-port":"true","to":"y1"},
                     {"from":"y1","to":"y2"},
                     {"from":"cond","from-port":"false","to":"n1"},
                     {"from":"n1","to":"n2"}]}"#,
    )
}

#[test]
fn resume_reconstructs_a_linear_frontier_and_continues() {
    let f = linear4();
    let plan = Plan::compile(&f).unwrap();
    // The run was killed after b committed: a and b are recorded, c/d are not.
    let completed = [
        Recorded::new("a", "main", json!({ "at": "a" })),
        Recorded::new("b", "main", json!({ "at": "b" })),
    ];
    let mut st = plan
        .resume("r1", json!({ "trigger": 1 }), &completed)
        .unwrap();
    assert_eq!(st.status(), RunStatus::Running);
    assert_eq!(st.step_seq(), 2); // two steps folded

    // The driver continues: the very next dispatch is c (not a re-run of a/b),
    // and c sees b's recorded output as its input.
    let mut resumed = Vec::new();
    let status = plan.drive(
        &mut st,
        || 0,
        |_, _| {},
        |d| {
            resumed.push(d.node.clone());
            NodeOutcome::ok(json!({ "at": d.node }))
        },
    );
    assert_eq!(status, RunStatus::Completed);
    assert_eq!(resumed, ["c", "d"]); // a and b are NOT re-dispatched
}

#[test]
fn resume_is_branch_aware_only_the_taken_branch_is_outstanding() {
    let f = branchy();
    let plan = Plan::compile(&f).unwrap();
    // cond took the "true" port; y1 (pg-write) committed but the run was killed
    // before y2 was recorded. Reconstruction must leave the frontier at y2 and
    // NEVER touch the false branch (n1/n2).
    let completed = [
        Recorded::new("cond", "true", json!({ "picked": "true" })),
        Recorded::new("y1", "main", json!({ "wrote": "y" })),
    ];
    let mut st = plan.resume("r1", json!({}), &completed).unwrap();
    let mut resumed = Vec::new();
    let status = plan.drive(
        &mut st,
        || 0,
        |_, _| {},
        |d| {
            resumed.push(d.node.clone());
            NodeOutcome::ok(json!({ "at": d.node }))
        },
    );
    assert_eq!(status, RunStatus::Completed);
    assert_eq!(resumed, ["y2"]); // only the taken branch's remainder; n1/n2 never run
}

#[test]
fn resume_kill_mid_branch_then_resume_completes_the_correct_branch() {
    // End-to-end at the engine level: run the branchy flow, kill it right after
    // the branch's pg-write, reconstruct from what was recorded, and assert the
    // resumed run finishes the SAME branch exactly once (the branch-aware
    // kill-mid-branch -> resume proof).
    let f = branchy();
    let plan = Plan::compile(&f).unwrap();

    // Original run: cond picks "false"; capture records up to the killed node.
    let mut records: Vec<Recorded> = Vec::new();
    let mut st = plan.start("orig", json!({}));
    // Walk manually, recording each success, and "kill" right after n1.
    loop {
        match plan.next(&mut st, 0) {
            Step::Dispatch(d) => {
                let (payload, port) = match d.node.as_str() {
                    "cond" => (json!({ "picked": "false" }), "false".to_string()),
                    other => (json!({ "at": other }), "main".to_string()),
                };
                records.push(Recorded::new(d.node.clone(), port.clone(), payload.clone()));
                plan.apply(&mut st, &d, NodeOutcome::Success { payload, port }, 0);
                if d.node == "n1" {
                    break; // killed after n1 committed, before n2
                }
            }
            _ => panic!("unexpected step"),
        }
    }
    assert_eq!(
        records.iter().map(|r| r.node.as_str()).collect::<Vec<_>>(),
        ["cond", "n1"]
    );

    // Resume a fresh state from the records; only n2 remains.
    let mut st2 = plan.resume("resumed", json!({}), &records).unwrap();
    let mut resumed = Vec::new();
    let status = plan.drive(
        &mut st2,
        || 0,
        |_, _| {},
        |d| {
            resumed.push(d.node.clone());
            NodeOutcome::ok(json!({ "at": d.node }))
        },
    );
    assert_eq!(status, RunStatus::Completed);
    assert_eq!(resumed, ["n2"]); // the false branch completes; y1/y2 never run
}

/// The R24 acceptance flow: a diamond A -> {B, C} -> D, acyclic yet D runs
/// twice (once per arriving token).
fn diamond() -> Flow {
    flow(
        r#"{"schema-version":"0.1","flow-id":"dia","version":1,
            "trigger":{"type":"manual"},"entry":"a",
            "nodes":[{"id":"a","type":"echo"},{"id":"b","type":"echo"},
                     {"id":"c","type":"echo"},{"id":"d","type":"echo"}],
            "edges":[{"from":"a","to":"b"},{"from":"a","to":"c"},
                     {"from":"b","to":"d"},{"from":"c","to":"d"}]}"#,
    )
}

/// A bounded 2-node loop with a port exit: in -> x, x --next--> y -> x,
/// x --done--> out. The dispatcher decides when x emits "done".
fn bounded_loop() -> Flow {
    flow(
        r#"{"schema-version":"0.1","flow-id":"loop","version":1,
            "trigger":{"type":"manual"},"entry":"in",
            "nodes":[{"id":"in","type":"echo"},{"id":"x","type":"echo"},
                     {"id":"y","type":"echo"},{"id":"out","type":"echo"}],
            "edges":[{"from":"in","to":"x"},{"from":"x","from-port":"next","to":"y"},
                     {"from":"y","to":"x"},{"from":"x","from-port":"done","to":"out"}]}"#,
    )
}

#[test]
fn resume_diamond_killed_mid_merge_reconstructs_and_completes() {
    // The R24 VERIFY: a diamond killed mid-D — D's first visit recorded, its
    // second outstanding — reconstructs without Mismatch/Overrun and completes
    // with exactly the one remaining visit, at the right occurrence.
    let f = diamond();
    let plan = Plan::compile(&f).unwrap();
    let completed = [
        Recorded::new("a", "main", json!({ "at": "a" })),
        Recorded::new("b", "main", json!({ "at": "b" })),
        Recorded::new("c", "main", json!({ "at": "c" })),
        Recorded::new("d", "main", json!({ "at": "d" })), // D's FIRST visit only
    ];
    let mut st = plan.resume("r1", json!({}), &completed).unwrap();
    let mut resumed = Vec::new();
    let mut keys = Vec::new();
    let status = plan.drive(
        &mut st,
        || 0,
        |_, _| {},
        |d| {
            resumed.push((d.node.clone(), d.occurrence));
            keys.push(d.idempotency_key.clone());
            NodeOutcome::ok(json!({ "at": d.node }))
        },
    );
    assert_eq!(status, RunStatus::Completed);
    assert_eq!(
        resumed,
        [("d".to_string(), 1)],
        "only D's second visit is outstanding, at occurrence 1"
    );
    // R25: the resumed second visit carries the SAME key it would have live —
    // replay rebuilds the visit counts, so the key does not collide with D's
    // recorded first execution.
    assert_eq!(keys, ["r1:d:1"]);
}

#[test]
fn resume_of_a_fully_recorded_diamond_is_idempotent() {
    // All five visits recorded (D twice): resume folds the per-visit history and
    // completes with nothing re-dispatched — the merge history no longer
    // collapses (pre-R24 the dropped second D row made this walk re-run D).
    let f = diamond();
    let plan = Plan::compile(&f).unwrap();
    let completed = [
        Recorded::new("a", "main", json!({ "at": "a" })),
        Recorded::new("b", "main", json!({ "at": "b" })),
        Recorded::new("c", "main", json!({ "at": "c" })),
        Recorded::new("d", "main", json!({ "d": 1 })),
        Recorded::new("d", "main", json!({ "d": 2 })),
    ];
    let mut st = plan.resume("r1", json!({}), &completed).unwrap();
    let mut resumed = Vec::new();
    let status = plan.drive(
        &mut st,
        || 0,
        |_, _| {},
        |d| {
            resumed.push(d.node.clone());
            NodeOutcome::ok(d.payload.clone())
        },
    );
    assert_eq!(status, RunStatus::Completed);
    assert!(resumed.is_empty());
    assert_eq!(st.result(), &json!({ "d": 2 })); // the LAST visit's emission
}

#[test]
fn resume_mid_loop_continues_at_the_right_visit() {
    // A loop killed after its second lap replays visit-by-visit and continues
    // at the correct occurrence — pre-R24 a loop crashing after 2 visits was
    // permanently unrecoverable (its collapsed history could not replay).
    let f = bounded_loop();
    let plan = Plan::compile(&f).unwrap();
    // Two full laps recorded (x emitting "next"), killed with x's third visit
    // outstanding: in, x@0, y@0, x@1, y@1.
    let completed = [
        Recorded::new("in", "main", json!(0)),
        Recorded::new("x", "next", json!(1)),
        Recorded::new("y", "main", json!(1)),
        Recorded::new("x", "next", json!(2)),
        Recorded::new("y", "main", json!(2)),
    ];
    let mut st = plan.resume("r1", json!(0), &completed).unwrap();
    let mut resumed = Vec::new();
    let status = plan.drive(
        &mut st,
        || 0,
        |_, _| {},
        |d| {
            resumed.push((d.node.clone(), d.occurrence));
            // The resumed third visit of x exits the loop.
            if d.node == "x" {
                NodeOutcome::ok_on(json!(3), "done")
            } else {
                NodeOutcome::ok(d.payload.clone())
            }
        },
    );
    assert_eq!(status, RunStatus::Completed);
    assert_eq!(
        resumed,
        [("x".to_string(), 2), ("out".to_string(), 0)],
        "the walk resumes at x's THIRD visit (occurrence 2), then exits"
    );
}

#[test]
fn resume_reconstructs_an_error_routed_branch() {
    // A node that failed and was routed to its error path is recorded as an
    // emission on ERROR_PORT carrying the error payload — so reconstruction
    // rebuilds the error branch with no error taxonomy.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"err","version":1,
            "trigger":{"type":"manual"},"entry":"a",
            "nodes":[{"id":"a","type":"http-call"},{"id":"h","type":"notify"},
                     {"id":"ok","type":"respond"}],
            "edges":[{"from":"a","to":"ok"},{"from":"a","from-port":"error","to":"h"}]}"#,
    );
    let plan = Plan::compile(&f).unwrap();
    let completed = [Recorded::new(
        "a",
        "error",
        json!({ "error": { "message": "boom" } }),
    )];
    let mut st = plan.resume("r1", json!({}), &completed).unwrap();
    let mut resumed = Vec::new();
    let status = plan.drive(
        &mut st,
        || 0,
        |_, _| {},
        |d| {
            resumed.push(d.node.clone());
            NodeOutcome::ok(d.payload.clone())
        },
    );
    assert_eq!(status, RunStatus::Completed);
    assert_eq!(resumed, ["h"]); // the error branch, not the success node "ok"
}

/// a error-routes to h; a's UNTAKEN main edge a->ok must never run. h succeeds.
/// The R26 shape: replaying a's error-routed record must reproduce the LIVE
/// step_seq/result, not the pre-fix success-fold (which bumped both).
fn error_route_flow() -> Flow {
    flow(
        r#"{"schema-version":"0.1","flow-id":"r26","version":1,
            "trigger":{"type":"manual"},"entry":"a",
            "nodes":[{"id":"a","type":"call"},{"id":"h","type":"handler"},
                     {"id":"ok","type":"echo"}],
            "edges":[{"from":"a","to":"ok"},
                     {"from":"a","from-port":"error","to":"h"}]}"#,
    )
}

#[test]
fn resumed_error_routed_run_matches_live_step_seq_and_result() {
    // R26: an error-ROUTED record must replay as the live error-route transition,
    // NOT a success fold. Drive LIVE, then resume from the full per-visit records
    // and assert step_seq and result match live exactly (pre-fix the resumed
    // step_seq was one higher — the error record wrongly bumped it).
    let f = error_route_flow();
    let plan = Plan::compile(&f).unwrap();

    // LIVE: a fails terminally -> error-routes to h; ok never runs; h succeeds.
    let live = run(&plan, "live", json!({}), |d| match d.node.as_str() {
        "a" => NodeOutcome::Error(NodeError::Terminal(wamn_runner::ErrorDetail::coded(
            "HTTP_500", "boom",
        ))),
        _ => NodeOutcome::ok(json!({ "handled": true })),
    });
    assert_eq!(live.status, RunStatus::Completed);
    assert_eq!(live.nodes(), ["a", "h"]); // ok skipped
    assert_eq!(live.state.step_seq(), 1); // only h's success counts
    // The error payload a actually emitted (h's live input) — reuse it verbatim
    // so the record matches what the run emitted.
    let error_payload = live
        .visited
        .iter()
        .find(|d| d.node == "h")
        .unwrap()
        .payload
        .clone();

    // RESUME from the full per-visit records: [a@error, h@main].
    let records = [
        Recorded::new("a", "error", error_payload.clone()),
        Recorded::new("h", "main", json!({ "handled": true })),
    ];
    let mut st = plan.resume("resumed", json!({}), &records).unwrap();
    assert_eq!(
        st.step_seq(),
        live.state.step_seq(),
        "the error record must not bump step_seq"
    );
    assert_eq!(
        st.result(),
        live.state.result(),
        "the error record must not overwrite result"
    );

    // Nothing outstanding: the resume completes with nothing re-dispatched.
    let mut resumed = Vec::new();
    let status = plan.drive(
        &mut st,
        || 0,
        |_, _| {},
        |d| {
            resumed.push(d.node.clone());
            NodeOutcome::ok(d.payload.clone())
        },
    );
    assert_eq!(status, RunStatus::Completed);
    assert!(resumed.is_empty());
}

#[test]
fn resume_partial_after_error_route_leaves_step_seq_zero_and_null_result() {
    // Only a's error record replayed: after routing (before h), the LIVE state is
    // step_seq 0 / result Null — the error route touches neither. Driving on
    // completes via h, which receives the error payload as its input.
    let f = error_route_flow();
    let plan = Plan::compile(&f).unwrap();
    let error_payload = json!({ "error": { "message": "boom", "code": "HTTP_500" } });
    let records = [Recorded::new("a", "error", error_payload.clone())];
    let mut st = plan.resume("partial", json!({}), &records).unwrap();
    assert_eq!(st.step_seq(), 0, "an error route is not a completed step");
    assert_eq!(st.result(), &Value::Null, "no success has set a result yet");

    // The live remainder: h runs with the error payload as its input, then done.
    let mut seen = Vec::new();
    let status = plan.drive(
        &mut st,
        || 0,
        |_, _| {},
        |d| {
            seen.push((d.node.clone(), d.payload.clone()));
            NodeOutcome::ok(json!({ "handled": true }))
        },
    );
    assert_eq!(status, RunStatus::Completed);
    assert_eq!(seen, [("h".to_string(), error_payload)]);
}

#[test]
fn resume_error_route_still_advances_the_occurrence() {
    // The err-loop shape a->b, b--error-->h, h->b. Resume from [a@main, b@error,
    // h@main]: b's error-routed record must still advance b's visit count, so the
    // NEXT dispatch is b's SECOND visit (occurrence 1, key "r1:b:1"). Proves the
    // new replay path keeps the occurrence-advancing semantics live has.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"err-loop","version":1,
            "trigger":{"type":"manual"},"entry":"a",
            "nodes":[{"id":"a","type":"echo"},{"id":"b","type":"call"},
                     {"id":"h","type":"handler"}],
            "edges":[{"from":"a","to":"b"},
                     {"from":"b","from-port":"error","to":"h"},
                     {"from":"h","to":"b"}]}"#,
    );
    let plan = Plan::compile(&f).unwrap();
    let records = [
        Recorded::new("a", "main", json!({ "at": "a" })),
        Recorded::new("b", "error", json!({ "error": { "message": "boom" } })),
        Recorded::new("h", "main", json!({ "at": "h" })),
    ];
    let mut st = plan.resume("r1", json!({}), &records).unwrap();
    match plan.next(&mut st, 0) {
        Step::Dispatch(d) => {
            assert_eq!(d.node, "b");
            assert_eq!(
                d.occurrence, 1,
                "the replayed error route advanced b's visit count"
            );
            assert_eq!(d.idempotency_key, "r1:b:1");
        }
        other => panic!("expected b's second visit, got {other:?}"),
    }
}

#[test]
fn resume_detects_history_drift() {
    let f = linear4();
    let plan = Plan::compile(&f).unwrap();
    // The first recorded step names "b", but the flow dispatches "a" first.
    let completed = [Recorded::new("b", "main", json!({}))];
    let err = plan.resume("r1", json!({}), &completed).unwrap_err();
    assert_eq!(
        err,
        ResumeError::Mismatch {
            recorded: "b".into(),
            dispatched: "a".into()
        }
    );
}

#[test]
fn resume_rejects_more_records_than_the_flow_walks() {
    let f = linear4();
    let plan = Plan::compile(&f).unwrap();
    // Five records for a four-node flow: the fifth overruns the terminal state.
    let completed: Vec<Recorded> = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|n| Recorded::new(*n, "main", json!({})))
        .collect();
    let err = plan.resume("r1", json!({}), &completed).unwrap_err();
    assert_eq!(err, ResumeError::Overrun { node: "e".into() });
}

#[test]
fn resume_of_a_fully_recorded_run_is_complete_and_idempotent() {
    let f = linear4();
    let plan = Plan::compile(&f).unwrap();
    let completed: Vec<Recorded> = ["a", "b", "c", "d"]
        .iter()
        .map(|n| Recorded::new(*n, "main", json!({ "at": n })))
        .collect();
    let mut st = plan.resume("r1", json!({}), &completed).unwrap();
    // Nothing remains: the driver's first step completes without re-dispatching.
    let mut resumed = Vec::new();
    let status = plan.drive(
        &mut st,
        || 0,
        |_, _| {},
        |d| {
            resumed.push(d.node.clone());
            NodeOutcome::ok(d.payload.clone())
        },
    );
    assert_eq!(status, RunStatus::Completed);
    assert!(resumed.is_empty());
}

// ---- seed_at: partial re-run from a chosen node ---------------------------

#[test]
fn seed_at_runs_only_the_downstream_subtree() {
    let f = linear4();
    let plan = Plan::compile(&f).unwrap();
    // Partial re-run from c with its captured input: a and b are NOT re-run.
    let mut st = plan
        .seed_at("rerun-1", "c", json!({ "captured": "c-input" }))
        .unwrap();
    assert_eq!(st.status(), RunStatus::Running);
    let mut seen = Vec::new();
    let status = plan.drive(
        &mut st,
        || 0,
        |_, _| {},
        |d| {
            seen.push((d.node.clone(), d.payload.clone()));
            NodeOutcome::ok(json!({ "at": d.node }))
        },
    );
    assert_eq!(status, RunStatus::Completed);
    assert_eq!(
        seen,
        [
            ("c".to_string(), json!({ "captured": "c-input" })),
            ("d".to_string(), json!({ "at": "c" })),
        ]
    );
}

#[test]
fn seed_at_unknown_node_is_rejected() {
    let f = linear4();
    let plan = Plan::compile(&f).unwrap();
    let err = plan.seed_at("r", "nope", json!({})).unwrap_err();
    assert_eq!(err, UnknownNode("nope".into()));
}

// ---- dispatch budget: the runaway-loop runtime bound (cjv.4) --------------

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
fn run_bounded(plan: &Plan, st: &mut RunState, max_iters: usize) -> (Vec<String>, RunStatus) {
    let mut dispatched = Vec::new();
    for _ in 0..max_iters {
        match plan.next(st, 0) {
            Step::Done(s) => return (dispatched, s),
            Step::Wait { .. } => panic!("unexpected wait in a budget test"),
            Step::Dispatch(d) => {
                dispatched.push(d.node.clone());
                plan.apply(st, &d, NodeOutcome::ok(json!("loop")), 0);
            }
        }
    }
    panic!("no terminal status within {max_iters} iterations — the dispatch budget did not fire");
}

#[test]
fn a_runaway_cycle_fails_at_exactly_the_budget() {
    let f = runaway_cycle();
    let mut plan = Plan::compile(&f).unwrap();
    plan.set_dispatch_budget(5);
    let mut st = plan.start("r1", json!("go"));
    let (dispatched, status) = run_bounded(&plan, &mut st, 20);
    // Exactly 5 node executions were allowed, then the run failed terminally.
    assert_eq!(dispatched.len(), 5);
    assert_eq!(st.dispatched(), 5);
    assert_eq!(status, RunStatus::Failed);
    let failure = st.failure().expect("failure recorded");
    assert_eq!(failure.kind, FailKind::RunawayBudget);
    // The failure names the node that would have run next (the 6th execution).
    assert_eq!(failure.node, "a");
    assert_eq!(failure.detail.code.as_deref(), Some("runaway-budget"));
}

#[test]
fn a_flow_that_uses_exactly_the_budget_completes() {
    // linear4 dispatches exactly 4 nodes; budget 4 must let it complete (the
    // budget is "may execute N nodes", not "fails at N").
    let f = linear4();
    let mut plan = Plan::compile(&f).unwrap();
    plan.set_dispatch_budget(4);
    let mut st = plan.start("r1", json!("go"));
    let (dispatched, status) = run_bounded(&plan, &mut st, 20);
    assert_eq!(status, RunStatus::Completed);
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
    let mut plan = Plan::compile(&f).unwrap();
    plan.set_dispatch_budget(3);
    let mut st = plan.start("r1", json!("go"));
    let mut executions = 0;
    let status = loop {
        if executions > 20 {
            panic!("budget did not fire");
        }
        // Jump the clock past any scheduled backoff so every retry is due.
        match plan.next(&mut st, u64::MAX / 2) {
            Step::Done(s) => break s,
            Step::Wait { .. } => panic!("retry should be due at a huge now"),
            Step::Dispatch(d) => {
                executions += 1;
                plan.apply(
                    &mut st,
                    &d,
                    NodeOutcome::Error(NodeError::Retryable(wamn_runner::ErrorDetail::msg(
                        "flaky",
                    ))),
                    u64::MAX / 2,
                );
            }
        }
    };
    assert_eq!(executions, 3);
    assert_eq!(status, RunStatus::Failed);
    assert_eq!(st.failure().unwrap().kind, FailKind::RunawayBudget);
}

#[test]
fn reconstruction_is_exempt_from_the_budget() {
    // 4 recorded steps exceed a budget of 3, but resume folds history without
    // counting: the resumed live walk (0 outstanding nodes here) completes.
    let f = linear4();
    let mut plan = Plan::compile(&f).unwrap();
    plan.set_dispatch_budget(3);
    let completed: Vec<Recorded> = ["a", "b", "c", "d"]
        .iter()
        .map(|n| Recorded::new(*n, "main", json!({ "at": n })))
        .collect();
    let mut st = plan.resume("r1", json!("go"), &completed).unwrap();
    assert_eq!(st.dispatched(), 0, "folded history must not count");
    let (dispatched, status) = run_bounded(&plan, &mut st, 10);
    assert_eq!(status, RunStatus::Completed);
    assert!(dispatched.is_empty());

    // A partially-recorded resume still gets the FULL budget for live work:
    // 3 recorded + budget 1 leaves exactly the one outstanding node runnable.
    let mut plan2 = Plan::compile(&f).unwrap();
    plan2.set_dispatch_budget(1);
    let mut st2 = plan2.resume("r2", json!("go"), &completed[..3]).unwrap();
    let (live, status2) = run_bounded(&plan2, &mut st2, 10);
    assert_eq!(status2, RunStatus::Completed);
    assert_eq!(live, ["d"]);
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
    let mut plan = Plan::compile(&f).unwrap();
    plan.set_dispatch_budget(5);
    let mut st = plan.start("r1", json!("go"));
    let (dispatched, status) = run_bounded(&plan, &mut st, 20);
    assert_eq!(status, RunStatus::Failed);
    assert_eq!(st.failure().unwrap().kind, FailKind::RunawayBudget);
    // The rescue node never ran: the verdict bypassed the error path.
    assert!(!dispatched.iter().any(|n| n == "rescue"));
}

#[test]
fn the_default_budget_is_generous_but_finite() {
    let f = runaway_cycle();
    let plan = Plan::compile(&f).unwrap();
    assert_eq!(plan.dispatch_budget(), wamn_runner::DEFAULT_DISPATCH_BUDGET);
    assert_eq!(wamn_runner::DEFAULT_DISPATCH_BUDGET, 10_000);
    let mut st = plan.start("r1", json!("go"));
    let (dispatched, status) = run_bounded(&plan, &mut st, 10_100);
    assert_eq!(status, RunStatus::Failed);
    assert_eq!(dispatched.len(), 10_000);
    assert_eq!(st.failure().unwrap().kind, FailKind::RunawayBudget);
}

// ---------------------------------------------------------------------------
// R32: cross-invocation retry — the durable-queue park→reclaim→reconstruct cycle
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

/// The durable-queue outcome of driving a run across parks (R32): every LIVE
/// dispatch as `(node, attempt)` over ALL claims, how many times it parked, how
/// many claims it took, and the terminal verdict.
struct ParkTrace {
    dispatches: Vec<(String, u32)>,
    parks: usize,
    claims: usize,
    status: RunStatus,
    failure: Option<FailKind>,
}

impl ParkTrace {
    fn steps(&self) -> Vec<(&str, u32)> {
        self.dispatches
            .iter()
            .map(|(n, a)| (n.as_str(), *a))
            .collect()
    }
}

/// Drive a run EXACTLY as the flow-runner's claim path does across durable-queue
/// parks (R32). Each claim reconstructs branch-aware from the recorded completed
/// steps ([`Plan::resume`]) PLUS the persisted in-flight retry cursor
/// ([`Plan::restore_retry`] — the `state_json` the driver round-trips), then
/// drives until the run parks on a [`Step::Wait`] or terminates. A `Wait` is
/// translated to a PARK: the (virtual) queue serves the backoff, so the next
/// claim re-enters DUE — the retry budget must advance across claims via the
/// persisted attempt, or the run never terminates (the exact R32 bug the
/// `max_claims` guard turns into a test failure). Records only SUCCESS emissions
/// as completed steps — enough for these fixtures, where an error-route is always
/// followed by the run terminating in the SAME claim (no park after it).
fn drive_across_parks(
    plan: &Plan,
    run_id: &str,
    input: Value,
    max_claims: usize,
    mut dispatch_fn: impl FnMut(&Dispatch) -> NodeOutcome,
) -> ParkTrace {
    let mut completed: Vec<Recorded> = Vec::new();
    // The persisted state_json cursor: (node, attempt, shared-throttle key). The
    // throttle rides the cursor so it survives park->reclaim exactly as the driver
    // round-trips it (wamn-2jkm.66).
    let mut retry: Option<(String, u32, Option<ThrottleKey>)> = None;
    let mut dispatches = Vec::new();
    let mut parks = 0usize;
    for claim in 1..=max_claims {
        let mut st = plan.resume(run_id, input.clone(), &completed).unwrap();
        if let Some((node, attempt, throttle)) = &retry {
            plan.restore_retry(&mut st, node, *attempt, throttle.clone());
        }
        loop {
            match plan.next(&mut st, 0) {
                Step::Done(status) => {
                    return ParkTrace {
                        dispatches,
                        parks,
                        claims: claim,
                        status,
                        failure: st.failure().map(|f| f.kind),
                    };
                }
                Step::Wait {
                    node,
                    attempt,
                    throttle,
                    ..
                } => {
                    // Persist the budget cursor AND the shared-throttle key, then
                    // park (the driver's state_json round-trip — wamn-2jkm.66).
                    retry = Some((node, attempt, throttle));
                    parks += 1;
                    break;
                }
                Step::Dispatch(d) => {
                    dispatches.push((d.node.clone(), d.attempt));
                    let outcome = dispatch_fn(&d);
                    if let NodeOutcome::Success { payload, port } = &outcome {
                        // Checkpoint the completed node (the driver's node_runs
                        // row) so the next claim folds it instead of re-dispatching.
                        // A now-stale retry cursor is left as-is: restore_retry's
                        // front-check no-ops it once this node is no longer the
                        // frontier front (the driver likewise leans on that guard
                        // rather than clearing state_json on every success).
                        completed.push(Recorded::new(
                            d.node.clone(),
                            port.clone(),
                            payload.clone(),
                        ));
                    }
                    plan.apply(&mut st, &d, outcome, 0);
                }
            }
        }
    }
    panic!(
        "run did not terminate within {max_claims} claims — the retry budget never advanced (R32)"
    );
}

#[test]
fn wait_carries_the_pending_retry_attempt() {
    // The engine's half of the park translation: after a retryable failure the
    // NEXT step is a Wait carrying the attempt the driver persists (1) at the
    // default backoff (100ms). A mutant that zeroes this cursor is caught here.
    let f = one_retryable_node("");
    let plan = Plan::compile(&f).unwrap();
    let mut st = plan.start("r1", json!({}));
    let Step::Dispatch(d) = plan.next(&mut st, 0) else {
        panic!("first step dispatches");
    };
    assert_eq!(d.attempt, 0);
    plan.apply(
        &mut st,
        &d,
        NodeOutcome::Error(NodeError::Retryable(wamn_runner::ErrorDetail::msg("flaky"))),
        0,
    );
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

#[test]
fn restore_retry_promotes_the_outstanding_node_due_now_with_its_attempt() {
    // After a park+reconstruct the retrying node sits at the frontier front with
    // an empty current slot; restore_retry promotes it carrying its persisted
    // attempt, DUE NOW (the queue served the backoff), so the next step is a
    // Dispatch at that attempt — not a fresh attempt-0 promotion, not a re-Wait.
    let f = one_retryable_node("");
    let plan = Plan::compile(&f).unwrap();
    let mut st = plan.resume("r1", json!({}), &[]).unwrap(); // fresh: b outstanding
    assert!(plan.restore_retry(&mut st, "b", 2, None));
    match plan.next(&mut st, 0) {
        Step::Dispatch(d) => {
            assert_eq!(d.node, "b");
            assert_eq!(
                d.attempt, 2,
                "the persisted attempt survived reconstruction"
            );
        }
        other => panic!("expected a due dispatch at the restored attempt, got {other:?}"),
    }
}

#[test]
fn restore_retry_carries_the_persisted_shared_throttle_key() {
    // wamn-2jkm.66: a `rate-limited` retry parks carrying its shared-throttle key
    // (node type, credential, target host). The per-run backoff is served by the
    // queue park, but the cross-run GATE identity must survive the reclaim too —
    // restore_retry must promote the node carrying the ORIGINAL key, not None.
    let f = one_retryable_node("");
    let plan = Plan::compile(&f).unwrap();
    let mut st = plan.resume("r1", json!({}), &[]).unwrap(); // fresh: b outstanding
    let key = ThrottleKey::new("http-call", Some("erp".into()), Some("erp.example".into()));
    assert!(plan.restore_retry(&mut st, "b", 2, Some(key.clone())));
    assert_eq!(
        st.current_throttle(),
        Some(&key),
        "the restored node carries the persisted shared-throttle key"
    );

    // The absent-key round-trip stays None (a plain retryable retry has no gate).
    let mut st2 = plan.resume("r2", json!({}), &[]).unwrap();
    assert!(plan.restore_retry(&mut st2, "b", 1, None));
    assert_eq!(
        st2.current_throttle(),
        None,
        "a retry that carried no throttle restores with no key"
    );
}

#[test]
fn restore_retry_is_a_noop_for_a_node_that_is_not_the_front() {
    // Stale state_json (the node already completed on an earlier claim, so it is
    // no longer the frontier front) must NOT hijack the walk, nor must an unknown
    // node id.
    let f = linear4();
    let plan = Plan::compile(&f).unwrap();
    let mut st = plan.resume("r1", json!("go"), &[]).unwrap(); // front is "a"
    assert!(!plan.restore_retry(&mut st, "c", 5, None)); // c is not the front
    assert!(!plan.restore_retry(&mut st, "nope", 5, None)); // unknown node
    // The walk is untouched: the next dispatch is still the real front, fresh.
    match plan.next(&mut st, 0) {
        Step::Dispatch(d) => {
            assert_eq!(d.node, "a");
            assert_eq!(d.attempt, 0);
        }
        other => panic!("expected a's fresh dispatch, got {other:?}"),
    }
}

#[test]
fn retry_budget_survives_parks_to_exhaustion() {
    // The R32 acceptance case: a node Retryable on attempt 0, no error edge.
    // Across successive claims the run PARKS (never aborts, never holds a lease)
    // and the budget advances via the persisted attempt until it fails
    // RetryExhausted — it does NOT loop forever re-running attempt 0. Default
    // budget = 3 attempts.
    let f = one_retryable_node("");
    let plan = Plan::compile(&f).unwrap();
    let t = drive_across_parks(&plan, "r1", json!({}), 50, |_| {
        NodeOutcome::Error(NodeError::Retryable(wamn_runner::ErrorDetail::msg(
            "always",
        )))
    });
    assert_eq!(t.status, RunStatus::Failed);
    assert_eq!(t.failure, Some(FailKind::RetryExhausted));
    // Attempts 0,1,2 — one per claim — with a park between each.
    assert_eq!(t.steps(), vec![("b", 0), ("b", 1), ("b", 2)]);
    assert_eq!(t.parks, 2);
    assert_eq!(t.claims, 3);
}

#[test]
fn retry_budget_survives_parks_then_error_routes_when_exhausted() {
    // max-attempts=2 and an error edge b--error-->h: across a park the budget
    // exhausts and the run ROUTES to the handler (rather than failing), then
    // completes. Proves the "error-routes OR fails RetryExhausted" acceptance.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"r32e","version":1,
            "trigger":{"type":"manual"},"entry":"b",
            "nodes":[{"id":"b","type":"call","config":{"retry":{"max-attempts":2}}},
                     {"id":"h","type":"handler"}],
            "edges":[{"from":"b","from-port":"error","to":"h"}]}"#,
    );
    let plan = Plan::compile(&f).unwrap();
    let t = drive_across_parks(&plan, "r1", json!({}), 50, |d| match d.node.as_str() {
        "b" => NodeOutcome::Error(NodeError::Retryable(wamn_runner::ErrorDetail::msg("x"))),
        _ => NodeOutcome::ok(json!({ "handled": true })),
    });
    assert_eq!(t.status, RunStatus::Completed);
    assert_eq!(t.parks, 1); // attempt 0 parks; attempt 1 exhausts and routes
    assert_eq!(t.steps(), vec![("b", 0), ("b", 1), ("h", 0)]);
}

#[test]
fn a_completed_predecessor_is_not_re_run_across_a_retry_park() {
    // a -> b: a succeeds once, then b retries across parks and eventually
    // succeeds. Reconstruction folds a's recorded success each claim (a is NOT
    // re-dispatched) while b's attempt advances across the parks.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"r32seq","version":1,
            "trigger":{"type":"manual"},"entry":"a",
            "nodes":[{"id":"a","type":"echo"},{"id":"b","type":"call"}],
            "edges":[{"from":"a","to":"b"}]}"#,
    );
    let plan = Plan::compile(&f).unwrap();
    let b_attempts = Cell::new(0u32);
    let t = drive_across_parks(&plan, "r1", json!("go"), 50, |d| match d.node.as_str() {
        "a" => NodeOutcome::ok(json!({ "at": "a" })),
        _ => {
            let n = b_attempts.replace(b_attempts.get() + 1);
            if n < 2 {
                NodeOutcome::Error(NodeError::Retryable(wamn_runner::ErrorDetail::msg("flaky")))
            } else {
                NodeOutcome::ok(json!({ "at": "b" }))
            }
        }
    });
    assert_eq!(t.status, RunStatus::Completed);
    // a dispatched exactly once; b dispatched at attempts 0,1,2 across 2 parks.
    assert_eq!(t.steps(), vec![("a", 0), ("b", 0), ("b", 1), ("b", 2)]);
    assert_eq!(t.parks, 2);
}

// ---------------------------------------------------------------------------
// SDK contract drift-guards (5.3)
// ---------------------------------------------------------------------------

/// The SDK defines its own port constants (it must not depend on the flow
/// schema crate); this pins them to the engine's `wamn_flow` values.
#[test]
fn sdk_port_constants_mirror_the_flow_schema() {
    assert_eq!(wamn_node_sdk::MAIN_PORT, wamn_runner::MAIN_PORT);
    assert_eq!(wamn_node_sdk::ERROR_PORT, wamn_runner::ERROR_PORT);
}

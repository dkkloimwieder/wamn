//! Walk conformance — the frontier-ordering / port-routing / error-edge floor,
//! exercised with NO cluster, NO DB, NO wasm: build a `Wiring`, drive it with a
//! programmable node invoker, and assert purely from returned `Step`s and the
//! final `Walk`.
//!
//! Ported from `crates/execution/flow-engine/tests/runner.rs`. Every case there
//! that survives the language decoupling is here, case for case; the fixtures
//! changed from flow-document JSON to `Wiring` literals because the flow
//! document is the retired language, and the asserts that counted the engine's
//! synthetic `event` entry node are down by one where that node is gone. The
//! per-case notes below record every changed literal.

use std::cell::Cell;

use serde_json::{Value, json};
use wamn_router::{
    Delivery, ErrorDetail, FailureKind, NodeCall, NodeError, NodeOutcome, RateLimitDetail,
    RetryPolicy, Step, ThrottleKey, Walk, WalkStatus, Wiring, WiringEdge, WiringErrorKind,
    WiringNode,
};

// ---- fixtures -------------------------------------------------------------

/// A node invoking an admitted `component` with no config.
fn node(id: &str, component: &str) -> WiringNode {
    node_cfg(id, component, Value::Null)
}

/// A node invoking an admitted `component` with opaque `config`.
fn node_cfg(id: &str, component: &str, config: Value) -> WiringNode {
    WiringNode {
        id: id.to_string(),
        component: component.to_string(),
        config,
        connection: None,
        terminal: None,
    }
}

/// An edge leaving `from` on the default `main` port, with no ordinal.
fn edge(from: &str, to: &str) -> WiringEdge {
    edge_on(from, "main", to)
}

/// An edge leaving `from` on a named port, with no ordinal.
fn edge_on(from: &str, port: &str, to: &str) -> WiringEdge {
    edge_to(from, port, to, "input")
}

/// An edge selecting one named source output and target input.
fn edge_to(from: &str, from_port: &str, to: &str, to_port: &str) -> WiringEdge {
    WiringEdge {
        from: from.to_string(),
        from_port: from_port.to_string(),
        to: to.to_string(),
        to_port: to_port.to_string(),
        ordinal: None,
    }
}

/// An edge leaving `from` on `main` at an explicit fan-out ordinal.
fn edge_ord(from: &str, to: &str, ordinal: u32) -> WiringEdge {
    WiringEdge {
        ordinal: Some(ordinal),
        ..edge(from, to)
    }
}

fn wiring(entry: &str, nodes: Vec<WiringNode>, edges: Vec<WiringEdge>) -> Wiring {
    Wiring::compile(entry, nodes, edges).expect("fixture wiring compiles")
}

// ---- driver ---------------------------------------------------------------

/// A recorded walk of one delivery to a terminal status.
struct Trace {
    /// Every `NodeCall`, in order.
    invoked: Vec<NodeCall>,
    /// Every `Wait` as `(node, until_ms, throttle)`, in order.
    waits: Vec<(String, u64, Option<ThrottleKey>)>,
    status: WalkStatus,
    walk: Walk,
}

impl Trace {
    /// Node ids invoked, in order.
    fn nodes(&self) -> Vec<&str> {
        self.invoked.iter().map(|c| c.node.as_str()).collect()
    }
}

/// Drive a walk: a `Wait` "sleeps" by jumping a virtual clock to the deadline; an
/// `Invoke` calls `invoke_fn`. Records the whole trace.
fn run(
    wiring: &Wiring,
    delivery_id: &str,
    payload: Value,
    mut invoke_fn: impl FnMut(&NodeCall) -> NodeOutcome,
) -> Trace {
    let clock = Cell::new(0u64);
    let mut invoked = Vec::new();
    let mut waits = Vec::new();
    let mut walk = wiring.start(Delivery {
        id: delivery_id.to_string(),
        payload,
        caller_attached: false,
    });
    let status = loop {
        match wiring.next(&mut walk, clock.get()) {
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
            Step::Invoke(call) => {
                invoked.push(call.clone());
                let outcome = invoke_fn(&call);
                wiring
                    .apply(&mut walk, &call, outcome, clock.get())
                    .unwrap();
            }
        }
    };
    Trace {
        invoked,
        waits,
        status,
        walk,
    }
}

/// Drive with a hard iteration ceiling so a hop-limit-removed mutant FAILS the
/// assert instead of hanging the test binary (the plain `run` helper loops until
/// terminal, which a runaway mutant never reaches).
fn run_bounded(wiring: &Wiring, walk: &mut Walk, max_iters: usize) -> (Vec<String>, WalkStatus) {
    let mut invoked = Vec::new();
    for _ in 0..max_iters {
        match wiring.next(walk, 0) {
            Step::Done(s) => return (invoked, s),
            Step::Wait { .. } => panic!("unexpected wait in a hop-limit test"),
            Step::Invoke(call) => {
                invoked.push(call.node.clone());
                wiring
                    .apply(walk, &call, NodeOutcome::ok(json!("loop")), 0)
                    .unwrap();
            }
        }
    }
    panic!("no terminal status within {max_iters} iterations — the hop limit did not fire");
}

fn delivery(payload: Value) -> Delivery {
    Delivery {
        id: "d1".to_string(),
        payload,
        caller_attached: false,
    }
}

// ---- walk: linear / branch / merge / fan-out ------------------------------

#[test]
fn linear_walk_completes_in_order() {
    let w = wiring(
        "a",
        vec![node("a", "echo"), node("b", "echo"), node("c", "echo")],
        vec![edge("a", "b"), edge("b", "c")],
    );
    // Each node emits a payload naming itself, so the result is the last node's.
    let t = run(&w, "r1", json!({ "seen": [] }), |c| {
        NodeOutcome::ok(json!({ "at": c.node }))
    });
    assert_eq!(t.status, WalkStatus::Completed);
    assert_eq!(t.nodes(), ["a", "b", "c"]);
    assert_eq!(t.walk.step_seq(), 3); // engine asserted 4: + its synthetic event entry
    assert_eq!(t.walk.result(), &json!({ "at": "c" }));
    // Each node's input payload is the upstream node's output.
    assert_eq!(t.invoked[0].payload, json!({ "seen": [] })); // the entry gets the delivery payload
    assert_eq!(t.invoked[1].payload, json!({ "at": "a" })); // b sees a's output
    assert_eq!(t.invoked[2].payload, json!({ "at": "b" })); // c sees b's output
}

#[test]
fn destination_input_port_reaches_each_invocation_and_survives_retry() {
    let w = wiring(
        "source",
        vec![node("source", "emit"), node("target", "consume")],
        vec![edge_to("source", "record", "target", "batch")],
    );
    let target_attempt = Cell::new(0);
    let trace = run(&w, "r1", json!({}), |call| {
        if call.node == "source" {
            return NodeOutcome::ok_on(json!({"records": []}), "record");
        }
        if target_attempt.replace(target_attempt.get() + 1) == 0 {
            NodeOutcome::Error(NodeError::Retryable(ErrorDetail::msg("again")))
        } else {
            NodeOutcome::ok(json!({}))
        }
    });

    assert_eq!(trace.invoked[0].input_port, None);
    assert_eq!(trace.invoked[1].input_port.as_deref(), Some("batch"));
    assert_eq!(trace.invoked[2].input_port.as_deref(), Some("batch"));
}

#[test]
fn branch_follows_only_the_selected_port() {
    let w = wiring(
        "cond",
        vec![
            node("cond", "conditional"),
            node("yes", "echo"),
            node("no", "echo"),
        ],
        vec![
            edge_on("cond", "true", "yes"),
            edge_on("cond", "false", "no"),
        ],
    );
    let t = run(&w, "r1", json!({}), |c| match c.node.as_str() {
        "cond" => NodeOutcome::ok_on(json!({ "picked": true }), "true"),
        _ => NodeOutcome::ok(json!({ "at": c.node })),
    });
    assert_eq!(t.status, WalkStatus::Completed);
    assert_eq!(t.nodes(), ["cond", "yes"]); // "no" never runs
}

/// s fans out on main to a and b; both edge into m -> m runs once per arrival.
fn fan_out_wiring() -> Wiring {
    wiring(
        "s",
        vec![
            node("s", "echo"),
            node("a", "echo"),
            node("b", "echo"),
            node("m", "echo"),
        ],
        vec![
            edge("s", "a"),
            edge("s", "b"),
            edge("a", "m"),
            edge("b", "m"),
        ],
    )
}

#[test]
fn fan_out_and_merge_without_a_join_barrier() {
    let w = fan_out_wiring();
    let t = run(&w, "r1", json!({}), |c| {
        NodeOutcome::ok(json!({ "at": c.node }))
    });
    assert_eq!(t.status, WalkStatus::Completed);
    // BFS order: s, then a, b, then m (from a), m (from b).
    assert_eq!(t.nodes(), ["s", "a", "b", "m", "m"]);
    assert_eq!(t.walk.step_seq(), 5); // engine asserted 6: + its synthetic event entry
}

#[test]
fn fan_out_order_follows_the_explicit_edge_ordinal() {
    // Fan-out order is the explicit `WiringEdge::ordinal`, not the edge's
    // position in the array. Here `s` declares a before b but ordinals say b
    // first, so b must run first.
    let w = wiring(
        "s",
        vec![node("s", "echo"), node("a", "echo"), node("b", "echo")],
        vec![edge_ord("s", "a", 1), edge_ord("s", "b", 0)],
    );
    let t = run(&w, "r1", json!({}), |c| {
        NodeOutcome::ok(json!({ "at": c.node }))
    });
    assert_eq!(t.status, WalkStatus::Completed);
    assert_eq!(
        t.nodes(),
        ["s", "b", "a"],
        "array position must not decide fan-out order"
    );
}

#[test]
fn merge_visits_carry_distinct_occurrences() {
    // A merge runs once per arriving token; each visit is its own occurrence, so
    // a caller keying per-visit records never collides them.
    let w = fan_out_wiring();
    let t = run(&w, "r1", json!({}), |c| {
        NodeOutcome::ok(json!({ "at": c.node }))
    });
    assert_eq!(t.status, WalkStatus::Completed);
    let visits: Vec<(&str, u32)> = t
        .invoked
        .iter()
        .map(|c| (c.node.as_str(), c.occurrence))
        .collect();
    assert_eq!(
        visits,
        [("s", 0), ("a", 0), ("b", 0), ("m", 0), ("m", 1)],
        "each arrival at the merge is a distinct occurrence"
    );
}

#[test]
fn occurrence_is_stable_across_retries_of_one_visit() {
    // Retries share the visit (attempt bumps, occurrence does not) — the record
    // identity is per-visit, not per-attempt.
    let w = wiring("b", vec![node("b", "call")], vec![]);
    let attempts = Cell::new(0u32);
    let t = run(&w, "r1", json!({}), |_| {
        let n = attempts.replace(attempts.get() + 1);
        if n < 2 {
            NodeOutcome::Error(NodeError::Retryable(ErrorDetail::msg("x")))
        } else {
            NodeOutcome::ok(json!({}))
        }
    });
    assert_eq!(t.status, WalkStatus::Completed);
    assert_eq!(t.invoked.len(), 3);
    assert!(t.invoked.iter().all(|c| c.occurrence == 0));
    assert_eq!(t.invoked[2].attempt, 2);
}

#[test]
fn an_error_routed_visit_advances_the_occurrence() {
    // b's first visit error-routes (a COMPLETED visit), h loops back, and b's
    // second visit must be occurrence 1: a caller keying records off occurrence
    // would otherwise collide the revisit with the recorded error visit.
    let w = wiring(
        "a",
        vec![node("a", "echo"), node("b", "call"), node("h", "handler")],
        vec![edge("a", "b"), edge_on("b", "error", "h"), edge("h", "b")],
    );
    let first = Cell::new(true);
    let t = run(&w, "r1", json!({}), |c| match c.node.as_str() {
        "b" if first.replace(false) => {
            NodeOutcome::Error(NodeError::Terminal(ErrorDetail::msg("boom")))
        }
        _ => NodeOutcome::ok(json!({ "at": c.node })),
    });
    assert_eq!(t.status, WalkStatus::Completed);
    let visits: Vec<(&str, u32)> = t
        .invoked
        .iter()
        .map(|c| (c.node.as_str(), c.occurrence))
        .collect();
    assert_eq!(visits, [("a", 0), ("b", 0), ("h", 0), ("b", 1)]);
}

#[test]
fn a_leaf_with_no_successors_just_ends() {
    let w = wiring("a", vec![node("a", "echo")], vec![]);
    let t = run(&w, "r1", json!({ "x": 1 }), |_| {
        NodeOutcome::ok(json!({ "done": true }))
    });
    assert_eq!(t.status, WalkStatus::Completed);
    assert_eq!(t.nodes(), ["a"]);
    assert_eq!(t.walk.result(), &json!({ "done": true }));
}

// ---- error paths ----------------------------------------------------------

#[test]
fn terminal_error_routes_to_error_port_and_continues() {
    // a -> b, b has main->c and error->h. b fails terminally -> h runs, c does not.
    let w = wiring(
        "a",
        vec![
            node("a", "echo"),
            node("b", "call"),
            node("c", "echo"),
            node("h", "handler"),
        ],
        vec![edge("a", "b"), edge("b", "c"), edge_on("b", "error", "h")],
    );
    let t = run(&w, "r1", json!({}), |c| match c.node.as_str() {
        "b" => NodeOutcome::Error(NodeError::Terminal(ErrorDetail {
            message: "boom".into(),
            code: Some("HTTP_500".into()),
            data: None,
        })),
        _ => NodeOutcome::ok(json!({ "at": c.node })),
    });
    assert_eq!(t.status, WalkStatus::Completed); // error was handled
    assert_eq!(t.nodes(), ["a", "b", "h"]); // c skipped
    // The handler received the error payload.
    assert_eq!(
        t.invoked.last().unwrap().node,
        "h",
        "handler ran last: {:?}",
        t.nodes()
    );
    assert_eq!(
        t.invoked.last().unwrap().payload,
        json!({"error": {"message": "boom", "code": "HTTP_500"}}),
    );
}

#[test]
fn terminal_error_with_no_error_path_fails_the_walk() {
    let w = wiring(
        "a",
        vec![node("a", "echo"), node("b", "call")],
        vec![edge("a", "b")],
    );
    let t = run(&w, "r1", json!({}), |c| match c.node.as_str() {
        "b" => NodeOutcome::Error(NodeError::Terminal(ErrorDetail::msg("boom"))),
        _ => NodeOutcome::ok(json!({})),
    });
    assert_eq!(t.status, WalkStatus::Failed);
    let fail = t.walk.failure().expect("failure recorded");
    assert_eq!(fail.node, "b");
    assert_eq!(fail.kind, FailureKind::Terminal);
    assert_eq!(fail.detail.message, "boom");
}

// ---- retries / backoff ----------------------------------------------------

#[test]
fn retryable_retries_then_succeeds() {
    let w = wiring("b", vec![node("b", "call")], vec![]);
    let attempts = Cell::new(0u32);
    let t = run(&w, "run-9", json!({}), |_| {
        let n = attempts.get();
        attempts.set(n + 1);
        if n < 2 {
            NodeOutcome::Error(NodeError::Retryable(ErrorDetail::msg("try again")))
        } else {
            NodeOutcome::ok(json!({ "ok": true }))
        }
    });
    assert_eq!(t.status, WalkStatus::Completed);
    // 3 invocations (attempt 0,1,2), 2 waits at the default backoff (100, then 300).
    assert_eq!(t.nodes(), ["b", "b", "b"]);
    assert_eq!(t.invoked[0].attempt, 0);
    assert_eq!(t.invoked[2].attempt, 2);
    assert_eq!(t.waits.len(), 2);
    assert_eq!(t.waits[0].1, 100); // now(0) + backoff(0)=100
    assert_eq!(t.waits[1].1, 300); // now(100) + backoff(1)=200
    assert!(t.waits.iter().all(|(_, _, thr)| thr.is_none())); // plain retryable, no throttle
    // step_seq counts only the one successful completion.
    assert_eq!(t.walk.step_seq(), 1); // engine asserted 2: + its synthetic event entry
}

#[test]
fn retry_budget_exhausts_to_failure() {
    let w = wiring("b", vec![node("b", "call")], vec![]);
    let t = run(&w, "r1", json!({}), |_| {
        NodeOutcome::Error(NodeError::Retryable(ErrorDetail::msg("nope")))
    });
    assert_eq!(t.status, WalkStatus::Failed);
    assert_eq!(t.nodes().len(), 3); // default max_attempts = 3
    assert_eq!(t.walk.failure().unwrap().kind, FailureKind::RetryExhausted);
}

#[test]
fn retry_config_overrides_budget_and_routes_to_error_path_when_exhausted() {
    // max-attempts=2 via config; b--error-->h catches the exhaustion.
    let w = wiring(
        "b",
        vec![
            node_cfg(
                "b",
                "call",
                json!({"retry": {"max-attempts": 2, "base-ms": 10}}),
            ),
            node("h", "handler"),
        ],
        vec![edge_on("b", "error", "h")],
    );
    let t = run(&w, "r1", json!({}), |c| match c.node.as_str() {
        "b" => NodeOutcome::Error(NodeError::Retryable(ErrorDetail::msg("x"))),
        _ => NodeOutcome::ok(json!({ "handled": true })),
    });
    assert_eq!(t.status, WalkStatus::Completed);
    assert_eq!(t.nodes(), ["b", "b", "h"]); // 2 attempts then the error edge
    assert_eq!(t.waits[0].1, 10); // base-ms override
}

#[test]
fn rate_limited_honors_retry_after_and_emits_the_shared_throttle_key() {
    let w = wiring("call", vec![node("call", "http-call")], vec![]);
    let first = Cell::new(true);
    let t = run(&w, "r1", json!({}), |_| {
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
    assert_eq!(t.status, WalkStatus::Completed);
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
    let w = wiring(
        "b",
        vec![node_cfg("b", "call", json!({"retry": {"max-attempts": 9}}))],
        vec![],
    );
    let t = run(&w, "r1", json!({}), |_| {
        NodeOutcome::Error(NodeError::InvalidInput(ErrorDetail::msg("bad shape")))
    });
    assert_eq!(t.status, WalkStatus::Failed);
    assert_eq!(t.nodes().len(), 1); // exactly one invocation, no retry
    assert_eq!(t.walk.failure().unwrap().kind, FailureKind::InvalidInput);
}

// ---- node-call ABI fields -------------------------------------------------

#[test]
fn node_call_carries_component_config_and_deadline_without_wiring_credentials() {
    let w = wiring(
        "n",
        vec![node_cfg(
            "n",
            "http-call",
            json!({"url": "https://x", "deadline-ms": 5000}),
        )],
        vec![],
    );
    let t = run(&w, "r1", json!({}), |_| NodeOutcome::ok(json!({})));
    let c = &t.invoked[0];
    assert_eq!(c.component, "http-call");
    assert_eq!(c.input_port, None);
    assert_eq!(c.occurrence, 0);
    assert_eq!(c.credential, None);
    assert_eq!(c.deadline_ms, Some(5000));
    assert_eq!(c.config["url"], json!("https://x"));
}

#[test]
fn cancelled_is_terminal_without_failure_verdict_or_successor() {
    let w = wiring(
        "a",
        vec![node("a", "first"), node("b", "second")],
        vec![edge("a", "b")],
    );

    let t = run(&w, "r1", json!({}), |_| NodeOutcome::Cancelled);

    assert_eq!(t.status, WalkStatus::Cancelled);
    assert_eq!(t.nodes(), ["a"]);
    assert!(t.walk.failure().is_none());
    assert!(t.walk.verdict().is_none());
    assert_eq!(t.walk.step_seq(), 0);
}

// ---- wiring compilation guard ---------------------------------------------

#[test]
fn compile_rejects_an_unresolved_entry() {
    // entry points at a node that does not exist -> compile error.
    let err = Wiring::compile("missing", vec![node("a", "echo")], vec![]).unwrap_err();
    assert!(matches!(err.kind(), WiringErrorKind::UnresolvedEntry(id) if id == "missing"));
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

// ---- hop limit: the runaway-loop runtime bound ----------------------------

/// A 4-node linear wiring a -> b -> c -> d.
fn linear4() -> Wiring {
    wiring(
        "a",
        vec![
            node("a", "echo"),
            node("b", "echo"),
            node("c", "echo"),
            node("d", "echo"),
        ],
        vec![edge("a", "b"), edge("b", "c"), edge("c", "d")],
    )
}

/// A permitted 2-node cycle with no exit: `in → a → b → a → …`. Loops are a
/// wiring feature, so termination is bounded at runtime by the hop limit, not at
/// compile time.
fn runaway_cycle() -> Wiring {
    wiring(
        "in",
        vec![node("in", "echo"), node("a", "echo"), node("b", "echo")],
        vec![edge("in", "a"), edge("a", "b"), edge("b", "a")],
    )
}

#[test]
fn a_runaway_cycle_fails_at_exactly_the_hop_limit() {
    let mut w = runaway_cycle();
    w.set_hop_limit(5);
    let mut walk = w.start(delivery(json!("go")));
    let (invoked, status) = run_bounded(&w, &mut walk, 20);
    // Five invocations are allowed (in, a, b, a, b), then the walk fails
    // terminally at the configured total of five. The engine asserted 4 here:
    // one of its five units went to the synthetic event entry.
    assert_eq!(invoked.len(), 5);
    assert_eq!(walk.hops(), 5);
    assert_eq!(status, WalkStatus::Failed);
    let failure = walk.failure().expect("failure recorded");
    assert_eq!(failure.kind, FailureKind::HopLimit);
    // The failure names the node that would have run next (the 6th invocation).
    // The engine named "b" — its walk was one node further along.
    assert_eq!(failure.node, "a");
    assert_eq!(failure.detail.code.as_deref(), Some("hop-limit"));
}

#[test]
fn a_wiring_that_uses_exactly_the_hop_limit_completes() {
    // linear4 invokes exactly 4 nodes; a limit of 4 must let the walk complete
    // (the limit is "may invoke N nodes", not "fails at N"). The engine used 5
    // for the same shape because of its synthetic event entry.
    let mut w = linear4();
    w.set_hop_limit(4);
    let mut walk = w.start(delivery(json!("go")));
    let (invoked, status) = run_bounded(&w, &mut walk, 20);
    assert_eq!(status, WalkStatus::Completed);
    assert_eq!(invoked.len(), 4);
    assert!(walk.failure().is_none());
}

#[test]
fn retries_count_against_the_hop_limit() {
    // A node that never stops failing retryable would burn its retry budget —
    // but with a hop limit below the retry allowance, the walk fails HopLimit
    // first: every invocation (retries included) counts.
    let w = {
        let mut w = wiring(
            "x",
            vec![node_cfg(
                "x",
                "echo",
                json!({"retry": {"max-attempts": 10, "base-ms": 0}}),
            )],
            vec![],
        );
        w.set_hop_limit(3);
        w
    };
    let mut walk = w.start(delivery(json!("go")));
    let mut executions = 0;
    let status = loop {
        if executions > 20 {
            panic!("hop limit did not fire");
        }
        // Jump the clock past any scheduled backoff so every retry is due.
        match w.next(&mut walk, u64::MAX / 2) {
            Step::Done(s) => break s,
            Step::Wait { .. } => panic!("retry should be due at a huge now"),
            Step::Invoke(call) => {
                executions += 1;
                w.apply(
                    &mut walk,
                    &call,
                    NodeOutcome::Error(NodeError::Retryable(ErrorDetail::msg("flaky"))),
                    u64::MAX / 2,
                )
                .unwrap();
            }
        }
    };
    assert_eq!(executions, 3); // engine asserted 2: + its synthetic event entry
    assert_eq!(walk.hops(), 3);
    assert_eq!(status, WalkStatus::Failed);
    assert_eq!(walk.failure().unwrap().kind, FailureKind::HopLimit);
}

#[test]
fn the_hop_limit_verdict_is_terminal_even_with_an_error_path() {
    // The looping node has an error edge to a rescue node — which must NOT catch
    // the hop-limit verdict (an error path can itself be part of the loop).
    let mut w = wiring(
        "in",
        vec![
            node("in", "echo"),
            node("a", "echo"),
            node("b", "echo"),
            node("rescue", "echo"),
        ],
        vec![
            edge("in", "a"),
            edge("a", "b"),
            edge("b", "a"),
            edge_on("a", "error", "rescue"),
        ],
    );
    w.set_hop_limit(5);
    let mut walk = w.start(delivery(json!("go")));
    let (invoked, status) = run_bounded(&w, &mut walk, 20);
    assert_eq!(status, WalkStatus::Failed);
    assert_eq!(walk.failure().unwrap().kind, FailureKind::HopLimit);
    // The rescue node never ran: the verdict bypassed the error path.
    assert!(!invoked.iter().any(|n| n == "rescue"));
}

#[test]
fn the_default_hop_limit_is_generous_but_finite() {
    let w = runaway_cycle();
    assert_eq!(w.hop_limit(), wamn_router::DEFAULT_HOP_LIMIT);
    assert_eq!(wamn_router::DEFAULT_HOP_LIMIT, 10_000);
    let mut walk = w.start(delivery(json!("go")));
    let (invoked, status) = run_bounded(&w, &mut walk, 10_100);
    assert_eq!(status, WalkStatus::Failed);
    assert_eq!(invoked.len(), 10_000); // engine asserted 9_999: + its synthetic event entry
    assert_eq!(walk.hops(), 10_000);
    assert_eq!(walk.failure().unwrap().kind, FailureKind::HopLimit);
}

// ---- in-memory retry cursor ------------------------------------------------

#[test]
fn wait_carries_the_pending_retry_attempt() {
    // After a retryable failure the next in-memory step is a Wait carrying the
    // attempt (1) at the default backoff (100ms). A mutant that zeroes this
    // cursor is caught here.
    let w = wiring("b", vec![node("b", "call")], vec![]);
    let mut walk = w.start(delivery(json!({})));
    let Step::Invoke(call) = w.next(&mut walk, 0) else {
        panic!("first step invokes");
    };
    assert_eq!(call.attempt, 0);
    w.apply(
        &mut walk,
        &call,
        NodeOutcome::Error(NodeError::Retryable(ErrorDetail::msg("flaky"))),
        0,
    )
    .unwrap();
    match w.next(&mut walk, 0) {
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

// ---- the public entry point ------------------------------------------------
//
// NOT a ported case: the engine had no test for `drive`, but `route` is the one
// entry point wamn-0h0g.16.1 mandates, so it gets one.

#[test]
fn route_drives_a_wiring_to_a_terminal_outcome() {
    struct Invoker {
        clock: u64,
        seen: Vec<String>,
    }
    impl wamn_router::NodeInvoker for Invoker {
        fn invoke(&mut self, call: &NodeCall) -> NodeOutcome {
            self.seen.push(call.node.clone());
            NodeOutcome::ok(json!({ "at": call.node }))
        }
        fn now_ms(&mut self) -> u64 {
            self.clock
        }
        fn wait_until(&mut self, until_ms: u64, _throttle: Option<&ThrottleKey>) {
            self.clock = until_ms;
        }
    }

    let w = wiring(
        "a",
        vec![node("a", "echo"), node("b", "echo")],
        vec![edge("a", "b")],
    );
    let mut invoker = Invoker {
        clock: 0,
        seen: Vec::new(),
    };
    let outcome = wamn_router::route(delivery(json!({})), &w, &mut invoker);
    assert_eq!(outcome.status, WalkStatus::Completed);
    assert_eq!(invoker.seen, ["a", "b"]);
    assert_eq!(outcome.result, json!({ "at": "b" }));
    assert_eq!(outcome.hops, 2);
    assert!(outcome.failure.is_none());
}

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
//!
//! The last section is the D1 walk simulator (`wamn-54b0.1`): the same builders
//! and the same fake clock, driven from a `proptest` seed instead of a hand-written
//! fixture, checking WALK-1..6 after every `apply`.

use std::cell::Cell;

use proptest::prelude::*;
use serde_json::{Value, json};
use wamn_router::invariants::{self, WalkState, WalkTrace};
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
        operation: component.to_string(),
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

// ---- D1: the seeded walk simulator (wamn-54b0.1) ---------------------------
//
// A random wiring and a random outcome script from one `proptest` seed, driven
// through the same `next`/`apply` loop the cases above use, with the same fake
// clock: it moves only when this harness moves it, on a `Wait`, and never on its
// own. WALK-1..6 (`wamn_router::invariants`) are checked after every `apply`.
// A failure shrinks to the smallest wiring and script that still breaks an
// invariant, and `proptest` prints the seed to replay it.

/// The ports a generated node may emit on: the default, the reserved error path,
/// and one ordinary branch. Three is enough to produce branches, merges and
/// error routes; more only widens the search without reaching new walk decisions.
const SIM_PORTS: [&str; 3] = ["main", "error", "alt"];

/// One generated node — the retry policy is the only config the walk reads.
#[derive(Debug, Clone)]
struct NodeSpec {
    max_attempts: u32,
    base_ms: u64,
}

/// One generated edge, as indices into the generated node list.
#[derive(Debug, Clone)]
struct EdgeSpec {
    from: usize,
    port: usize,
    to: usize,
    ordinal: u32,
}

/// One drawn invocation outcome — the seven variants of `outcome.rs`, weighted.
#[derive(Debug, Clone)]
enum OutcomeSpec {
    Success(usize),
    Retryable,
    RateLimited(Option<u64>),
    Terminal,
    InvalidInput,
    Cancelled,
}

/// One generated walk: a wiring, a hop limit, and the script its invocations
/// draw from.
#[derive(Debug, Clone)]
struct Scenario {
    nodes: Vec<NodeSpec>,
    edges: Vec<EdgeSpec>,
    hop_limit: u64,
    script: Vec<OutcomeSpec>,
}

fn node_spec() -> impl Strategy<Value = NodeSpec> {
    // A zero base keeps some cases free of `Wait` steps entirely; the others
    // exercise the backoff curve and the clock the harness moves.
    (1u32..=3, prop::sample::select(vec![0u64, 10, 100]))
        .prop_map(|(max_attempts, base_ms)| NodeSpec {
            max_attempts,
            base_ms,
        })
}

fn edge_spec(node_count: usize) -> impl Strategy<Value = EdgeSpec> {
    // `to` is unconstrained, so cycles and self-edges are drawn like any other
    // shape: a loop is a wiring feature the hop limit bounds, not an error.
    (0..node_count, 0..SIM_PORTS.len(), 0..node_count, 0u32..3).prop_map(
        |(from, port, to, ordinal)| EdgeSpec {
            from,
            port,
            to,
            ordinal,
        },
    )
}

fn outcome_spec() -> impl Strategy<Value = OutcomeSpec> {
    prop_oneof![
        6 => (0..SIM_PORTS.len()).prop_map(OutcomeSpec::Success),
        2 => Just(OutcomeSpec::Retryable),
        1 => proptest::option::of(0u64..500).prop_map(OutcomeSpec::RateLimited),
        2 => Just(OutcomeSpec::Terminal),
        1 => Just(OutcomeSpec::InvalidInput),
        1 => Just(OutcomeSpec::Cancelled),
    ]
}

fn scenario() -> impl Strategy<Value = Scenario> {
    // Node count is drawn first so edges can index into it directly; shrinking
    // walks it back down, which is what puts a counterexample under five nodes.
    prop::collection::vec(node_spec(), 1..=6).prop_flat_map(|nodes| {
        let count = nodes.len();
        (
            Just(nodes),
            prop::collection::vec(edge_spec(count), 0..=8),
            4u64..=32,
            prop::collection::vec(outcome_spec(), 1..=12),
        )
            .prop_map(|(nodes, edges, hop_limit, script)| Scenario {
                nodes,
                edges,
                hop_limit,
                script,
            })
    })
}

/// Compile a scenario through the same builders the hand-written cases use.
fn sim_wiring(scenario: &Scenario) -> Wiring {
    let nodes = scenario
        .nodes
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            node_cfg(
                &format!("n{i}"),
                "sim",
                json!({"retry": {"max-attempts": spec.max_attempts, "base-ms": spec.base_ms}}),
            )
        })
        .collect();
    let edges = scenario
        .edges
        .iter()
        .map(|spec| WiringEdge {
            ordinal: Some(spec.ordinal),
            ..edge_on(
                &format!("n{}", spec.from),
                SIM_PORTS[spec.port],
                &format!("n{}", spec.to),
            )
        })
        .collect();
    let mut compiled = wiring("n0", nodes, edges);
    compiled.set_hop_limit(scenario.hop_limit);
    compiled
}

fn sim_outcome(spec: &OutcomeSpec) -> NodeOutcome {
    match spec {
        OutcomeSpec::Success(port) => {
            NodeOutcome::ok_on(json!({ "port": SIM_PORTS[*port] }), SIM_PORTS[*port])
        }
        OutcomeSpec::Retryable => NodeOutcome::Error(NodeError::Retryable(ErrorDetail::msg("sim"))),
        OutcomeSpec::RateLimited(retry_after_ms) => {
            NodeOutcome::Error(NodeError::RateLimited(RateLimitDetail {
                detail: ErrorDetail::msg("sim"),
                retry_after_ms: *retry_after_ms,
                target_host: None,
            }))
        }
        OutcomeSpec::Terminal => NodeOutcome::Error(NodeError::Terminal(ErrorDetail::msg("sim"))),
        OutcomeSpec::InvalidInput => {
            NodeOutcome::Error(NodeError::InvalidInput(ErrorDetail::msg("sim")))
        }
        OutcomeSpec::Cancelled => NodeOutcome::Cancelled,
    }
}

/// Walk one scenario to a terminal status, checking WALK-1..6 after every
/// `apply`. The clock starts at 0 and advances only to a `Wait` deadline.
fn simulate(scenario: &Scenario) -> Result<(), TestCaseError> {
    let w = sim_wiring(scenario);
    let mut walk = w.start(delivery(json!({ "sim": true })));
    let mut trace = WalkTrace::new(&w);
    let mut clock = 0u64;
    let mut drawn = 0usize;
    // Each invocation spends one hop and can be preceded by one `Wait`, so a walk
    // still running past this ceiling has outlived its hop limit — WALK-1, caught
    // here rather than by hanging the test binary.
    let ceiling = usize::try_from(scenario.hop_limit).unwrap_or(usize::MAX) * 2 + 4;
    for _ in 0..ceiling {
        let status_before = walk.status();
        let step = w.next(&mut walk, clock);
        trace.stepped(status_before, &step, clock);
        match step {
            Step::Done(_) => {
                return check_invariants(&w, &walk, &trace);
            }
            Step::Wait { until_ms, .. } => clock = until_ms, // virtual sleep
            Step::Invoke(call) => {
                let outcome = sim_outcome(&scenario.script[drawn % scenario.script.len()]);
                drawn += 1;
                // No generated node declares a `Terminal`, so the only refusals
                // left describe a driver feeding back an outcome the walk never
                // handed out. This loop cannot do that, so one is a real defect.
                w.apply(&mut walk, &call, outcome.clone(), clock)
                    .map_err(|refused| TestCaseError::fail(refused.to_string()))?;
                trace.applied(&w, &call, &outcome);
                check_invariants(&w, &walk, &trace)?;
            }
        }
    }
    Err(TestCaseError::fail(
        "WALK-1 violated: the walk did not end within twice its hop limit",
    ))
}

fn check_invariants(w: &Wiring, walk: &Walk, trace: &WalkTrace) -> Result<(), TestCaseError> {
    invariants::check(&WalkState {
        wiring: w,
        walk,
        trace,
    })
    .map_err(|violation| TestCaseError::fail(violation.to_string()))
}

proptest! {
    // `SourceParallel` (the default) looks for a `lib.rs`/`main.rs` beside the
    // test and finds neither here, so it prints no seed at all. `Direct` names
    // the regression file outright, which is what makes a failure replayable:
    // the run prints the `cc <seed>` line to paste into it.
    #![proptest_config(ProptestConfig {
        cases: 10_000,
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(
                "tests/walk.proptest-regressions",
            ),
        )),
        ..ProptestConfig::default()
    })]

    /// WALK-1..6 hold after every `apply`, over a random wiring (fan-out, merges,
    /// permitted cycles, error edges) and a random weighted outcome script.
    #[test]
    fn the_walk_holds_walk_1_through_6(scenario in scenario()) {
        simulate(&scenario)?;
    }
}

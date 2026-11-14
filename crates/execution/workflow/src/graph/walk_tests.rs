fn message(text: &str) -> ErrorDetail {
    ErrorDetail {
        message: text.into(),
        ..Default::default()
    }
}
fn on_port(payload: Value, port: &str) -> NodeOutcome {
    NodeOutcome::Success {
        payload,
        port: port.into(),
    }
}

use std::cell::Cell;

use super::retry::RetryPolicy;
use super::walk::Walk;
use super::wiring::{DEFAULT_HOP_LIMIT, Wiring};
use super::{
    Delivery, ErrorDetail, FailureType, NodeCall, NodeError, NodeOutcome, RateLimitDetail, Step,
    WalkStatus,
};
use serde_json::{Value, json};
use wamn_catalog::{WiringEdge, WiringNode};

fn node(id: &str, component: &str) -> (String, WiringNode) {
    node_cfg(id, component, Value::Null)
}

fn node_cfg(id: &str, component: &str, config: Value) -> (String, WiringNode) {
    (
        id.to_owned(),
        WiringNode {
            interface_version: "0.1.0".into(),
            operation_dependency: None,
            component: component.to_string(),
            operation: component.to_string(),
            params: if config.is_null() {
                Default::default()
            } else {
                serde_json::from_value(config).unwrap()
            },
            terminal: None,
        },
    )
}

fn edge(from: &str, to: &str) -> WiringEdge {
    edge_on(from, "main", to)
}

fn edge_on(from: &str, port: &str, to: &str) -> WiringEdge {
    edge_to(from, port, to, "input")
}

fn edge_to(from: &str, from_port: &str, to: &str, to_port: &str) -> WiringEdge {
    WiringEdge {
        from: from.to_string(),
        from_port: from_port.to_string(),
        to: to.to_string(),
        to_port: Some(to_port.to_string()),
    }
}

fn wiring(entry: &str, nodes: Vec<(String, WiringNode)>, edges: Vec<WiringEdge>) -> Wiring {
    let document = wamn_catalog::WiringDocument::parse(&json!({
        "format-version": "0.1", "wiring-id": "test", "version": 1,
        "entry": entry, "nodes": nodes.into_iter().collect::<std::collections::BTreeMap<_, _>>(), "edges": edges,
    })).expect("catalog fixture admits");
    Wiring::compile(document, &std::collections::BTreeMap::new()).expect("graph indexes")
}

struct Trace<'a> {
    invoked: Vec<NodeCall<'a>>,
    waits: Vec<u64>,
    status: WalkStatus,
    walk: Walk,
}

impl Trace<'_> {
    fn nodes(&self) -> Vec<&str> {
        self.invoked.iter().map(|c| c.node.as_str()).collect()
    }
}

fn run<'a>(
    wiring: &'a Wiring,
    _delivery_id: &str,
    payload: Value,
    mut invoke_fn: impl FnMut(&NodeCall) -> NodeOutcome,
) -> Trace<'a> {
    let clock = Cell::new(0u64);
    let mut invoked = Vec::new();
    let mut waits = Vec::new();
    let mut walk = wiring.start(Delivery {
        payload,
        caller_attached: false,
    });
    let status = loop {
        match wiring.next(&mut walk, clock.get()) {
            Step::Done(s) => break s,
            Step::Wait { until_ms } => {
                waits.push(until_ms);
                clock.set(until_ms);
            }
            Step::Invoke(call) => {
                invoked.push(NodeCall {
                    node: call.node.clone(),
                    input_port: call.input_port.clone(),
                    definition: call.definition,
                    payload: std::sync::Arc::clone(&call.payload),
                    attempt: call.attempt,
                    occurrence: call.occurrence,
                    deadline_ms: call.deadline_ms,
                });
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
        payload,
        caller_attached: false,
    }
}

#[test]
fn linear_walk_completes_in_order() {
    let w = wiring(
        "a",
        vec![node("a", "echo"), node("b", "echo"), node("c", "echo")],
        vec![edge("a", "b"), edge("b", "c")],
    );
    let t = run(&w, "r1", json!({ "seen": [] }), |c| {
        NodeOutcome::ok(json!({ "at": c.node }))
    });
    assert_eq!(t.status, WalkStatus::Completed);
    assert_eq!(t.nodes(), ["a", "b", "c"]);
    assert_eq!(t.walk.result(), &json!({ "at": "c" }));
    assert_eq!(*t.invoked[0].payload, json!({ "seen": [] })); // the entry gets the delivery payload
    assert_eq!(*t.invoked[1].payload, json!({ "at": "a" })); // b sees a's output
    assert_eq!(*t.invoked[2].payload, json!({ "at": "b" })); // c sees b's output
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
            return on_port(json!({"records": []}), "record");
        }
        if target_attempt.replace(target_attempt.get() + 1) == 0 {
            NodeOutcome::Error(NodeError::Retryable(message("again")))
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
        "cond" => on_port(json!({ "picked": true }), "true"),
        _ => NodeOutcome::ok(json!({ "at": c.node })),
    });
    assert_eq!(t.status, WalkStatus::Completed);
    assert_eq!(t.nodes(), ["cond", "yes"]); // "no" never runs
}

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
    assert_eq!(t.nodes(), ["s", "a", "b", "m", "m"]);
}

#[test]
fn merge_visits_carry_distinct_occurrences() {
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
    let w = wiring("b", vec![node("b", "call")], vec![]);
    let attempts = Cell::new(0u32);
    let t = run(&w, "r1", json!({}), |_| {
        let n = attempts.replace(attempts.get() + 1);
        if n < 2 {
            NodeOutcome::Error(NodeError::Retryable(message("x")))
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
    let w = wiring(
        "a",
        vec![node("a", "echo"), node("b", "call"), node("h", "handler")],
        vec![edge("a", "b"), edge_on("b", "error", "h"), edge("h", "b")],
    );
    let first = Cell::new(true);
    let t = run(&w, "r1", json!({}), |c| match c.node.as_str() {
        "b" if first.replace(false) => NodeOutcome::Error(NodeError::Terminal(message("boom"))),
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

#[test]
fn terminal_error_routes_to_error_port_and_continues() {
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
    assert_eq!(
        t.invoked.last().unwrap().node,
        "h",
        "handler ran last: {:?}",
        t.nodes()
    );
    assert_eq!(
        *t.invoked.last().unwrap().payload,
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
        "b" => NodeOutcome::Error(NodeError::Terminal(message("boom"))),
        _ => NodeOutcome::ok(json!({})),
    });
    assert_eq!(t.status, WalkStatus::Failed);
    let fail = t.walk.failure().expect("failure recorded");
    assert_eq!(fail.node, "b");
    assert_eq!(fail.type_, FailureType::Terminal);
}

#[test]
fn retryable_retries_then_succeeds() {
    let w = wiring("b", vec![node("b", "call")], vec![]);
    let attempts = Cell::new(0u32);
    let t = run(&w, "run-9", json!({}), |_| {
        let n = attempts.get();
        attempts.set(n + 1);
        if n < 2 {
            NodeOutcome::Error(NodeError::Retryable(message("try again")))
        } else {
            NodeOutcome::ok(json!({ "ok": true }))
        }
    });
    assert_eq!(t.status, WalkStatus::Completed);
    assert_eq!(t.nodes(), ["b", "b", "b"]);
    assert_eq!(t.invoked[0].attempt, 0);
    assert_eq!(t.invoked[2].attempt, 2);
    assert_eq!(t.waits.len(), 2);
    assert_eq!(t.waits[0], 100); // now(0) + backoff(0)=100
    assert_eq!(t.waits[1], 300); // now(100) + backoff(1)=200
}

#[test]
fn retry_budget_exhausts_to_failure() {
    let w = wiring("b", vec![node("b", "call")], vec![]);
    let t = run(&w, "r1", json!({}), |_| {
        NodeOutcome::Error(NodeError::Retryable(message("nope")))
    });
    assert_eq!(t.status, WalkStatus::Failed);
    assert_eq!(t.nodes().len(), 3); // default max_attempts = 3
    assert_eq!(t.walk.failure().unwrap().type_, FailureType::RetryExhausted);
}

#[test]
fn retry_config_overrides_budget_and_routes_to_error_path_when_exhausted() {
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
        "b" => NodeOutcome::Error(NodeError::Retryable(message("x"))),
        _ => NodeOutcome::ok(json!({ "handled": true })),
    });
    assert_eq!(t.status, WalkStatus::Completed);
    assert_eq!(t.nodes(), ["b", "b", "h"]); // 2 attempts then the error edge
    assert_eq!(t.waits[0], 10); // base-ms override
}

#[test]
fn rate_limited_honors_retry_after() {
    let w = wiring("call", vec![node("call", "http-call")], vec![]);
    let first = Cell::new(true);
    let t = run(&w, "r1", json!({}), |_| {
        if first.replace(false) {
            NodeOutcome::Error(NodeError::RateLimited(RateLimitDetail {
                detail: message("429"),
                retry_after_ms: Some(5000),
            }))
        } else {
            NodeOutcome::ok(json!({ "ok": true }))
        }
    });
    assert_eq!(t.status, WalkStatus::Completed);
    assert_eq!(t.waits.len(), 1);
    assert_eq!(t.waits[0], 5000);
}

#[test]
fn invalid_input_is_never_retried() {
    let w = wiring(
        "b",
        vec![node_cfg("b", "call", json!({"retry": {"max-attempts": 9}}))],
        vec![],
    );
    let t = run(&w, "r1", json!({}), |_| {
        NodeOutcome::Error(NodeError::InvalidInput(message("bad shape")))
    });
    assert_eq!(t.status, WalkStatus::Failed);
    assert_eq!(t.nodes().len(), 1); // exactly one invocation, no retry
    assert_eq!(t.walk.failure().unwrap().type_, FailureType::InvalidInput);
}

#[test]
fn node_call_carries_configuration_and_deadline() {
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
    assert_eq!(c.definition.component, "http-call");
    assert_eq!(c.input_port, None);
    assert_eq!(c.occurrence, 0);
    assert_eq!(c.deadline_ms, Some(5000));
    assert_eq!(c.definition.params["url"], json!("https://x"));
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
}

#[test]
fn retry_policy_reads_config_and_computes_backoff() {
    let d = RetryPolicy::DEFAULT;
    assert_eq!(d.max_attempts, 3);
    assert_eq!(d.backoff_ms(0), 100);
    assert_eq!(d.backoff_ms(1), 200);
    assert_eq!(d.backoff_ms(2), 400);
    assert!(d.may_retry(0) && d.may_retry(1) && !d.may_retry(2));
    let capped = RetryPolicy {
        base_ms: 1000,
        factor: 10.0,
        cap_ms: 5000,
        max_attempts: 10,
    };
    assert_eq!(capped.backoff_ms(0), 1000);
    assert_eq!(capped.backoff_ms(3), 5000); // 1000*1000 capped
    let p = RetryPolicy::from_config(
        &serde_json::from_value(json!({ "retry": { "max-attempts": 5, "base-ms": 50 } })).unwrap(),
    );
    assert_eq!(p.max_attempts, 5);
    assert_eq!(p.base_ms, 50);
    assert_eq!(p.factor.to_bits(), RetryPolicy::DEFAULT.factor.to_bits());
    assert_eq!(
        RetryPolicy::from_config(&Default::default()),
        RetryPolicy::DEFAULT
    );
}

fn runaway_cycle() -> Wiring {
    wiring(
        "in",
        vec![node("in", "echo"), node("a", "echo"), node("b", "echo")],
        vec![edge("in", "a"), edge("a", "b"), edge("b", "a")],
    )
}

#[test]
fn a_runaway_cycle_fails_at_exactly_the_hop_limit() {
    let w = runaway_cycle();
    let mut walk = w.start(delivery(json!("go")));
    let (invoked, status) = run_bounded(
        &w,
        &mut walk,
        usize::try_from(DEFAULT_HOP_LIMIT + 1).unwrap(),
    );
    assert_eq!(invoked.len(), usize::try_from(DEFAULT_HOP_LIMIT).unwrap());
    assert_eq!(walk.hops(), DEFAULT_HOP_LIMIT);
    assert_eq!(status, WalkStatus::Failed);
    let failure = walk.failure().expect("failure recorded");
    assert_eq!(failure.type_, FailureType::HopLimit);
    assert_eq!(failure.detail.code.as_deref(), Some("hop-limit"));
}

#[test]
fn retries_count_against_the_hop_limit() {
    let w = {
        let w = wiring(
            "x",
            vec![node_cfg(
                "x",
                "echo",
                json!({"retry": {"max-attempts": 10001, "base-ms": 0}}),
            )],
            vec![],
        );
        w
    };
    let mut walk = w.start(delivery(json!("go")));
    let mut executions = 0;
    let status = loop {
        assert!(executions <= DEFAULT_HOP_LIMIT, "hop limit did not fire");
        match w.next(&mut walk, executions * 1000) {
            Step::Done(s) => break s,
            Step::Wait { .. } => panic!("retry should be due at a huge now"),
            Step::Invoke(call) => {
                executions += 1;
                w.apply(
                    &mut walk,
                    &call,
                    NodeOutcome::Error(NodeError::Retryable(message("flaky"))),
                    executions * 1000,
                )
                .unwrap();
            }
        }
    };
    assert_eq!(executions, DEFAULT_HOP_LIMIT); // engine asserted 2: + its synthetic event entry
    assert_eq!(walk.hops(), DEFAULT_HOP_LIMIT);
    assert_eq!(status, WalkStatus::Failed);
    assert_eq!(walk.failure().unwrap().type_, FailureType::HopLimit);
}

#[test]
fn the_hop_limit_verdict_is_terminal_even_with_an_error_path() {
    let w = wiring(
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
    let mut walk = w.start(delivery(json!("go")));
    let (invoked, status) = run_bounded(
        &w,
        &mut walk,
        usize::try_from(DEFAULT_HOP_LIMIT + 1).unwrap(),
    );
    assert_eq!(status, WalkStatus::Failed);
    assert_eq!(walk.failure().unwrap().type_, FailureType::HopLimit);
    assert!(!invoked.iter().any(|n| n == "rescue"));
}

#[test]
fn the_default_hop_limit_is_generous_but_finite() {
    let w = runaway_cycle();
    let mut walk = w.start(delivery(json!("go")));
    let (invoked, status) = run_bounded(&w, &mut walk, 10_100);
    assert_eq!(status, WalkStatus::Failed);
    assert_eq!(invoked.len(), 10_000); // engine asserted 9_999: + its synthetic event entry
    assert_eq!(walk.hops(), 10_000);
    assert_eq!(walk.failure().unwrap().type_, FailureType::HopLimit);
}

#[test]
fn a_wiring_that_uses_exactly_the_hop_limit_completes() {
    let w = wiring(
        "loop",
        vec![node("loop", "echo")],
        vec![edge("loop", "loop")],
    );
    let mut calls = 0;
    let trace = run(&w, "limit", json!({}), |_| {
        calls += 1;
        if calls == DEFAULT_HOP_LIMIT {
            on_port(json!({}), "finished")
        } else {
            NodeOutcome::ok(json!({}))
        }
    });
    assert_eq!(trace.status, WalkStatus::Completed);
    assert_eq!(trace.walk.hops(), DEFAULT_HOP_LIMIT);
    assert!(trace.walk.failure().is_none());
}

//! What [`wamn_router::route`] does with an [`ApplyError`](wamn_router::ApplyError)
//! (wamn-0h0g.16.14).
//!
//! `Wiring::apply` refuses a transition it cannot take, and its refusal is
//! atomic so a driver that can re-ask its component sees the walk exactly as it
//! found it. `route` is not such a driver: it hands out one call and feeds back
//! the answer, so for it every refusal is already decided. Three of the six
//! refusals are DATA — a component's returned document, or an authored wiring
//! meeting a delivery it does not fit — and must fail ONE DELIVERY. The other
//! three can only be produced by a driver feeding back an outcome the walk never
//! handed out, which `route` cannot do, so those stay a panic.

use serde_json::{Value, json};
use wamn_router::{
    Delivery, ERROR_PORT, FailureKind, NodeCall, NodeOutcome, Outcome, Terminal, ThrottleKey,
    Verdict, WalkStatus, Wiring, WiringEdge, WiringNode,
};

// ---- fixtures -------------------------------------------------------------

fn node(id: &str, terminal: Option<Terminal>) -> WiringNode {
    WiringNode {
        id: id.to_string(),
        component: "echo".to_string(),
        operation: "run".to_string(),
        config: Value::Null,
        connection: None,
        terminal,
    }
}

fn edge_on(from: &str, port: &str, to: &str) -> WiringEdge {
    WiringEdge {
        from: from.to_string(),
        from_port: port.to_string(),
        to: to.to_string(),
        to_port: "input".to_string(),
        ordinal: None,
    }
}

fn edge(from: &str, to: &str) -> WiringEdge {
    edge_on(from, "main", to)
}

fn wiring(entry: &str, nodes: Vec<WiringNode>, edges: Vec<WiringEdge>) -> Wiring {
    Wiring::compile(entry, nodes, edges).expect("fixture wiring compiles")
}

/// An invoker that answers every call from a closure. No clock and no waiting:
/// nothing here schedules a retry.
struct Scripted<F>(F);

impl<F: FnMut(&NodeCall) -> NodeOutcome> wamn_router::NodeInvoker for Scripted<F> {
    fn invoke(&mut self, call: &NodeCall) -> NodeOutcome {
        (self.0)(call)
    }
    fn now_ms(&mut self) -> u64 {
        0
    }
    fn wait_until(&mut self, _until_ms: u64, _throttle: Option<&ThrottleKey>) {
        panic!("no case here schedules a retry");
    }
}

fn drive(
    w: &Wiring,
    caller_attached: bool,
    answer: impl FnMut(&NodeCall) -> NodeOutcome,
) -> Outcome {
    wamn_router::route(
        Delivery {
            id: "d1".to_string(),
            payload: json!({}),
            caller_attached,
        },
        w,
        &mut Scripted(answer),
    )
}

fn failure(outcome: &Outcome) -> (&str, FailureKind, Option<&str>) {
    let failure = outcome
        .failure
        .as_ref()
        .expect("a failed walk records its failure");
    (
        failure.node.as_str(),
        failure.kind,
        failure.detail.code.as_deref(),
    )
}

// ---- node data: a non-object context replacement ---------------------------

/// The reported defect: `InvalidContext` is GUEST DATA — a component returned a
/// context replacement that is not an object — and it took the whole router
/// process down through `route`'s `.expect(..)`.
#[test]
fn a_non_object_context_replacement_fails_the_delivery_not_the_router() {
    let w = wiring("a", vec![node("a", None)], vec![]);

    let outcome = drive(&w, false, |_| {
        NodeOutcome::ok_with_context(json!({"output": 1}), "main", json!(["merge"]))
    });

    assert_eq!(outcome.status, WalkStatus::Failed);
    assert_eq!(
        failure(&outcome),
        ("a", FailureKind::InvalidContext, Some("invalid-context"))
    );
    assert_eq!(
        outcome.verdict, None,
        "a failure is not a verdict, so the host decides what the caller hears"
    );
}

/// A component's bad context is an ordinary node failure, so it takes the same
/// error edge every other node failure takes — the route wamn-0h0g.16.3 chose
/// for `MissingDedupId` rather than the panicking surface next to it.
#[test]
fn a_non_object_context_replacement_takes_the_nodes_error_edge() {
    let w = wiring(
        "a",
        vec![node("a", None), node("recover", None)],
        vec![edge_on("a", ERROR_PORT, "recover")],
    );

    let mut seen: Vec<Value> = Vec::new();
    let outcome = drive(&w, false, |call| {
        seen.push(call.payload.clone());
        if call.node == "a" {
            NodeOutcome::ok_with_context(json!({"output": 1}), "main", json!("not-an-object"))
        } else {
            NodeOutcome::ok(json!({"recovered": true}))
        }
    });

    assert_eq!(outcome.status, WalkStatus::Completed);
    assert!(outcome.failure.is_none());
    assert_eq!(
        seen[1],
        json!({"error": {
            "code": "invalid-context",
            "message": "context replacement must be an object, got \"not-an-object\"",
        }}),
        "the error edge carries the refusal the walk reported"
    );
}

/// The replacement never lands: the walk's own context is what the recovery node
/// observes, exactly as `Wiring::apply`'s atomic rejection promises a direct
/// driver.
#[test]
fn a_refused_context_replacement_never_reaches_the_error_edge_node() {
    let w = wiring(
        "a",
        vec![node("a", None), node("recover", None)],
        vec![edge_on("a", ERROR_PORT, "recover")],
    );

    let mut contexts: Vec<Value> = Vec::new();
    drive(&w, false, |call| {
        contexts.push(call.context.clone());
        if call.node == "a" {
            NodeOutcome::ok_with_context(json!({"output": 1}), "main", json!(["merge"]))
        } else {
            NodeOutcome::ok(json!({"recovered": true}))
        }
    });

    assert_eq!(contexts, [json!({}), json!({})]);
}

// ---- authored data: a wiring that does not fit its delivery ----------------

/// A `respond` wiring reached by a delivery with no caller (ingress path 2) is
/// an authoring mismatch, not a host-invariant violation: it answers nobody, so
/// it fails its own delivery.
#[test]
fn a_respond_node_without_a_caller_fails_the_delivery_not_the_router() {
    let w = wiring("a", vec![node("a", Some(Terminal::Respond))], vec![]);

    let outcome = drive(&w, false, |_| NodeOutcome::ok(json!({"answer": 1})));

    assert_eq!(outcome.status, WalkStatus::Failed);
    assert_eq!(
        failure(&outcome),
        (
            "a",
            FailureKind::RespondWithoutCaller,
            Some("respond-without-caller")
        )
    );
}

/// One delivery ends once. A fan-out into two terminals is an authored wiring's
/// defect, and the SECOND terminal fails while the first verdict stands.
#[test]
fn a_second_terminal_fails_the_delivery_not_the_router() {
    let w = wiring(
        "fan",
        vec![
            node("fan", None),
            node("first", Some(Terminal::Respond)),
            node("second", Some(Terminal::Respond)),
        ],
        vec![edge("fan", "first"), edge("fan", "second")],
    );

    let outcome = drive(&w, true, |call| NodeOutcome::ok(json!({"at": call.node})));

    assert_eq!(outcome.status, WalkStatus::Failed);
    assert_eq!(
        failure(&outcome),
        ("second", FailureKind::SecondVerdict, Some("second-verdict"))
    );
    assert_eq!(
        outcome.verdict,
        Some(Verdict::Respond {
            payload: json!({"at": "first"})
        }),
        "the first verdict is the one this delivery recorded; the second is the defect"
    );
}

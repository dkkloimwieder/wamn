//! What [`wamn_router::route`] does with an [`ApplyError`](wamn_router::ApplyError)
//! (wamn-0h0g.16.14).
//!
//! `Wiring::apply` refuses a transition it cannot take, and its refusal is
//! atomic so a driver that can re-ask its component sees the walk exactly as it
//! found it. `route` is not such a driver: it hands out one call and feeds back
//! the answer, so for it every refusal is already decided. Two of the five
//! refusals are DATA — an authored wiring meeting a delivery it does not fit —
//! and must fail ONE DELIVERY. The other three can only be produced by a driver
//! feeding back an outcome the walk never handed out, which `route` cannot do,
//! so those stay a panic.

use serde_json::{Value, json};
use wamn_router::{
    Delivery, FailureKind, NodeCall, NodeOutcome, Outcome, Terminal, ThrottleKey, Verdict,
    WalkStatus, Wiring, WiringEdge, WiringNode,
};

// ---- fixtures -------------------------------------------------------------

fn node(id: &str, terminal: Option<Terminal>) -> WiringNode {
    WiringNode {
        id: id.to_string(),
        component: "echo".to_string(),
        operation: "echo".to_string(),
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
            payload: json!({"at": "first"}),
            node_id: "first".to_string(),
        }),
        "the first verdict is the one this delivery recorded; the second is the defect"
    );
}

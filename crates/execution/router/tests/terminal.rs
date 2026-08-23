//! Terminal verdicts — `respond`, `emit`, discard (wamn-0h0g.16.3).
//!
//! The retired engine decided `respond` by comparing a node's reserved type
//! name against the string `"respond"`; the node language retires with it, so
//! here a wiring node DECLARES its [`Terminal`] and the walk reads a variant.
//! These cases pin what each verdict means, what makes one rejected, and — the
//! part a string compare could never express — that `discard` is a decision the
//! walk records rather than the absence of one.

use serde_json::{Value, json};
use wamn_event_wire::Op;
use wamn_router::{
    ApplyErrorKind, DEDUP_ID_FIELD, Delivery, ErrorDetail, FailureKind, NodeError, NodeOutcome,
    Step, Terminal, Verdict, Walk, WalkStatus, Wiring, WiringEdge, WiringNode,
};

// ---- fixtures -------------------------------------------------------------

fn node(id: &str, terminal: Option<Terminal>) -> WiringNode {
    WiringNode {
        id: id.to_string(),
        component: "echo".to_string(),
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

fn started(wiring: &Wiring, caller_attached: bool) -> Walk {
    wiring.start(Delivery {
        id: "d1".to_string(),
        payload: json!({}),
        caller_attached,
    })
}

/// Drive to a terminal status, letting each node emit `outcome_fn`'s answer.
fn run(
    w: &Wiring,
    walk: &mut Walk,
    mut outcome_fn: impl FnMut(&str) -> NodeOutcome,
) -> (Vec<String>, WalkStatus) {
    let mut invoked = Vec::new();
    loop {
        match w.next(walk, 0) {
            Step::Done(status) => return (invoked, status),
            Step::Wait { .. } => panic!("unexpected wait"),
            Step::Invoke(call) => {
                invoked.push(call.node.clone());
                let outcome = outcome_fn(&call.node);
                w.apply(walk, &call, outcome, 0).expect("outcome applies");
            }
        }
    }
}

/// A node emitting a payload that names it.
fn says(node: &str) -> NodeOutcome {
    NodeOutcome::ok(json!({ "at": node }))
}

fn emit_terminal() -> Terminal {
    Terminal::emit("orders", Op::Insert)
}

// ---- respond --------------------------------------------------------------

/// The caller's answer is the RESPOND node's payload, even when the wiring keeps
/// working afterwards — which is why the verdict carries a payload of its own
/// instead of the host reading the walk's `result`.
#[test]
fn respond_answers_the_attached_caller_and_the_walk_carries_on() {
    let w = wiring(
        "a",
        vec![node("a", Some(Terminal::Respond)), node("audit", None)],
        vec![edge("a", "audit")],
    );
    let mut walk = started(&w, true);
    let (invoked, status) = run(&w, &mut walk, says);

    assert_eq!(invoked, ["a", "audit"]);
    assert_eq!(status, WalkStatus::Completed);
    assert_eq!(
        walk.verdict(),
        Some(&Verdict::Respond {
            payload: json!({ "at": "a" }),
            node_id: "a".to_string(),
        })
    );
    // The post-respond node still moved the walk's result along; the caller's
    // answer did not move with it.
    assert_eq!(walk.result(), &json!({ "at": "audit" }));
}

#[test]
fn respond_node_attribution_is_wiring_owned_not_guest_echoed() {
    let w = wiring(
        "terminal-owned",
        vec![node("terminal-owned", Some(Terminal::Respond))],
        vec![],
    );
    let mut walk = started(&w, true);
    let forged = json!({"node-id": "guest-forged", "answer": 42});
    let (_, status) = run(&w, &mut walk, |_| NodeOutcome::ok(forged.clone()));

    assert_eq!(status, WalkStatus::Completed);
    assert_eq!(
        walk.verdict(),
        Some(&Verdict::Respond {
            payload: forged,
            node_id: "terminal-owned".to_string(),
        })
    );
}

#[test]
fn respond_with_no_caller_attached_is_rejected_and_leaves_the_walk_untouched() {
    let w = wiring("a", vec![node("a", Some(Terminal::Respond))], vec![]);
    let mut walk = started(&w, false);
    let Step::Invoke(call) = w.next(&mut walk, 0) else {
        panic!("expected an invocation");
    };
    let before = walk.clone();

    let error = w
        .apply(&mut walk, &call, says("a"), 0)
        .expect_err("responding to nobody is a wiring contract violation");

    assert_eq!(
        error.kind(),
        &ApplyErrorKind::RespondWithoutCaller("a".to_string())
    );
    assert_eq!(walk, before, "a rejected verdict must not mutate the walk");
}

#[test]
fn a_second_terminal_node_is_rejected() {
    let w = wiring(
        "a",
        vec![
            node("a", Some(Terminal::Respond)),
            node("b", Some(Terminal::Respond)),
        ],
        vec![edge("a", "b")],
    );
    let mut walk = started(&w, true);
    let Step::Invoke(first) = w.next(&mut walk, 0) else {
        panic!("expected an invocation");
    };
    w.apply(&mut walk, &first, says("a"), 0)
        .expect("a responds");
    let Step::Invoke(second) = w.next(&mut walk, 0) else {
        panic!("expected an invocation");
    };

    let error = w
        .apply(&mut walk, &second, says("b"), 0)
        .expect_err("one delivery ends once");

    assert_eq!(
        error.kind(),
        &ApplyErrorKind::SecondVerdict("b".to_string())
    );
    assert_eq!(
        walk.verdict(),
        Some(&Verdict::Respond {
            payload: json!({ "at": "a" }),
            node_id: "a".to_string(),
        }),
        "the first verdict stands"
    );
}

#[test]
fn first_respond_verdict_stands_when_later_work_fails_or_cancels() {
    for cancelled in [false, true] {
        let w = wiring(
            "respond",
            vec![
                node("respond", Some(Terminal::Respond)),
                node("later", None),
            ],
            vec![edge("respond", "later")],
        );
        let mut walk = started(&w, true);
        let (_, status) = run(&w, &mut walk, |node| match node {
            "respond" => says(node),
            _ if cancelled => NodeOutcome::Cancelled,
            _ => NodeOutcome::Error(NodeError::Terminal(ErrorDetail::msg("later failed"))),
        });

        assert_eq!(
            status,
            if cancelled {
                WalkStatus::Cancelled
            } else {
                WalkStatus::Failed
            }
        );
        assert_eq!(
            walk.verdict(),
            Some(&Verdict::Respond {
                payload: json!({"at": "respond"}),
                node_id: "respond".to_string(),
            }),
            "later terminal state must not replace the first response attribution"
        );
    }
}

// ---- emit -----------------------------------------------------------------

#[test]
fn emit_publishes_the_event_under_the_authors_dedup_id() {
    let w = wiring("a", vec![node("a", Some(emit_terminal()))], vec![]);
    let mut walk = started(&w, false);
    let event = json!({ DEDUP_ID_FIELD: "wiring-1:7:a:d1", "order": 42 });
    let (_, status) = run(&w, &mut walk, |_| NodeOutcome::ok(event.clone()));

    assert_eq!(status, WalkStatus::Completed);
    assert_eq!(
        walk.verdict(),
        Some(&Verdict::Emit {
            event,
            dedup_id: "wiring-1:7:a:d1".to_string(),
            entity: "orders".to_string(),
            operation: Op::Insert,
        }),
        "the dedup id travels beside the event the boundary dedups on"
    );
}

/// No dedup id means nothing publishable, which is a node data failure — so it
/// takes the ordinary error edge rather than ending the delivery.
#[test]
fn emit_without_a_dedup_id_routes_to_the_error_edge() {
    let w = wiring(
        "a",
        vec![node("a", Some(emit_terminal())), node("oops", None)],
        vec![edge_on("a", "error", "oops")],
    );
    let mut walk = started(&w, false);
    let (invoked, status) = run(&w, &mut walk, |node| match node {
        "a" => NodeOutcome::ok(json!({ "order": 42 })),
        other => says(other),
    });

    assert_eq!(invoked, ["a", "oops"]);
    assert_eq!(status, WalkStatus::Completed);
    assert_eq!(
        walk.verdict(),
        Some(&Verdict::Discard),
        "nothing was published, so the delivery discarded"
    );
}

#[test]
fn emit_without_a_dedup_id_and_no_error_edge_fails_the_walk() {
    let w = wiring("a", vec![node("a", Some(emit_terminal()))], vec![]);
    let mut walk = started(&w, false);
    // An empty dedup id is as unpublishable as an absent one.
    let (_, status) = run(&w, &mut walk, |_| {
        NodeOutcome::ok(json!({ DEDUP_ID_FIELD: "" }))
    });

    assert_eq!(status, WalkStatus::Failed);
    assert_eq!(
        walk.failure().map(|failure| failure.kind),
        Some(FailureKind::MissingDedupId)
    );
    assert_eq!(walk.verdict(), None, "a failed walk reached no verdict");
}

// ---- discard --------------------------------------------------------------

#[test]
fn an_exhausted_frontier_with_no_caller_discards() {
    let w = wiring(
        "a",
        vec![node("a", None), node("b", None)],
        vec![edge("a", "b")],
    );
    let mut walk = started(&w, false);
    let (_, status) = run(&w, &mut walk, says);

    assert_eq!(status, WalkStatus::Completed);
    assert_eq!(
        walk.verdict(),
        Some(&Verdict::Discard),
        "discard is recorded, not merely the absence of a verdict"
    );
}

/// The mirror of discard: exhausting the frontier while somebody is still
/// waiting is not a quiet completion, it is a caller left hanging.
#[test]
fn an_exhausted_frontier_with_a_caller_still_attached_fails() {
    let w = wiring("a", vec![node("a", None)], vec![]);
    let mut walk = started(&w, true);
    let (_, status) = run(&w, &mut walk, says);

    assert_eq!(status, WalkStatus::Failed);
    assert_eq!(
        walk.failure().map(|failure| failure.kind),
        Some(FailureKind::UnreleasedCaller)
    );
    assert_eq!(walk.verdict(), None);
}

#[test]
fn a_failed_walk_reaches_no_verdict() {
    let w = wiring("a", vec![node("a", Some(Terminal::Respond))], vec![]);
    let mut walk = started(&w, true);
    let (_, status) = run(&w, &mut walk, |_| {
        NodeOutcome::Error(NodeError::Terminal(ErrorDetail::msg("boom")))
    });

    assert_eq!(status, WalkStatus::Failed);
    assert_eq!(
        walk.verdict(),
        None,
        "a failure is not a verdict — what the caller is told is ingress's call"
    );
}

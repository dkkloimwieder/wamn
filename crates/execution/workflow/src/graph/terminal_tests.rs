fn message(text: &str) -> ErrorDetail {
    ErrorDetail {
        message: text.into(),
        ..Default::default()
    }
}

use super::{
    Delivery, ErrorDetail, FailureType, NodeError, NodeOutcome, Step, Verdict, WalkStatus,
};
use serde_json::json;
use wamn_event_wire::Op;

use super::terminal::DEDUP_ID_FIELD;
use super::walk::Walk;
use super::wiring::Wiring;
use wamn_catalog::{WiringEdge, WiringNode, WiringTerminal as Terminal};

fn node(id: &str, terminal: Option<Terminal>) -> (String, WiringNode) {
    (
        id.to_owned(),
        WiringNode {
            interface_version: "0.1.0".into(),
            operation_dependency: None,
            component: "echo".to_string(),
            operation: "echo".to_string(),
            params: Default::default(),
            terminal,
        },
    )
}

fn edge_on(from: &str, port: &str, to: &str) -> WiringEdge {
    WiringEdge {
        from: from.to_string(),
        from_port: port.to_string(),
        to: to.to_string(),
        to_port: Some("input".to_string()),
    }
}

fn edge(from: &str, to: &str) -> WiringEdge {
    edge_on(from, "main", to)
}

fn wiring(entry: &str, nodes: Vec<(String, WiringNode)>, edges: Vec<WiringEdge>) -> Wiring {
    let document = wamn_catalog::WiringDocument::parse(&json!({
        "format-version": "0.1", "wiring-id": "test", "version": 1,
        "entry": entry, "nodes": nodes.into_iter().collect::<std::collections::BTreeMap<_, _>>(), "edges": edges,
    })).expect("catalog fixture admits");
    Wiring::compile(document, &std::collections::BTreeMap::new()).expect("graph indexes")
}

fn started(wiring: &Wiring, caller_attached: bool) -> Walk {
    wiring.start(Delivery {
        payload: json!({}),
        caller_attached,
    })
}

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

fn says(node: &str) -> NodeOutcome {
    NodeOutcome::ok(json!({ "at": node }))
}

fn emit_terminal() -> Terminal {
    Terminal::emit("orders", Op::Insert)
}

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
fn respond_with_no_caller_fails_the_walk() {
    let w = wiring("a", vec![node("a", Some(Terminal::Respond))], vec![]);
    let mut walk = started(&w, false);
    let Step::Invoke(call) = w.next(&mut walk, 0) else {
        panic!("expected an invocation");
    };
    w.apply(&mut walk, &call, says("a"), 0).unwrap();
    assert_eq!(
        walk.failure().unwrap().type_,
        FailureType::RespondWithoutCaller
    );
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

    w.apply(&mut walk, &second, says("b"), 0).unwrap();
    assert_eq!(walk.failure().unwrap().type_, FailureType::SecondVerdict);
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
            _ => NodeOutcome::Error(NodeError::Terminal(message("later failed"))),
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
            node_id: "a".to_string(),
        }),
        "the dedup id travels beside the event the boundary dedups on, under the \
         node that declared the emit"
    );
}

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
    let (_, status) = run(&w, &mut walk, |_| {
        NodeOutcome::ok(json!({ DEDUP_ID_FIELD: "" }))
    });

    assert_eq!(status, WalkStatus::Failed);
    assert_eq!(
        walk.failure().map(|failure| failure.type_),
        Some(FailureType::MissingDedupId)
    );
    assert_eq!(walk.verdict(), None, "a failed walk reached no verdict");
}

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

#[test]
fn an_exhausted_frontier_with_a_caller_still_attached_fails() {
    let w = wiring("a", vec![node("a", None)], vec![]);
    let mut walk = started(&w, true);
    let (_, status) = run(&w, &mut walk, says);

    assert_eq!(status, WalkStatus::Failed);
    assert_eq!(
        walk.failure().map(|failure| failure.type_),
        Some(FailureType::UnreleasedCaller)
    );
    assert_eq!(walk.verdict(), None);
}

#[test]
fn a_failed_walk_reaches_no_verdict() {
    let w = wiring("a", vec![node("a", Some(Terminal::Respond))], vec![]);
    let mut walk = started(&w, true);
    let (_, status) = run(&w, &mut walk, |_| {
        NodeOutcome::Error(NodeError::Terminal(message("boom")))
    });

    assert_eq!(status, WalkStatus::Failed);
    assert_eq!(
        walk.verdict(),
        None,
        "a failure is not a verdict — what the caller is told is ingress's call"
    );
}

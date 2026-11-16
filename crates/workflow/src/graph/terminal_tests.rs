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
use wamn_catalog::WiringEventOperation as Op;

use super::terminal::DEDUP_ID_FIELD;
use super::walk::Walk;
use super::wiring::Wiring;
use wamn_catalog::{WiringEdge, WiringNode, WiringTerminal as Terminal};

fn node(id: &str, terminal: Option<Terminal>) -> (String, WiringNode) {
    (
        id.to_owned(),
        WiringNode {
            interface_version: "0.1.0".parse().unwrap(),
            operation_dependency: None,
            component: "echo".parse().unwrap(),
            operation: "echo".parse().unwrap(),
            params: Default::default(),
            terminal,
        },
    )
}

fn edge_on(from: &str, port: &wamn_catalog::canonical_name::CanonicalName, to: &str) -> WiringEdge {
    WiringEdge {
        from: from.parse().unwrap(),
        from_port: port.clone(),
        to: to.parse().unwrap(),
        to_port: Some("input".parse().unwrap()),
    }
}

fn edge(from: &str, to: &str) -> WiringEdge {
    edge_on(from, &"main".parse().unwrap(), to)
}

fn wiring(entry: &str, nodes: Vec<(String, WiringNode)>, edges: Vec<WiringEdge>) -> Wiring {
    let document = wamn_catalog::WiringDocument::parse(&json!({
        "format-version": "0.1", "wiring-id": "test", "version": 1,
        "entry": entry, "nodes": nodes.into_iter().collect::<std::collections::BTreeMap<_, _>>(), "edges": edges,
    })).expect("catalog fixture admits");
    let components = super::components(&document);
    Wiring::compile(document, &components).expect("graph indexes")
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
    mut outcome_fn: impl FnMut(&wamn_catalog::canonical_name::CanonicalName) -> NodeOutcome,
) -> (Vec<wamn_catalog::canonical_name::CanonicalName>, WalkStatus) {
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

fn says(node: &wamn_catalog::canonical_name::CanonicalName) -> NodeOutcome {
    NodeOutcome::ok(json!({ "at": node }))
}

fn emit_terminal() -> Terminal {
    Terminal::emit("orders".parse().unwrap(), Op::Insert)
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
    {
        let Some(Verdict::Respond { payload, node_id }) = walk.verdict() else {
            panic!("unexpected outcome")
        };
        assert_eq!(payload["at"], serde_json::json!("a"));
        assert_eq!(node_id, "a");
    };
    {
        let observed = &(walk.result());
        assert_eq!(observed["at"], serde_json::json!("audit"));
    };
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
    {
        let Some(Verdict::Respond { payload, node_id }) = walk.verdict() else {
            panic!("unexpected outcome")
        };
        assert_eq!(payload, &forged);
        assert_eq!(node_id, "terminal-owned");
    };
}

#[test]
fn respond_with_no_caller_fails_the_walk() {
    let w = wiring("a", vec![node("a", Some(Terminal::Respond))], vec![]);
    let mut walk = started(&w, false);
    let Step::Invoke(call) = w.next(&mut walk, 0) else {
        panic!("expected an invocation");
    };
    w.apply(&mut walk, &call, says(&"a".parse().unwrap()), 0)
        .unwrap();
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
    w.apply(&mut walk, &first, says(&"a".parse().unwrap()), 0)
        .expect("a responds");
    let Step::Invoke(second) = w.next(&mut walk, 0) else {
        panic!("expected an invocation");
    };

    w.apply(&mut walk, &second, says(&"b".parse().unwrap()), 0)
        .unwrap();
    assert_eq!(walk.failure().unwrap().type_, FailureType::SecondVerdict);
    {
        let Some(Verdict::Respond { payload, node_id }) = walk.verdict() else {
            panic!("unexpected outcome")
        };
        assert_eq!(payload["at"], serde_json::json!("a"));
        assert_eq!(node_id, "a");
    };
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
        let (_, status) = run(&w, &mut walk, |node| match node.as_str() {
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
        {
            let Some(Verdict::Respond { payload, node_id }) = walk.verdict() else {
                panic!("unexpected outcome")
            };
            assert_eq!(payload["at"], serde_json::json!("respond"));
            assert_eq!(node_id, "respond");
        };
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
            entity: "orders".parse().unwrap(),
            operation: Op::Insert,
            node_id: "a".parse().unwrap(),
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
        vec![edge_on("a", &"error".parse().unwrap(), "oops")],
    );
    let mut walk = started(&w, false);
    let (invoked, status) = run(&w, &mut walk, |node| match node.as_str() {
        "a" => NodeOutcome::ok(json!({ "order": 42 })),
        other => says(&other.parse().unwrap()),
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
